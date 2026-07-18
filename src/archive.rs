//! Results archive: bundle the ingest dir + reproducibility metadata into
//! `results/<exp>.zip` (§13: experiment ID, manifest checksum, tool version,
//! and — when the caller opts in via `--record-image` — container image
//! digests).

use serde::Serialize;
use sha2::Digest;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::TelemetryError;
use crate::ingest::IngestStats;

#[derive(Serialize)]
pub struct ArchiveMetadata {
    pub experiment_id: String,
    pub manifest_sha256: String,
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
    /// Values are either a repo digest / image ID, or an honest
    /// `unresolved: …` string — a digest lookup failing must never destroy
    /// a completed campaign point's archive, but silently omitting the
    /// failure would misrepresent what was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digests: Option<BTreeMap<String, String>>,
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
pub fn resolve_image_digests(images: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for image in images {
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

/// Create `out` (zip) from the ingest dir + manifest + metadata.
pub fn collect(
    manifest_yaml: &str,
    experiment_id: &str,
    ingest_dir: &Path,
    out: &Path,
    image_digests: Option<BTreeMap<String, String>>,
) -> Result<ArchiveMetadata, TelemetryError> {
    let arch_err = |e: zip::result::ZipError| TelemetryError::Archive(e.to_string());

    let ingest_stats: Option<IngestStats> = std::fs::read(ingest_dir.join("ingest-stats.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());

    let metadata = ArchiveMetadata {
        experiment_id: experiment_id.to_string(),
        manifest_sha256: format!("{:x}", sha2::Sha256::digest(manifest_yaml.as_bytes())),
        collected_at_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_millis() as i64,
        vxtelemetry_version: env!("CARGO_PKG_VERSION").to_string(),
        ingest: ingest_stats,
        image_digests,
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();

    zip.start_file("metadata.json", opts).map_err(arch_err)?;
    zip.write_all(&serde_json::to_vec_pretty(&metadata).expect("serializable metadata"))?;

    zip.start_file("manifest.yaml", opts).map_err(arch_err)?;
    zip.write_all(manifest_yaml.as_bytes())?;

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
            manifest_sha256: "0".repeat(64),
            collected_at_unix_ms: 0,
            vxtelemetry_version: "0".into(),
            ingest: None,
            image_digests: None,
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
}
