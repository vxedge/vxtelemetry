//! Results archive: bundle the ingest dir + reproducibility metadata into
//! `results/<exp>.zip` (§13: experiment ID, manifest checksum, tool version).

use serde::Serialize;
use sha2::Digest;
use std::io::Write;
use std::path::Path;

use crate::TelemetryError;
use crate::ingest::IngestStats;

#[derive(Serialize)]
pub struct ArchiveMetadata {
    pub experiment_id: String,
    pub manifest_sha256: String,
    pub collected_at_unix_ms: i64,
    pub vxtelemetry_version: String,
    pub ingest: Option<IngestStats>,
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
