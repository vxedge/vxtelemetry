//! End-to-end: fake agent /metrics server → ingest rounds → collect archive →
//! reopen the zip and verify contents. Unprivileged; runs in plain CI.

use std::io::Read;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use vxtelemetry::ingest::{IngestStats, scrape_round};
use vxtelemetry::{archive, manifest_view::ManifestView};

/// Minimal fake agent: answers every connection with a fixed exposition.
async fn fake_agent() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await;
                let body = "vxprobe_sent_total 42\nvxprobe_rtt_seconds{quantile=\"0.99\"} 0.0001\n";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_then_collect_roundtrip() {
    let agent = fake_agent().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let manifest_yaml = format!(
        "experiment: {{ id: exp-e2e, name: e2e }}\ntelemetry:\n  prometheus:\n    scrape_targets: [\"{agent}\"]\n"
    );
    let view = ManifestView::from_yaml(&manifest_yaml).unwrap();
    let targets = view.scrape_targets();

    let tmp = std::env::temp_dir().join(format!("vxtel-e2e-{}", std::process::id()));
    let ingest_dir = tmp.join("ingest");
    tokio::fs::create_dir_all(&ingest_dir).await.unwrap();

    // Two scrape rounds.
    let mut stats = IngestStats::default();
    scrape_round(&targets, &ingest_dir, &mut stats)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    scrape_round(&targets, &ingest_dir, &mut stats)
        .await
        .unwrap();
    assert_eq!(stats.scrapes_ok, 2);
    assert_eq!(stats.scrapes_failed, 0);
    assert_eq!(stats.samples_total, 4, "two samples per round");
    assert_eq!(stats.completeness_pct(), 100.0);
    std::fs::write(
        ingest_dir.join("ingest-stats.json"),
        serde_json::to_vec(&stats).unwrap(),
    )
    .unwrap();

    // Collect and verify the archive.
    let out = tmp.join("exp-e2e.zip");
    let metadata = archive::collect(&manifest_yaml, "exp-e2e", &ingest_dir, &out).unwrap();
    assert_eq!(metadata.experiment_id, "exp-e2e");
    assert_eq!(metadata.manifest_sha256.len(), 64);

    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(names.contains(&"metadata.json".to_string()));
    assert!(names.contains(&"manifest.yaml".to_string()));
    assert!(names.contains(&"ingest/samples.jsonl".to_string()));
    assert!(
        names.iter().filter(|n| n.ends_with(".prom")).count() == 2,
        "two raw scrapes archived, got: {names:?}"
    );

    // Samples are structured and parseable.
    let mut samples = String::new();
    zip.by_name("ingest/samples.jsonl")
        .unwrap()
        .read_to_string(&mut samples)
        .unwrap();
    assert_eq!(samples.lines().count(), 4);
    assert!(samples.contains("vxprobe_rtt_seconds"));

    let _ = std::fs::remove_dir_all(&tmp);
}
