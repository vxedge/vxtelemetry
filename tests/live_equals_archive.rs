//! **Live equals archive** (X4.6): what an ingest wrote while a run was live, and what the
//! archive built from that folder reads back, are the same records in the same order.
//!
//! vxedged's live streams (`TelemetryService.StreamRecords`, `LogService.Follow`) send the lines
//! of `records.jsonl` and `logs.jsonl` as they are written; its contract tests prove that half.
//! This proves the other: archive v2 carries exactly those lines. `metrics/samples.jsonl.zst`
//! keeps `records.jsonl`'s order; `logs/<entity>.jsonl.zst` keeps each entity's lines in the
//! order `logs.jsonl` has them.

use std::io::Write;
use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use vxtelemetry::records::Record;
use vxtelemetry::workspace_view::{ScrapeTarget, WorkspaceView};
use vxtelemetry::{archive_v2, ingest, logs};

/// Serve `/metrics` to every scrape, each answer a little different.
async fn exporter() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut n = 0u32;
        while let Ok((mut stream, _)) = listener.accept().await {
            n += 1;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let body = format!(
                "vxtest_rtt_seconds{{quantile=\"0.99\"}} 0.{n}\n\
                 vxedge_node_memory_bytes{{entity=\"car/_/hpc1\",source=\"process\"}} {n}000\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    addr
}

fn lines(path: &Path) -> Vec<Record> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

#[tokio::test]
async fn an_archive_reads_back_what_the_ingest_wrote() {
    let root = std::env::temp_dir().join(format!("vxtel-live-archive-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (ws, ingest_dir, rt) = (root.join("ws"), root.join("ingest"), root.join("rt"));
    for d in [&ws, &ingest_dir, &rt] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(ws.join("main.yaml"), "kind: experiment\nid: e46\n").unwrap();

    let ok = exporter().await;
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().to_string()
    };
    let (serial, svc) = (rt.join("vx-hpc1.serial.log"), rt.join("svc-telemetry.log"));
    std::fs::write(
        ingest_dir.join("entities.json"),
        serde_json::json!([
            {"path": "host", "kind": "host", "scrape_targets": [ok],
             "logs": [{"stream": "stdout", "path": svc}]},
            {"path": "car/_/hpc1", "kind": "node", "scrape_targets": [dead],
             "logs": [{"stream": "serial", "path": serial}]},
        ])
        .to_string(),
    )
    .unwrap();
    let targets = [
        ScrapeTarget {
            address: ok,
            optional: false,
        },
        ScrapeTarget {
            address: dead,
            optional: false,
        },
    ];

    // Three rounds, the log files growing between them, one line split across two rounds.
    let mut stats = ingest::IngestStats::default();
    append(&serial, "Linux version 6.12\nhpc1 login: ");
    append(&svc, "ingest starting\n");
    ingest::scrape_round(&targets, &ingest_dir, &mut stats)
        .await
        .unwrap();
    append(&serial, "\n");
    append(&svc, "round 2\nround 2b\n");
    ingest::scrape_round(&targets, &ingest_dir, &mut stats)
        .await
        .unwrap();
    append(&serial, "root@hpc1:~# uptime\n");
    ingest::scrape_round(&targets, &ingest_dir, &mut stats)
        .await
        .unwrap();
    assert_eq!(stats.rounds, 3);
    assert_eq!(stats.log_lines_total, 6);

    let live_records = lines(&ingest_dir.join("records.jsonl"));
    let live_logs = lines(&ingest_dir.join(logs::FILE));
    assert!(
        live_records
            .iter()
            .any(|r| matches!(r, Record::Gap { entity, .. } if entity == "car/_/hpc1")),
        "the dead target leaves gaps: {live_records:?}"
    );

    let view = WorkspaceView::load(&ws).unwrap();
    let out = root.join("run.zip");
    archive_v2::collect(&view, &ingest_dir, &out, None).unwrap();
    assert!(archive_v2::verify(&out).unwrap().ok);

    assert_eq!(
        archive_v2::records(&out).unwrap(),
        live_records,
        "samples and gaps, in the order written"
    );
    // Logs: each entity's lines in the order written, entities in member order.
    let mut by_member = live_logs.clone();
    by_member.sort_by_key(|r| match r {
        Record::Log { entity, .. } => logs::member(entity),
        other => panic!("{other:?}"),
    });
    assert_eq!(archive_v2::logs(&out).unwrap(), by_member);
    let serial_lines: Vec<&str> = live_logs
        .iter()
        .filter_map(|r| match r {
            Record::Log { stream, line, .. } if stream == "serial" => Some(line.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        serial_lines,
        ["Linux version 6.12", "hpc1 login: ", "root@hpc1:~# uptime"],
        "a line split across rounds arrives once, whole"
    );
    std::fs::remove_dir_all(&root).ok();
}
