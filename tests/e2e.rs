//! End-to-end: fake agent /metrics server → ingest rounds → collect archive →
//! reopen the zip and verify contents. Unprivileged; runs in plain CI.

use std::io::Read;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use vxtelemetry::ingest::{IngestStats, scrape_round};
use vxtelemetry::records::Record;
use vxtelemetry::{archive, workspace_view::WorkspaceView};

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

    let tmp = std::env::temp_dir().join(format!("vxtel-e2e-{}", std::process::id()));
    let ingest_dir = tmp.join("ingest");
    tokio::fs::create_dir_all(&ingest_dir).await.unwrap();

    // A §6.0 workspace, the only shape this tool reads: several files, one of which declares
    // `kind: experiment`.
    let ws_dir = tmp.join("exp-e2e-ws");
    tokio::fs::create_dir_all(&ws_dir).await.unwrap();
    tokio::fs::write(
        ws_dir.join("main.yaml"),
        format!(
            "kind: experiment\nid: exp-e2e\nvehicles: [veh]\ntelemetry:\n  prometheus:\n    scrape_targets: [\"{agent}\"]\n"
        ),
    )
    .await
    .unwrap();
    tokio::fs::write(
        ws_dir.join("vehicle.yaml"),
        "kind: vehicle\nid: veh\ndevices: {}\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        ws_dir.join("vxp.yaml"),
        "schema: vxp/1\nid: vxedge/exp-e2e\nversion: 0.1.0\nentry: main.yaml\n",
    )
    .await
    .unwrap();

    let view = WorkspaceView::load(&ws_dir).unwrap();
    let targets = view.scrape_targets();

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

    // Collect and verify the archive (no --record-image: image_digests must
    // be absent from metadata.json, not null — the pre-existing shape).
    let out = tmp.join("exp-e2e.zip");
    let metadata = archive::collect(&view, &ingest_dir, &out, None).unwrap();
    assert_eq!(metadata.archive, archive::SCHEMA);
    assert_eq!(metadata.experiment_id, "exp-e2e");
    assert_eq!(metadata.workspace_sha256.len(), 64);
    assert_eq!(metadata.workspace_identity_algorithm, "vxws/2");
    assert_eq!(metadata.package.as_ref().unwrap().id, "vxedge/exp-e2e");
    assert!(metadata.package.as_ref().unwrap().digest.is_some());
    assert!(metadata.image_digests.is_none());
    assert!(metadata.instance.is_none(), "no slot passed, none recorded");

    // The daemon passes the run's slot; the archive records it.
    let mut slotted = WorkspaceView::load(&ws_dir).unwrap();
    slotted.instance = Some("1".into());
    let out_slot = tmp.join("slot.zip");
    let m = archive::collect(&slotted, &ingest_dir, &out_slot, None).unwrap();
    assert_eq!(m.instance.as_deref(), Some("1"));

    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().to_string())
        .collect();
    assert!(names.contains(&"index.json".to_string()));
    assert!(names.contains(&"metadata.json".to_string()));
    assert!(names.contains(&"metrics/samples.jsonl.zst".to_string()));
    // The archive carries the workspace itself — every file, so it is re-runnable rather than
    // merely described.
    assert!(names.contains(&"package/main.yaml".to_string()));
    assert!(names.contains(&"package/vehicle.yaml".to_string()));
    assert!(names.contains(&"package/vxp.yaml".to_string()));
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("ingest/") || n.ends_with(".prom")),
        "no raw scrapes or ingest folder, got: {names:?}"
    );

    // Samples are records, named by entity, and the archive checks out.
    let samples: Vec<Record> = archive::records(&out).unwrap();
    assert_eq!(samples.len(), 4);
    assert!(
        samples
            .iter()
            .any(|r| matches!(r, Record::Sample { metric, .. } if metric == "vxprobe_rtt_seconds")),
        "{samples:?}"
    );
    assert!(archive::verify(&out).unwrap().ok);
    // No --record-image: metadata.json in the archive itself carries no
    // image_digests key at all.
    let mut meta_json = String::new();
    zip.by_name("metadata.json")
        .unwrap()
        .read_to_string(&mut meta_json)
        .unwrap();
    assert!(!meta_json.contains("image_digests"));
    drop(zip);

    // With digests supplied, they land in the archived metadata.json.
    let out2 = tmp.join("exp-e2e-digests.zip");
    let mut digests = std::collections::BTreeMap::new();
    digests.insert("vx5g-core".to_string(), "sha256:feedface".to_string());
    let metadata2 = archive::collect(&view, &ingest_dir, &out2, Some(digests)).unwrap();
    assert_eq!(
        metadata2.image_digests.as_ref().unwrap()["vx5g-core"],
        "sha256:feedface"
    );
    let mut zip2 = zip::ZipArchive::new(std::fs::File::open(&out2).unwrap()).unwrap();
    let mut meta_json2 = String::new();
    zip2.by_name("metadata.json")
        .unwrap()
        .read_to_string(&mut meta_json2)
        .unwrap();
    assert!(meta_json2.contains("\"vx5g-core\": \"sha256:feedface\""));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// A target with a path is scraped at `<path>/metrics`: the daemon serves each run's exporter at
/// `/runs/<slot>/metrics`.
#[tokio::test(flavor = "multi_thread")]
async fn a_target_with_a_path_is_scraped_under_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.starts_with("GET /runs/1/metrics ") {
                    let body = "vxedge_run_up 1\n";
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    let body = vxtelemetry::scrape::fetch_metrics(&format!("{addr}/runs/1"))
        .await
        .unwrap();
    assert_eq!(body, "vxedge_run_up 1\n");
    assert!(
        vxtelemetry::scrape::fetch_metrics(&addr.to_string())
            .await
            .is_err(),
        "plain /metrics is not this run's"
    );
}
