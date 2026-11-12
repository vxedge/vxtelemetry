//! `vxtelemetry verify`'s exit codes: 0 when every check passes, 1 when one fails, 3 when nothing
//! failed but the archive's identity is not checkable.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use sha2::Digest;
use vxtelemetry::archive_v2;
use vxtelemetry::workspace_view::WorkspaceView;

fn members(archive: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i).unwrap();
        let name = e.name().unwrap().to_string();
        let mut bytes = Vec::new();
        e.read_to_end(&mut bytes).unwrap();
        out.insert(name, bytes);
    }
    out
}

/// Write `members` with a fresh `index.json`, as an archive whose index passes.
fn write_indexed(members: &BTreeMap<String, Vec<u8>>, out: &Path) {
    let files: Vec<serde_json::Value> = members
        .iter()
        .filter(|(p, _)| p.as_str() != "index.json")
        .map(|(p, b)| {
            let sha: String = sha2::Sha256::digest(b)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect();
            serde_json::json!({ "path": p, "size": b.len(), "sha256": format!("sha256:{sha}") })
        })
        .collect();
    let index = serde_json::json!({ "schema": archive_v2::SCHEMA, "files": files });
    let mut zip = zip::ZipWriter::new(std::fs::File::create(out).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("index.json", opts).unwrap();
    zip.write_all(&serde_json::to_vec(&index).unwrap()).unwrap();
    for (p, b) in members.iter().filter(|(p, _)| p.as_str() != "index.json") {
        zip.start_file(p.as_str(), opts).unwrap();
        zip.write_all(b).unwrap();
    }
    zip.finish().unwrap();
}

fn verify(archive: &Path) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_vxtelemetry"))
        .arg("verify")
        .arg(archive)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        serde_json::from_slice(&out.stdout).unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn verify_exit_codes_tell_checked_failed_and_not_checkable_apart() {
    let root = std::env::temp_dir().join(format!("vxtel-verify-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let ingest = root.join("ingest");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&ingest).unwrap();
    std::fs::write(ws.join("main.yaml"), "kind: experiment\nid: e2\n").unwrap();
    let view = WorkspaceView::load(&ws).unwrap();
    let good = root.join("good.zip");
    archive_v2::collect(&view, &ingest, &good, None).unwrap();

    let (code, json, _) = verify(&good);
    assert_eq!((code, &json["identity"]["outcome"]), (0, &"checked".into()));

    // Written before identity algorithms were recorded: the index passes, the identity is not
    // checkable, and the exit code says so.
    let mut old = members(&good);
    let mut meta: serde_json::Value = serde_json::from_slice(&old["metadata.json"]).unwrap();
    meta.as_object_mut()
        .unwrap()
        .remove("workspace_identity_algorithm");
    old.insert("metadata.json".into(), serde_json::to_vec(&meta).unwrap());
    let old_zip = root.join("old.zip");
    write_indexed(&old, &old_zip);
    let (code, json, stderr) = verify(&old_zip);
    assert_eq!(code, 3, "{json}");
    assert_eq!(json["index"], true);
    assert_eq!(json["identity"]["outcome"], "not_checkable");
    assert_eq!(json["identity"]["reason"], archive_v2::NO_ALGORITHM);
    assert!(
        stderr.contains("index OK; identity not checkable"),
        "{stderr}"
    );

    // The same archive with a member changed and the index left alone fails.
    old.insert("entities.json".into(), b"[]".to_vec());
    let forged = root.join("forged.zip");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&forged).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (p, b) in &old {
            zip.start_file(p.as_str(), opts).unwrap();
            zip.write_all(b).unwrap();
        }
        zip.finish().unwrap();
    }
    let (code, json, _) = verify(&forged);
    assert_eq!((code, &json["index"]), (1, &false.into()));

    std::fs::remove_dir_all(&root).ok();
}
