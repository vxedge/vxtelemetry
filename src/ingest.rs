//! Ingestion loop: scrape every manifest target on an interval, persist raw
//! expositions and structured samples under the ingest dir, track
//! completeness (ingestion rate and completeness are themselves reported
//! metrics — §12).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::{TelemetryError, promtext, scrape};

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct IngestStats {
    pub rounds: u64,
    pub scrapes_ok: u64,
    pub scrapes_failed: u64,
    pub samples_total: u64,
}

impl IngestStats {
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
    targets: &[String],
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
        match scrape::fetch_metrics(target).await {
            Ok(body) => {
                stats.scrapes_ok += 1;
                let raw_dir = ingest_dir.join("raw").join(sanitize(target));
                tokio::fs::create_dir_all(&raw_dir).await?;
                tokio::fs::write(raw_dir.join(format!("{ts}.prom")), &body).await?;
                let mut lines = String::new();
                for s in promtext::parse(&body) {
                    stats.samples_total += 1;
                    let record = SampleRecord {
                        ts_unix_ms: ts,
                        target,
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
                stats.scrapes_failed += 1;
                tracing::warn!(%target, error = %e, "scrape failed");
            }
        }
    }
    stats.rounds += 1;
    Ok(())
}

/// Scrape all targets every `interval` for `duration`; returns final stats
/// (also written to `<ingest_dir>/ingest-stats.json` for the archive).
pub async fn run_ingest(
    targets: &[String],
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
