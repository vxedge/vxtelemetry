//! Results archive: bundle the ingest dir + reproducibility metadata into
//! `results/<exp>.zip` (§13: experiment ID, workspace checksum, tool version,
//! and — when the caller opts in via `--record-image` — container image
//! digests).
//!
//! The archive carries the **workspace** the run provisioned from, file for file, under
//! `workspace/`: every file the workspace identity covers, documents, parts, assets, scripts and
//! tools, and nothing it excludes. So the archive is enough to re-run from, and it verifies
//! itself: re-hashing `workspace/` under `workspace_identity_algorithm` gives back
//! `workspace_sha256` ([`verify`]). Older archives carried only the documents and name no
//! algorithm; they cannot be checked this way, and [`verify`] says so.

use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use crate::TelemetryError;
use crate::ingest::IngestStats;
use crate::workspace_view::{PackageIdentity, WorkspaceView};

#[derive(Serialize)]
pub struct ArchiveMetadata {
    pub experiment_id: String,
    /// SHA-256 over every file of the source workspace. Replaces `manifest_sha256`, which
    /// hashed a §6.1 document that no longer exists; analysis notebooks reading archives from
    /// both eras should accept either key.
    pub workspace_sha256: String,
    /// The rule that computed `workspace_sha256` (`vxws/2` today). An archive without this key
    /// was written before `vxws/2` and holds a `vxws/1` value; identities under different
    /// algorithms are not comparable.
    pub workspace_identity_algorithm: String,
    /// The `.vxp` descriptor's id and version, when the workspace carries one — the identity a
    /// published experiment is cited by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageIdentity>,
    pub collected_at_unix_ms: i64,
    pub vxtelemetry_version: String,
    pub ingest: Option<IngestStats>,
    /// Content-addressed identities of the container images the experiment
    /// ran against (§13 reproducibility) — present only when the caller
    /// passed `--record-image`, absent (not `null`) otherwise so archives
    /// that predate this field keep an identical `metadata.json` shape.
    /// Which images matter is the campaign runner's knowledge, not this
    /// tool's: guessing here would mean hardcoding another repo's image
    /// naming (e.g. vxedge-core's fiveg pod images) into vxtelemetry.
    /// Values are a repo digest / image ID, [`HOST_PROCESS`] for a `local/`
    /// host-process part, or an honest `unresolved: …` string — a digest
    /// lookup failing must never destroy a completed campaign point's
    /// archive, but silently omitting the failure would misrepresent what was
    /// recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digests: Option<BTreeMap<String, String>>,
    /// The kernel of the host that collected, which is the run's host (`ARCHITECTURE.md` §13):
    /// `kernel.preempt_rt: preferred` runs on either kind, so the result says which. Absent in
    /// archives from before it was recorded, and where the kernel cannot be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<Kernel>,
}

/// The running kernel, read the way `vxedge-core` reads it and independently of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kernel {
    /// As `uname -r` prints it.
    pub release: String,
    pub preempt_rt: bool,
}

impl Kernel {
    /// `None` where `/proc/sys/kernel/osrelease` cannot be read, which is not Linux.
    pub fn probe() -> Option<Self> {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        let release = read("/proc/sys/kernel/osrelease")?.trim().to_string();
        let version = read("/proc/sys/kernel/version").unwrap_or_default();
        Some(Self {
            release,
            preempt_rt: is_preempt_rt(read("/sys/kernel/realtime").as_deref(), &version),
        })
    }
}

/// `/sys/kernel/realtime` decides when present; a kernel without it says so only in its version
/// string (`uname -v`). Pure.
fn is_preempt_rt(realtime: Option<&str>, version: &str) -> bool {
    match realtime {
        Some(flag) => flag.trim() == "1",
        None => version.contains("PREEMPT_RT"),
    }
}

/// Pull the content-addressed identity out of one `podman image inspect`
/// JSON document (the array element, not the array): the first repo digest
/// when the image was pulled/pushed (`RepoDigests[0]`, registry-verifiable),
/// else the local image ID (`Id`, still content-addressed — locally-built
/// images legitimately have no repo digest until pushed). Pure, so the
/// parsing is unit-testable without podman.
fn digest_from_inspect(doc: &serde_json::Value) -> Option<String> {
    if let Some(digest) = doc
        .get("RepoDigests")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .and_then(|d| d.as_str())
    {
        return Some(digest.to_string());
    }
    doc.get("Id")
        .and_then(|i| i.as_str())
        .map(|i| i.to_string())
}

/// Resolve each image reference to its content-addressed identity via
/// `podman image inspect` (argv exec, JSON read — no shell interpolation).
/// Best-effort per image: failures are recorded as `unresolved: …` values
/// rather than propagated, per `ArchiveMetadata::image_digests`'s contract.
/// What a `local/...` part records: vxedge runs it as a host process (`PAYLOAD_KIND_HOST_PROCESS`),
/// so there is no image to inspect. Distinct from `localhost/...`, a locally built Podman image.
pub const HOST_PROCESS: &str = "host-process: a local/ part runs on the host, not from an image";

pub fn resolve_image_digests(images: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for image in images {
        if image.starts_with("local/") {
            out.insert(image.clone(), HOST_PROCESS.to_string());
            continue;
        }
        let resolved = match Command::new("podman")
            .args(["image", "inspect", image])
            .output()
        {
            Err(e) => format!("unresolved: cannot exec podman: {e}"),
            Ok(o) if !o.status.success() => format!(
                "unresolved: podman image inspect failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Ok(o) => serde_json::from_slice::<serde_json::Value>(&o.stdout)
                .ok()
                .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
                .and_then(|doc| digest_from_inspect(&doc))
                .unwrap_or_else(|| {
                    "unresolved: inspect output had no RepoDigests or Id".to_string()
                }),
        };
        out.insert(image.clone(), resolved);
    }
    out
}

fn walk(
    dir: &Path,
    base: &Path,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, base, out)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .expect("walked path under base")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

/// Create `out` (zip) from the ingest dir + the source workspace + metadata.
pub fn collect(
    workspace: &WorkspaceView,
    ingest_dir: &Path,
    out: &Path,
    image_digests: Option<BTreeMap<String, String>>,
) -> Result<ArchiveMetadata, TelemetryError> {
    let arch_err = |e: zip::result::ZipError| TelemetryError::Archive(e.to_string());

    let ingest_stats: Option<IngestStats> = std::fs::read(ingest_dir.join("ingest-stats.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());

    let metadata = ArchiveMetadata {
        experiment_id: workspace.experiment_id.clone(),
        workspace_sha256: workspace.content_sha256(),
        workspace_identity_algorithm: workspace.identity_algorithm().to_string(),
        package: workspace.package.clone(),
        collected_at_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_millis() as i64,
        vxtelemetry_version: env!("CARGO_PKG_VERSION").to_string(),
        ingest: ingest_stats,
        image_digests,
        kernel: Kernel::probe(),
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();

    zip.start_file("metadata.json", opts).map_err(arch_err)?;
    zip.write_all(&serde_json::to_vec_pretty(&metadata).expect("serializable metadata"))?;

    // Every input the identity covers, so `workspace/` re-hashes to `workspace_sha256` (see
    // [`verify`]) and is enough to re-run from.
    for (name, bytes) in &workspace.inputs {
        zip.start_file(format!("workspace/{name}"), opts)
            .map_err(arch_err)?;
        zip.write_all(bytes)?;
    }

    let mut files = Vec::new();
    walk(ingest_dir, ingest_dir, &mut files)?;
    files.sort();
    for (rel, path) in files {
        zip.start_file(format!("ingest/{rel}"), opts)
            .map_err(arch_err)?;
        zip.write_all(&std::fs::read(&path)?)?;
    }
    zip.finish().map_err(arch_err)?;
    Ok(metadata)
}

/// What [`verify`] found: the identity the archive records and the one its `workspace/`
/// re-hashes to, and the same for the package digest when the archive records one.
#[derive(Debug, Serialize)]
pub struct Verification {
    /// Both the identity and, when recorded, the package digest match.
    pub ok: bool,
    pub algorithm: String,
    pub recorded: String,
    pub computed: String,
    /// Files under `workspace/`.
    pub files: usize,
    /// Present when `metadata.json` records `package.digest` (a `vxp/1` package).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_digest: Option<DigestCheck>,
}

#[derive(Debug, Serialize)]
pub struct DigestCheck {
    pub recorded: String,
    pub computed: String,
}

/// Re-hash an archive's `workspace/` under its recorded algorithm and compare with its recorded
/// `workspace_sha256`. Every entry under `workspace/` counts and nothing is excluded: the
/// archive holds the identity's inputs already, so anything added to it is a difference.
///
/// An archive that names no algorithm was written when archives carried only the workspace
/// documents; it is refused with that reason rather than reported as a mismatch.
pub fn verify(archive: &Path) -> Result<Verification, TelemetryError> {
    let arch_err = |e: zip::result::ZipError| TelemetryError::Archive(e.to_string());
    let bad = |msg: String| TelemetryError::Archive(format!("`{}`: {msg}", archive.display()));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?).map_err(arch_err)?;

    let metadata: serde_json::Value = {
        let mut entry = zip
            .by_name("metadata.json")
            .map_err(|_| bad("no metadata.json; not a results archive".into()))?;
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        serde_json::from_str(&text).map_err(|e| bad(format!("metadata.json: {e}")))?
    };
    let recorded = metadata["workspace_sha256"]
        .as_str()
        .ok_or_else(|| bad("metadata.json has no workspace_sha256".into()))?
        .to_string();
    let algorithm = metadata["workspace_identity_algorithm"]
        .as_str()
        .ok_or_else(|| {
            bad(
                "metadata.json names no workspace_identity_algorithm: the archive was written \
                 when archives carried only the workspace documents, so its workspace/ cannot \
                 reproduce its identity"
                    .into(),
            )
        })?
        .to_string();

    let mut inputs = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(arch_err)?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry
            .name()
            .map_err(arch_err)?
            .strip_prefix("workspace/")
            .map(str::to_string)
        else {
            continue;
        };
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        inputs.push((rel, bytes));
    }
    inputs.sort();
    let computed = crate::identity::digest(&algorithm, &inputs)?;
    // `workspace/` holds exactly the packed entries, so it packs to the recorded digest.
    let package_digest = match metadata["package"]["digest"].as_str() {
        Some(recorded) => Some(DigestCheck {
            recorded: recorded.to_string(),
            computed: crate::package::digest(&inputs)?,
        }),
        None => None,
    };
    Ok(Verification {
        ok: computed == recorded
            && package_digest
                .as_ref()
                .is_none_or(|d| d.recorded == d.computed),
        algorithm,
        recorded,
        computed,
        files: inputs.len(),
        package_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_prefers_repo_digest_over_local_id() {
        let doc = serde_json::json!({
            "Id": "abc123",
            "RepoDigests": ["docker.io/library/mongo@sha256:deadbeef"],
        });
        assert_eq!(
            digest_from_inspect(&doc).as_deref(),
            Some("docker.io/library/mongo@sha256:deadbeef")
        );
    }

    #[test]
    fn digest_falls_back_to_id_for_locally_built_images() {
        // Locally-built images (vx5g-core/vx5g-ran today) legitimately have
        // an empty RepoDigests until pushed somewhere — Id is still a
        // content-addressed identity.
        let doc = serde_json::json!({ "Id": "abc123", "RepoDigests": [] });
        assert_eq!(digest_from_inspect(&doc).as_deref(), Some("abc123"));
        let doc_no_field = serde_json::json!({ "Id": "abc123" });
        assert_eq!(
            digest_from_inspect(&doc_no_field).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn digest_is_none_when_document_has_neither() {
        assert_eq!(digest_from_inspect(&serde_json::json!({})), None);
    }

    #[test]
    fn metadata_omits_image_digests_entirely_when_absent() {
        // Archives that predate --record-image must keep an identical
        // metadata.json shape — absent, not null.
        let meta = ArchiveMetadata {
            experiment_id: "t".into(),
            workspace_sha256: "0".repeat(64),
            workspace_identity_algorithm: "vxws/2".into(),
            package: None,
            collected_at_unix_ms: 0,
            vxtelemetry_version: "0".into(),
            ingest: None,
            image_digests: None,
            kernel: None,
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert!(json.get("image_digests").is_none());

        let mut digests = BTreeMap::new();
        digests.insert("vx5g-core".to_string(), "abc".to_string());
        let meta_with = ArchiveMetadata {
            image_digests: Some(digests),
            ..meta
        };
        let json = serde_json::to_value(&meta_with).unwrap();
        assert_eq!(json["image_digests"]["vx5g-core"], "abc");
    }

    #[test]
    fn a_host_process_part_is_recorded_as_one_not_as_unresolved() {
        let out = resolve_image_digests(&["local/vxprobe:host-process".to_string()]);
        assert_eq!(out["local/vxprobe:host-process"], HOST_PROCESS);
    }

    #[test]
    fn unresolvable_image_is_recorded_honestly_not_dropped() {
        // A nonsense image name must yield an `unresolved:` entry (whether
        // podman exists on this machine or not), never a missing key and
        // never an error.
        let images = vec!["vxedge-test-image-that-cannot-exist:v0".to_string()];
        let out = resolve_image_digests(&images);
        let entry = out
            .get("vxedge-test-image-that-cannot-exist:v0")
            .expect("entry present");
        assert!(entry.starts_with("unresolved:"), "got: {entry}");
    }

    /// G1: an archive is enough to re-run from and to check. Its `workspace/` holds every file
    /// the identity covers, so unpacking it and re-hashing under the recorded algorithm gives
    /// back `workspace_sha256`. Excluded files (`.git`, editor droppings) stay out.
    #[test]
    fn the_archived_workspace_rehashes_to_the_recorded_identity() {
        let tmp = std::env::temp_dir().join(format!("vxtel-selfverify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let ws = tmp.join("ws");
        for (rel, body) in [
            ("main.yaml", "kind: experiment\nid: exp-sv\n"),
            (
                "exp-sv.vxp",
                "id: exp-sv\nversion: 0.1.0\nentry: main.yaml\n",
            ),
            ("parts/local-thing", "payload\n"),
            ("scripts/run.sh", "#!/usr/bin/env bash\n"),
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("main.yaml~", "backup\n"),
        ] {
            let p = ws.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let ingest = tmp.join("ingest");
        std::fs::create_dir_all(&ingest).unwrap();
        let out = tmp.join("exp-sv.zip");
        let view = WorkspaceView::load(&ws).unwrap();
        let meta = collect(&view, &ingest, &out, None).unwrap();

        let unpacked = tmp.join("unpacked");
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut names = Vec::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let name = entry.name().unwrap().to_string();
            if let Some(rel) = name.strip_prefix("workspace/") {
                names.push(rel.to_string());
                let p = unpacked.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::io::copy(&mut entry, &mut std::fs::File::create(p).unwrap()).unwrap();
            }
        }
        names.sort();
        assert_eq!(
            names,
            [
                "exp-sv.vxp",
                "main.yaml",
                "parts/local-thing",
                "scripts/run.sh"
            ]
        );
        assert_eq!(
            crate::identity::compute(&unpacked, &meta.workspace_identity_algorithm).unwrap(),
            meta.workspace_sha256
        );
        let v = verify(&out).unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.files, 4);
        std::fs::remove_dir_all(&tmp).ok();
    }

    fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A file added to, or changed in, an archive's `workspace/` is caught.
    #[test]
    fn a_tampered_archive_fails_verification() {
        let tmp = std::env::temp_dir().join(format!("vxtel-tamper-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let main: &[u8] = b"kind: experiment\nid: e\n";
        let id = crate::identity::digest("vxws/2", &[("main.yaml".into(), main.to_vec())]).unwrap();
        let meta = serde_json::json!({
            "workspace_sha256": id, "workspace_identity_algorithm": "vxws/2",
        })
        .to_string();
        let good = tmp.join("good.zip");
        zip_with(
            &good,
            &[
                ("metadata.json", meta.as_bytes()),
                ("workspace/main.yaml", main),
            ],
        );
        assert!(verify(&good).unwrap().ok);

        let extra = tmp.join("extra.zip");
        zip_with(
            &extra,
            &[
                ("metadata.json", meta.as_bytes()),
                ("workspace/main.yaml", main),
                ("workspace/parts/x", b"added"),
            ],
        );
        assert!(!verify(&extra).unwrap().ok);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An archive from before self-verifying archives is refused with the reason, not reported
    /// as a mismatch: its identity is real, it just was never meant to be reproducible this way.
    #[test]
    fn an_archive_naming_no_algorithm_is_refused_with_the_reason() {
        let tmp = std::env::temp_dir().join(format!("vxtel-old-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let old = tmp.join("old.zip");
        let meta = serde_json::json!({ "workspace_sha256": "0".repeat(64) }).to_string();
        zip_with(&old, &[("metadata.json", meta.as_bytes())]);
        let err = verify(&old).unwrap_err().to_string();
        assert!(err.contains("workspace_identity_algorithm"), "{err}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A `vxp/1` package's archive records its package digest and the kernel, and `verify`
    /// re-packs `workspace/` to the recorded digest: the bytes shipped are the package shared.
    #[test]
    fn a_vxp1_archive_records_and_verifies_its_package_digest() {
        let tmp = std::env::temp_dir().join(format!("vxtel-pkgdigest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let ws = tmp.join("ws");
        for (rel, body) in [
            ("main.yaml", "kind: experiment\nid: exp-pd\n"),
            (
                "vxp.yaml",
                "schema: vxp/1\nid: vxedge/exp-pd\nversion: 1.0.0\nentry: main.yaml\n",
            ),
            ("vxp.lock", "schema: vxp-lock/1\n"),
        ] {
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(ws.join(rel), body).unwrap();
        }
        let ingest = tmp.join("ingest");
        std::fs::create_dir_all(&ingest).unwrap();
        let mut view = WorkspaceView::load(&ws).unwrap();
        view.package.as_mut().unwrap().locked = Some(false);
        let out = tmp.join("exp-pd.zip");
        let meta = collect(&view, &ingest, &out, None).unwrap();
        let package = meta.package.as_ref().unwrap();
        let digest = crate::package::digest(&view.inputs).unwrap();
        assert_eq!(package.digest.as_deref(), Some(digest.as_str()));
        assert_eq!(package.locked, Some(false));
        #[cfg(target_os = "linux")]
        assert!(!meta.kernel.as_ref().unwrap().release.is_empty());

        let v = verify(&out).unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.package_digest.as_ref().unwrap().computed, digest);

        // A recorded digest the workspace does not pack to fails, even with the identity intact.
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..zip.len() {
            let mut e = zip.by_index(i).unwrap();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut bytes).unwrap();
            entries.push((e.name().unwrap().to_string(), bytes));
        }
        let forged = tmp.join("forged.zip");
        let entries: Vec<(String, Vec<u8>)> = entries
            .into_iter()
            .map(|(n, b)| {
                if n == "metadata.json" {
                    let text = String::from_utf8(b)
                        .unwrap()
                        .replace(&digest, &format!("sha256:{}", "0".repeat(64)));
                    (n, text.into_bytes())
                } else {
                    (n, b)
                }
            })
            .collect();
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        zip_with(&forged, &refs);
        let v = verify(&forged).unwrap();
        assert!(!v.ok, "{v:?}");
        assert_eq!(v.recorded, v.computed, "the identity alone still matches");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_realtime_kernel_is_read_from_sysfs_first() {
        assert!(is_preempt_rt(Some("1\n"), ""));
        assert!(!is_preempt_rt(Some("0\n"), "#1 SMP PREEMPT_RT"));
        assert!(is_preempt_rt(None, "#1 SMP PREEMPT_RT Thu"));
        assert!(!is_preempt_rt(None, "#1 SMP PREEMPT_DYNAMIC"));
    }
}
