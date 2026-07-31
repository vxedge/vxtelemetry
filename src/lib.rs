//! vxtelemetry core: scrape agent /metrics endpoints per an experiment
//! workspace, persist timestamped samples, and bundle everything into the
//! reproducible results archive (`results/<exp>.zip`).
//!
//! Workspace handling: this crate reads only the slice of the §6.0 workspace
//! it needs (experiment id, scrape targets, artifact dir, package identity)
//! with unknown fields ignored — full cross-file validation is the vxedge
//! CLI/daemon's job. See `workspace_view` for why it parses that itself
//! rather than depending on `vxedge-core`.

pub mod archive;
pub mod ingest;
pub mod promtext;
pub mod sample;
pub mod scrape;
pub mod workspace_view;

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("workspace error: {0}")]
    Manifest(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("scrape `{target}` failed: {detail}")]
    Scrape { target: String, detail: String },
    #[error("archive error: {0}")]
    Archive(String),
}
