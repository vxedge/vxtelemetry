//! Ingestion loop: scrape every manifest target on an interval, persist raw
//! expositions and structured samples under the ingest dir, track
//! completeness (ingestion rate and completeness are themselves reported
//! metrics — §12).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::manifest_view::ScrapeTarget;
use crate::{TelemetryError, promtext, scrape};

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct IngestStats {
    pub rounds: u64,
    pub scrapes_ok: u64,
    pub scrapes_failed: u64,
    pub samples_total: u64,
    /// Scrapes against `optional_scrape_targets` (§6) — tracked separately
    /// so a target that's legitimately absent under some experiment arms
    /// never drags down `completeness_pct()`.
    #[serde(default)]
    pub optional_scrapes_ok: u64,
    #[serde(default)]
    pub optional_scrapes_failed: u64,
}

impl IngestStats {
    /// Completeness over *required* targets only — see `optional_scrapes_*`
    /// for the (uncounted) optional-target attempts.
    pub fn completeness_pct(&self) -> f64 {
        let attempts = self.scrapes_ok + self.scrapes_failed;
        if attempts == 0 {
            return 100.0;
        }
        self.scrapes_ok as f64 * 100.0 / attempts as f64
    }
}

fn sanitize(target: &str) -> String {
    target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[derive(Serialize)]
struct SampleRecord<'a> {
    ts_unix_ms: i64,
    target: &'a str,
    metric: &'a str,
    labels: &'a str,
    value: f64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}

/// One scrape round across all targets. Failures are counted, not fatal.
pub async fn scrape_round(
    targets: &[ScrapeTarget],
    ingest_dir: &Path,
    stats: &mut IngestStats,
) -> Result<(), TelemetryError> {
    let ts = now_ms();
    let samples_path = ingest_dir.join("samples.jsonl");
    let mut samples_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&samples_path)
        .await?;

    for target in targets {
        match scrape::fetch_metrics(&target.address).await {
            Ok(body) => {
                if target.optional {
                    stats.optional_scrapes_ok += 1;
                } else {
                    stats.scrapes_ok += 1;
                }
                let raw_dir = ingest_dir.join("raw").join(sanitize(&target.address));
                tokio::fs::create_dir_all(&raw_dir).await?;
                tokio::fs::write(raw_dir.join(format!("{ts}.prom")), &body).await?;
                let mut lines = String::new();
                for s in promtext::parse(&body) {
                    stats.samples_total += 1;
                    let record = SampleRecord {
                        ts_unix_ms: ts,
                        target: &target.address,
                        metric: &s.metric,
                        labels: &s.labels,
                        value: s.value,
                    };
                    lines.push_str(&serde_json::to_string(&record).expect("serializable record"));
                    lines.push('\n');
                }
                samples_file.write_all(lines.as_bytes()).await?;
            }
            Err(e) => {
                if target.optional {
                    stats.optional_scrapes_failed += 1;
                } else {
                    stats.scrapes_failed += 1;
                }
                tracing::warn!(target = %target.address, error = %e, optional = target.optional, "scrape failed");
            }
        }
    }
    stats.rounds += 1;
    Ok(())
}

/// Scrape all targets every `interval` for `duration`; returns final stats
/// (also written to `<ingest_dir>/ingest-stats.json` for the archive).
pub async fn run_ingest(
    targets: &[ScrapeTarget],
    ingest_dir: PathBuf,
    interval: Duration,
    duration: Duration,
) -> Result<IngestStats, TelemetryError> {
    tokio::fs::create_dir_all(&ingest_dir).await?;
    let mut stats = IngestStats::default();
    // Inclusive range so the last tick lands at rounds*interval == duration
    // (tokio::time::interval's first tick fires immediately) — matches
    // sample.rs's loop so ingest and sample cover the same window instead of
    // ingest finishing early and silently missing scrapes near the end.
    let rounds = (duration.as_secs() / interval.as_secs().max(1)).max(1);
    let mut ticker = tokio::time::interval(interval);
    for _ in 0..=rounds {
        ticker.tick().await;
        scrape_round(targets, &ingest_dir, &mut stats).await?;
    }
    tokio::fs::write(
        ingest_dir.join("ingest-stats.json"),
        serde_json::to_vec_pretty(&stats).expect("serializable stats"),
    )
    .await?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(listener: TcpListener, body: &'static str) {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    }

    /// Ephemeral port grabbed then released — guaranteed nothing listens
    /// there (same pattern vxprobe's loopback test uses).
    async fn dead_addr() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().to_string()
    }

    async fn tmp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vxtel-ingest-test-{label}-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    #[tokio::test]
    async fn optional_target_failure_does_not_affect_completeness() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ok_addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(serve_once(listener, "vxtest_metric 1\n"));

        let targets = vec![
            ScrapeTarget {
                address: ok_addr,
                optional: false,
            },
            ScrapeTarget {
                address: dead_addr().await,
                optional: true,
            },
        ];
        let dir = tmp_dir("optional").await;
        let mut stats = IngestStats::default();
        scrape_round(&targets, &dir, &mut stats).await.unwrap();

        assert_eq!(stats.scrapes_ok, 1);
        assert_eq!(stats.scrapes_failed, 0);
        assert_eq!(stats.optional_scrapes_ok, 0);
        assert_eq!(stats.optional_scrapes_failed, 1);
        assert_eq!(
            stats.completeness_pct(),
            100.0,
            "optional failure must not count"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn required_target_failure_reduces_completeness() {
        let targets = vec![ScrapeTarget {
            address: dead_addr().await,
            optional: false,
        }];
        let dir = tmp_dir("required").await;
        let mut stats = IngestStats::default();
        scrape_round(&targets, &dir, &mut stats).await.unwrap();

        assert_eq!(stats.scrapes_failed, 1);
        assert_eq!(stats.completeness_pct(), 0.0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
