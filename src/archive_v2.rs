//! **Archive v2** (`ARCHITECTURE.md` §12, decisions X-7 and X-15): one zip per run, whose large
//! record files are zstd-compressed members, readable by any zip reader and by Python's standard
//! library from 3.14 (`zipfile` and `compression.zstd`).
//!
//! ```text
//! index.json                  schema vxarchive/2; every other member with its size and sha256
//! metadata.json               identity, package, kernel, ingest stats (as v1, plus `archive`)
//! package/                    the workspace's identity inputs, so re-hashing gives the identity
//! entities.json               the entity paths records name, as vxedged mapped them
//! metrics/samples.jsonl.zst   `sample` and `gap` records
//! phases.jsonl                `phase` records, when there are any
//! logs/<entity>.jsonl.zst     `log` records, one member per entity that logged (decision X-18)
//! events.jsonl                vxAPI events, when the ingest folder holds them
//! ```
//!
//! v1 archives stay readable: [`records`] turns a v1 `ingest/samples.jsonl` into the same
//! `sample` records, named by `host` unless the archive carries `entities.json`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::TelemetryError;
use crate::archive::{ArchiveMetadata, DigestCheck, IdentityCheck, IdentityOutcome, Verification};
use crate::records::{Entities, Record};
use crate::workspace_view::WorkspaceView;

/// The schema `index.json` names.
pub const SCHEMA: &str = "vxarchive/2";
const SAMPLES: &str = "metrics/samples.jsonl.zst";
const PHASES: &str = "phases.jsonl";
const EVENTS: &str = "events.jsonl";
const ENTITIES: &str = "entities.json";
const PACKAGE: &str = "package/";

/// One member, as `index.json` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub schema: String,
    pub files: Vec<IndexEntry>,
}

fn hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn arch(e: impl std::fmt::Display) -> TelemetryError {
    TelemetryError::Archive(e.to_string())
}

/// Write an archive v2 of `workspace` and what was ingested under `ingest_dir` to `out`.
pub fn collect(
    workspace: &WorkspaceView,
    ingest_dir: &Path,
    out: &Path,
    image_digests: Option<BTreeMap<String, String>>,
) -> Result<ArchiveMetadata, TelemetryError> {
    let mut metadata = crate::archive::metadata_for(workspace, ingest_dir, image_digests);
    metadata.archive = Some(SCHEMA.to_string());

    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    members.push((
        "metadata.json".into(),
        serde_json::to_vec_pretty(&metadata).expect("serializable metadata"),
    ));
    if let Ok(bytes) = std::fs::read(ingest_dir.join(ENTITIES)) {
        members.push((ENTITIES.into(), bytes));
    }
    // Records: samples and gaps compressed; phases as plain lines, as readers seek them.
    let (mut samples, mut phases) = (Vec::new(), Vec::new());
    if let Ok(text) = std::fs::read_to_string(ingest_dir.join("records.jsonl")) {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let target = if line.contains(r#""record":"phase""#) {
                &mut phases
            } else {
                &mut samples
            };
            target.extend_from_slice(line.as_bytes());
            target.push(b'\n');
        }
    }
    members.push((
        SAMPLES.into(),
        zstd::encode_all(samples.as_slice(), 9).map_err(arch)?,
    ));
    if !phases.is_empty() {
        members.push((PHASES.into(), phases));
    }
    // Logs: one compressed member per entity, each in the order the ingest read its lines.
    let mut logs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(ingest_dir.join(crate::logs::FILE)) {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let member = crate::logs::member(v["entity"].as_str().unwrap_or(crate::records::HOST));
            let body = logs.entry(member).or_default();
            body.extend_from_slice(line.as_bytes());
            body.push(b'\n');
        }
    }
    for (member, body) in logs {
        members.push((member, zstd::encode_all(body.as_slice(), 9).map_err(arch)?));
    }
    if let Ok(bytes) = std::fs::read(ingest_dir.join(EVENTS)) {
        members.push((EVENTS.into(), bytes));
    }
    for (name, bytes) in &workspace.inputs {
        members.push((format!("{PACKAGE}{name}"), bytes.clone()));
    }

    let index = Index {
        schema: SCHEMA.to_string(),
        files: members
            .iter()
            .map(|(path, bytes)| IndexEntry {
                path: path.clone(),
                size: bytes.len() as u64,
                sha256: format!("sha256:{}", hex(bytes)),
            })
            .collect(),
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut zip = zip::ZipWriter::new(std::fs::File::create(out)?);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default();
    zip.start_file("index.json", deflated).map_err(arch)?;
    zip.write_all(&serde_json::to_vec_pretty(&index).expect("serializable index"))?;
    for (path, bytes) in &members {
        let opts = if path.ends_with(".zst") {
            stored
        } else {
            deflated
        };
        zip.start_file(path.as_str(), opts).map_err(arch)?;
        zip.write_all(bytes)?;
    }
    zip.finish().map_err(arch)?;
    Ok(metadata)
}

fn members_of(archive: &Path) -> Result<BTreeMap<String, Vec<u8>>, TelemetryError> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?).map_err(arch)?;
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(arch)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(arch)?.to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        out.insert(name, bytes);
    }
    Ok(out)
}

/// Whether `archive` is an archive v2: it holds an `index.json` naming [`SCHEMA`].
pub fn is_v2(archive: &Path) -> bool {
    members_of(archive)
        .ok()
        .and_then(|m| m.get("index.json").cloned())
        .and_then(|b| serde_json::from_slice::<Index>(&b).ok())
        .is_some_and(|i| i.schema == SCHEMA)
}

/// Why an archive's identity is not checkable: it records an identity but no algorithm. Archives
/// were written this way when they carried only the workspace documents, so `package/` cannot
/// reproduce the identity, and the algorithm is never assumed.
pub const NO_ALGORITHM: &str =
    "written before identity algorithms were recorded; package/ holds only the documents";
/// Why an archive's identity is not checkable: it records `manifest_sha256` and no
/// `workspace_sha256`, from before workspace identities existed.
pub const MANIFEST_ONLY: &str = "written before workspace identities; records a manifest digest";
/// Why an archive's identity is not checkable: `metadata.json` records no identity at all.
pub const NO_IDENTITY: &str = "metadata.json records no workspace identity";

/// Verify an archive v2. The index check is the integrity result: every member matches
/// `index.json`, and nothing is added or missing. The identity is then checked, mismatched, or
/// not checkable with the reason: `package/` is re-hashed only under the algorithm the archive
/// records, never an assumed one. The package digest is checked when the archive records one.
pub fn verify(archive: &Path) -> Result<Verification, TelemetryError> {
    let members = members_of(archive)?;
    let bad = |m: String| TelemetryError::Archive(format!("`{}`: {m}", archive.display()));
    let index: Index = serde_json::from_slice(
        members
            .get("index.json")
            .ok_or_else(|| bad("no index.json".into()))?,
    )
    .map_err(|e| bad(format!("index.json: {e}")))?;
    let listed: BTreeMap<&str, &IndexEntry> =
        index.files.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut index_ok = members.len() == listed.len() + 1;
    for (path, bytes) in &members {
        if path == "index.json" {
            continue;
        }
        index_ok &= listed.get(path.as_str()).is_some_and(|e| {
            e.size == bytes.len() as u64 && e.sha256 == format!("sha256:{}", hex(bytes))
        });
    }

    let metadata: serde_json::Value = serde_json::from_slice(
        members
            .get("metadata.json")
            .ok_or_else(|| bad("no metadata.json".into()))?,
    )
    .map_err(|e| bad(format!("metadata.json: {e}")))?;
    let recorded = metadata["workspace_sha256"].as_str().map(str::to_string);
    let algorithm = metadata["workspace_identity_algorithm"]
        .as_str()
        .map(str::to_string);
    let inputs: Vec<(String, Vec<u8>)> = members
        .iter()
        .filter_map(|(p, b)| p.strip_prefix(PACKAGE).map(|r| (r.to_string(), b.clone())))
        .collect();
    let (identity, computed) = match (&recorded, &algorithm) {
        (Some(recorded), Some(algorithm)) => {
            let computed = crate::identity::digest(algorithm, &inputs)?;
            let outcome = if &computed == recorded {
                IdentityOutcome::Checked
            } else {
                IdentityOutcome::Mismatch
            };
            (
                IdentityCheck {
                    outcome,
                    reason: None,
                },
                Some(computed),
            )
        }
        (Some(_), None) => (not_checkable(NO_ALGORITHM), None),
        (None, _) if metadata["manifest_sha256"].is_string() => {
            (not_checkable(MANIFEST_ONLY), None)
        }
        (None, _) => (not_checkable(NO_IDENTITY), None),
    };
    let package_digest = match metadata["package"]["digest"].as_str() {
        Some(recorded) => Some(DigestCheck {
            recorded: recorded.to_string(),
            computed: crate::package::digest(&inputs)?,
        }),
        None => None,
    };
    let package_ok = package_digest
        .as_ref()
        .is_none_or(|d| d.recorded == d.computed);
    Ok(Verification {
        ok: index_ok && identity.outcome == IdentityOutcome::Checked && package_ok,
        index: Some(index_ok),
        identity,
        algorithm,
        recorded,
        computed,
        files: inputs.len(),
        package_digest,
    })
}

fn not_checkable(reason: &str) -> IdentityCheck {
    IdentityCheck {
        outcome: IdentityOutcome::NotCheckable,
        reason: Some(reason.to_string()),
    }
}

/// Every record an archive holds, v1 or v2, in file order: samples and gaps, then phases.
pub fn records(archive: &Path) -> Result<Vec<Record>, TelemetryError> {
    let members = members_of(archive)?;
    let mut out = Vec::new();
    let mut push_lines = |text: &str| -> Result<(), TelemetryError> {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).map_err(arch)?);
        }
        Ok(())
    };
    if let Some(compressed) = members.get(SAMPLES) {
        let bytes = zstd::decode_all(compressed.as_slice()).map_err(arch)?;
        push_lines(&String::from_utf8_lossy(&bytes))?;
        if let Some(phases) = members.get(PHASES) {
            push_lines(&String::from_utf8_lossy(phases))?;
        }
        return Ok(out);
    }
    // v1: the ingest folder's samples, in their older shape.
    let entities = {
        let dir = std::env::temp_dir().join(format!("vxtel-v1-{}", std::process::id()));
        match members.get("ingest/entities.json") {
            Some(bytes) => {
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join(ENTITIES), bytes)?;
                let e = Entities::load(&dir);
                let _ = std::fs::remove_dir_all(&dir);
                e
            }
            None => Entities::default(),
        }
    };
    if let Some(bytes) = members.get("ingest/samples.jsonl") {
        for line in String::from_utf8_lossy(bytes)
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let v: serde_json::Value = serde_json::from_str(line).map_err(arch)?;
            out.push(Record::Sample {
                ts_ns: v["ts_unix_ms"].as_i64().unwrap_or(0) * 1_000_000,
                entity: entities.of(v["target"].as_str().unwrap_or("")).to_string(),
                metric: v["metric"].as_str().unwrap_or("").to_string(),
                labels: crate::records::parse_labels(v["labels"].as_str().unwrap_or("")),
                value: v["value"].as_f64().unwrap_or(f64::NAN),
            });
        }
    }
    Ok(out)
}

/// Every `log` record a v2 archive holds, entity by entity in member order, each entity's lines
/// in the order they were read. A v1 archive, or a run that logged nothing, has none.
pub fn logs(archive: &Path) -> Result<Vec<Record>, TelemetryError> {
    let mut out = Vec::new();
    for (name, compressed) in members_of(archive)? {
        if !(name.starts_with("logs/") && name.ends_with(".jsonl.zst")) {
            continue;
        }
        let bytes = zstd::decode_all(compressed.as_slice()).map_err(arch)?;
        for line in String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            out.push(serde_json::from_str(line).map_err(arch)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (std::path::PathBuf, WorkspaceView, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!("vxtel-v2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let ws = tmp.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("main.yaml"), "kind: experiment\nid: e2\n").unwrap();
        std::fs::write(
            ws.join("vxp.yaml"),
            "schema: vxp/1\nid: vxedge/e2\nversion: 1.0.0\nentry: main.yaml\n",
        )
        .unwrap();
        let ingest = tmp.join("ingest");
        std::fs::create_dir_all(&ingest).unwrap();
        std::fs::write(
            ingest.join("entities.json"),
            r#"[{"path":"host","kind":"host"},{"path":"car/front/ecu1","kind":"node","scrape_targets":["10.1.0.2:9801"]}]"#,
        )
        .unwrap();
        std::fs::write(
            ingest.join("records.jsonl"),
            concat!(
                r#"{"record":"sample","ts_ns":10,"entity":"car/front/ecu1","metric":"m","value":1.0}"#,
                "\n",
                r#"{"record":"gap","entity":"host","from_ns":11,"to_ns":12,"reason":"scrape failed"}"#,
                "\n",
                r#"{"record":"phase","ts_ns":9,"phase":"measure"}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            ingest.join(crate::logs::FILE),
            concat!(
                r#"{"record":"log","ts_ns":10,"entity":"car/front/ecu1","stream":"serial","line":"boot"}"#,
                "\n",
                r#"{"record":"log","ts_ns":10,"entity":"host/services/traffic","stream":"stdout","line":"go"}"#,
                "\n",
                r#"{"record":"log","ts_ns":20,"entity":"car/front/ecu1","stream":"serial","line":"ready"}"#,
                "\n"
            ),
        )
        .unwrap();
        // The v1 shape too, which a v2 archive leaves out.
        std::fs::write(
            ingest.join("samples.jsonl"),
            r#"{"ts_unix_ms":1,"target":"10.1.0.2:9801","metric":"m","labels":"{q=\"1\"}","value":2.0}"#,
        )
        .unwrap();
        let view = WorkspaceView::load(&ws).unwrap();
        (tmp, view, ingest)
    }

    #[test]
    fn a_campaign_trial_records_its_identity_and_late_phases() {
        let (tmp, view, ingest) = fixture("trial");
        std::fs::write(
            ingest.join(crate::archive::TRIAL_FILE),
            r#"{"campaign_id":"c1","trial_id":"r1-idle","arm":"idle","repetition":1,"parameters":{"load_mbps":0}}"#,
        )
        .unwrap();
        // Teardown came after the ingest's last round: collect still moves it into the records.
        std::fs::write(
            ingest.join(crate::phases::FILE),
            "{\"record\":\"phase\",\"ts_ns\":99,\"trial\":\"r1-idle\",\"phase\":\"teardown\"}\n",
        )
        .unwrap();
        let out = tmp.join("trial.zip");
        let meta = collect(&view, &ingest, &out, None).unwrap();
        let trial = meta.trial.unwrap();
        assert_eq!(
            (trial["trial_id"].as_str(), trial["repetition"].as_u64()),
            (Some("r1-idle"), Some(1))
        );
        let recs = records(&out).unwrap();
        assert!(
            recs.iter().any(|r| matches!(r, Record::Phase { ts_ns: 99, phase, trial: Some(t) } if phase == "teardown" && t == "r1-idle")),
            "{recs:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_daemons_tools_are_recorded_as_given() {
        let (tmp, view, ingest) = fixture("tools");
        let tools = r#"[{"name":"vxprobe","path":"/opt/vx/vxprobe","version":"0.2.2","sha256":"ab","profile":"release"}]"#;
        std::fs::write(ingest.join(crate::archive::TOOLS_FILE), tools).unwrap();
        let out = tmp.join("tools.zip");
        collect(&view, &ingest, &out, None).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut text = String::new();
        zip.by_name("metadata.json")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            metadata["tools"],
            serde_json::from_str::<serde_json::Value>(tools).unwrap()
        );

        // Anything but a list is not the daemon's tools, and is left out.
        std::fs::write(
            ingest.join(crate::archive::TOOLS_FILE),
            r#"{"vxprobe":"0.2.2"}"#,
        )
        .unwrap();
        let meta = collect(&view, &ingest, &tmp.join("not-a-list.zip"), None).unwrap();
        assert!(meta.tools.is_none());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_v2_archive_lists_checks_and_reads_back_its_records() {
        let (tmp, view, ingest) = fixture("roundtrip");
        let out = tmp.join("run.zip");
        let meta = collect(&view, &ingest, &out, None).unwrap();
        assert_eq!(meta.archive.as_deref(), Some(SCHEMA));
        assert!(is_v2(&out));
        let members = members_of(&out).unwrap();
        for m in [
            "index.json",
            "metadata.json",
            ENTITIES,
            SAMPLES,
            PHASES,
            "package/main.yaml",
        ] {
            assert!(members.contains_key(m), "{m}: {:?}", members.keys());
        }
        assert!(
            !members.keys().any(|k| k.starts_with("ingest/")),
            "v2 drops the raw ingest folder"
        );
        let v = verify(&out).unwrap();
        assert!(v.ok, "{v:?}");
        assert!(v.package_digest.is_some());

        let recs = records(&out).unwrap();
        assert_eq!(recs.len(), 3);
        assert!(matches!(&recs[0], Record::Sample { entity, .. } if entity == "car/front/ecu1"));
        assert!(matches!(&recs[1], Record::Gap { .. }));
        assert!(matches!(&recs[2], Record::Phase { phase, .. } if phase == "measure"));

        assert!(members.contains_key("logs/car/front/ecu1.jsonl.zst"));
        assert!(members.contains_key("logs/host/services/traffic.jsonl.zst"));
        let lines: Vec<(String, String)> = logs(&out)
            .unwrap()
            .into_iter()
            .map(|r| match r {
                Record::Log { entity, line, .. } => (entity, line),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            lines,
            [
                ("car/front/ecu1".into(), "boot".into()),
                ("car/front/ecu1".into(), "ready".into()),
                ("host/services/traffic".into(), "go".into()),
            ],
            "per entity, in the order read"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_changed_member_fails_the_index() {
        let (tmp, view, ingest) = fixture("tamper");
        let out = tmp.join("run.zip");
        collect(&view, &ingest, &out, None).unwrap();
        let mut members = members_of(&out).unwrap();
        members.insert(ENTITIES.into(), b"[]".to_vec());
        let forged = tmp.join("forged.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&forged).unwrap());
        for (p, b) in &members {
            zip.start_file(p.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b).unwrap();
        }
        zip.finish().unwrap();
        let v = verify(&forged).unwrap();
        assert!(!v.ok, "{v:?}");
        assert_eq!(v.index, Some(false));
        assert_eq!(v.identity.outcome, IdentityOutcome::Checked);
        assert_eq!(v.recorded, v.computed, "the identity alone still matches");
        assert_eq!(v.exit_code(), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// `members` as an archive with a fresh `index.json`, so the index check passes.
    fn reindexed(members: &BTreeMap<String, Vec<u8>>, out: &Path) {
        let files = members
            .iter()
            .filter(|(p, _)| p.as_str() != "index.json")
            .map(|(p, b)| IndexEntry {
                path: p.clone(),
                size: b.len() as u64,
                sha256: format!("sha256:{}", hex(b)),
            })
            .collect();
        let index = Index {
            schema: SCHEMA.into(),
            files,
        };
        let mut zip = zip::ZipWriter::new(std::fs::File::create(out).unwrap());
        zip.start_file("index.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&index).unwrap()).unwrap();
        for (p, b) in members.iter().filter(|(p, _)| p.as_str() != "index.json") {
            zip.start_file(p.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A collected archive's members, with `metadata.json` changed by `edit`.
    fn with_metadata(
        out: &Path,
        edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) -> BTreeMap<String, Vec<u8>> {
        let mut members = members_of(out).unwrap();
        let mut meta: serde_json::Value =
            serde_json::from_slice(&members["metadata.json"]).unwrap();
        edit(meta.as_object_mut().unwrap());
        members.insert(
            "metadata.json".into(),
            serde_json::to_vec_pretty(&meta).unwrap(),
        );
        members
    }

    #[test]
    fn a_checked_identity_verifies() {
        let (tmp, view, ingest) = fixture("checked");
        let out = tmp.join("run.zip");
        collect(&view, &ingest, &out, None).unwrap();
        let v = verify(&out).unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.index, Some(true));
        assert_eq!(v.identity.outcome, IdentityOutcome::Checked);
        assert_eq!(v.identity.reason, None);
        assert_eq!(v.algorithm.as_deref(), Some(crate::identity::CURRENT));
        assert!(v.computed.is_some() && v.recorded == v.computed);
        assert_eq!(v.exit_code(), 0);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_changed_package_member_is_an_identity_mismatch() {
        let (tmp, view, ingest) = fixture("mismatch");
        let out = tmp.join("run.zip");
        collect(&view, &ingest, &out, None).unwrap();
        let mut members = members_of(&out).unwrap();
        members.insert(
            "package/main.yaml".into(),
            b"kind: experiment
id: e3
"
            .to_vec(),
        );
        let forged = tmp.join("forged.zip");
        reindexed(&members, &forged);
        let v = verify(&forged).unwrap();
        assert!(!v.ok, "{v:?}");
        assert_eq!(v.index, Some(true), "the index was rewritten to match");
        assert_eq!(v.identity.outcome, IdentityOutcome::Mismatch);
        assert_ne!(v.recorded, v.computed);
        assert_eq!(v.exit_code(), 1);
        assert!(v.summary().contains("identity MISMATCH"), "{}", v.summary());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn no_recorded_algorithm_is_not_checkable_and_none_is_assumed() {
        let (tmp, view, ingest) = fixture("noalg");
        let out = tmp.join("run.zip");
        collect(&view, &ingest, &out, None).unwrap();
        // The identity package/ would re-hash to under the current algorithm: still not checked.
        let members = with_metadata(&out, |m| {
            m.remove("workspace_identity_algorithm");
            m.remove("package");
        });
        let old = tmp.join("old.zip");
        reindexed(&members, &old);
        let v = verify(&old).unwrap();
        assert!(!v.ok, "{v:?}");
        assert_eq!(v.index, Some(true));
        assert_eq!(v.identity.outcome, IdentityOutcome::NotCheckable);
        assert_eq!(v.identity.reason.as_deref(), Some(NO_ALGORITHM));
        assert_eq!(
            (v.algorithm.as_deref(), v.computed.as_deref()),
            (None, None)
        );
        assert!(v.recorded.is_some(), "the recorded identity is still shown");
        assert_eq!(v.exit_code(), crate::archive::EXIT_NOT_CHECKABLE);
        assert_eq!(
            v.summary(),
            format!("index OK; identity not checkable: {NO_ALGORITHM}")
        );
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["identity"]["outcome"], "not_checkable");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_manifest_digest_alone_is_not_checkable() {
        let (tmp, view, ingest) = fixture("manifest");
        let out = tmp.join("run.zip");
        collect(&view, &ingest, &out, None).unwrap();
        let members = with_metadata(&out, |m| {
            m.remove("workspace_sha256");
            m.remove("workspace_identity_algorithm");
            m.remove("package");
            m.insert("manifest_sha256".into(), "0".repeat(64).into());
        });
        let old = tmp.join("old.zip");
        reindexed(&members, &old);
        let v = verify(&old).unwrap();
        assert_eq!(v.index, Some(true));
        assert_eq!(v.identity.outcome, IdentityOutcome::NotCheckable);
        assert_eq!(v.identity.reason.as_deref(), Some(MANIFEST_ONLY));
        assert_eq!(v.recorded, None);
        assert_eq!(v.exit_code(), crate::archive::EXIT_NOT_CHECKABLE);

        // A failed index is a failure whatever the identity says.
        let mut tampered = members_of(&old).unwrap();
        tampered.insert(ENTITIES.into(), b"[]".to_vec());
        let forged = tmp.join("forged.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&forged).unwrap());
        for (p, b) in &tampered {
            zip.start_file(p.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b).unwrap();
        }
        zip.finish().unwrap();
        let v = verify(&forged).unwrap();
        assert_eq!(v.index, Some(false));
        assert_eq!(v.identity.outcome, IdentityOutcome::NotCheckable);
        assert_eq!(v.exit_code(), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_v1_archive_reads_as_the_same_records() {
        let (tmp, view, ingest) = fixture("v1");
        std::fs::remove_file(ingest.join("records.jsonl")).unwrap();
        let out = tmp.join("v1.zip");
        crate::archive::collect(&view, &ingest, &out, None).unwrap();
        assert!(!is_v2(&out));
        let recs = records(&out).unwrap();
        assert_eq!(
            recs,
            [Record::Sample {
                ts_ns: 1_000_000,
                entity: "car/front/ecu1".into(),
                metric: "m".into(),
                labels: BTreeMap::from([("q".to_string(), "1".to_string())]),
                value: 2.0,
            }]
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
