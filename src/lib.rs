//! vxtelemetry core: scrape agent /metrics endpoints per an experiment
//! manifest, persist timestamped samples, and bundle everything into the
//! reproducible results archive (`results/<exp>.zip`).
//!
//! Manifest handling: this crate reads only the slice of the manifest it
//! needs (experiment id, scrape targets, artifact dir) with unknown fields
//! ignored — full schema validation is the vxedge CLI/daemon's job.

pub mod archive;
pub mod ingest;
pub mod manifest_view;
pub mod promtext;
pub mod scrape;

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("scrape `{target}` failed: {detail}")]
    Scrape { target: String, detail: String },
    #[error("archive error: {0}")]
    Archive(String),
}
