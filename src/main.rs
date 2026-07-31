//! vxtelemetry — scrape agent metrics per experiment workspace; archive results.
//!
//! `vxtelemetry ingest  --workspace exp-001-ws --ingest-dir /tmp/exp-001 --duration-s 60`
//! `vxtelemetry collect --workspace exp-001-ws --ingest-dir /tmp/exp-001 --out results/exp-001.zip`
//!
//! `--workspace` names a §6.0 experiment **directory**. `--manifest` stays as an alias so
//! campaign scripts keep parsing, but it takes the same directory: §6.1 manifests no longer
//! exist to point it at.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;
use vxtelemetry::workspace_view::WorkspaceView;
use vxtelemetry::{archive, ingest, sample};

#[derive(Parser)]
#[command(
    name = "vxtelemetry",
    version,
    about = "vxedge telemetry ingestion + results archiving"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scrape the workspace's targets on an interval; persist raw + structured samples.
    Ingest {
        /// The §6.0 experiment workspace directory.
        #[arg(long, alias = "manifest")]
        workspace: PathBuf,
        #[arg(long)]
        ingest_dir: PathBuf,
        #[arg(long, default_value_t = 5)]
        interval_s: u64,
        #[arg(long, default_value_t = 60)]
        duration_s: u64,
    },
    /// Sample CPU%/RSS of processes matched by cmdline regex (label=regex).
    Sample {
        #[arg(long = "match", required = true)]
        matchers: Vec<String>,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 500)]
        interval_ms: u64,
        #[arg(long, default_value_t = 60)]
        duration_s: u64,
    },
    /// Bundle ingest dir + the source workspace + reproducibility metadata into a zip.
    Collect {
        /// The §6.0 experiment workspace directory.
        #[arg(long, alias = "manifest")]
        workspace: PathBuf,
        #[arg(long)]
        ingest_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Container image reference(s) whose content-addressed digest should
        /// be recorded in the archive's metadata (repeatable). Resolved via
        /// `podman image inspect` at collect time; a failed lookup is
        /// recorded as an `unresolved: …` value rather than aborting the
        /// archive. Which images matter is the campaign runner's call — this
        /// tool doesn't guess.
        #[arg(long = "record-image")]
        record_images: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // stdout is the machine-readable JSON contract
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Ingest {
            workspace,
            ingest_dir,
            interval_s,
            duration_s,
        } => {
            let view = WorkspaceView::load(&workspace)?;
            let targets = view.scrape_targets();
            if targets.is_empty() {
                return Err("the workspace declares no telemetry.prometheus.scrape_targets".into());
            }
            tracing::info!(experiment = %view.experiment_id, targets = targets.len(), "ingest starting");
            let stats = ingest::run_ingest(
                &targets,
                ingest_dir,
                Duration::from_secs(interval_s.max(1)),
                Duration::from_secs(duration_s),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
            if stats.completeness_pct() < 100.0 {
                tracing::warn!(
                    completeness_pct = stats.completeness_pct(),
                    "some scrapes failed — completeness below 100%"
                );
            }
        }
        Cmd::Sample {
            matchers,
            out_dir,
            interval_ms,
            duration_s,
        } => {
            let parsed: Result<Vec<_>, _> =
                matchers.iter().map(|m| sample::Matcher::parse(m)).collect();
            let summary = sample::run_sample(
                &parsed?,
                &out_dir,
                Duration::from_millis(interval_ms.max(100)),
                Duration::from_secs(duration_s),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Cmd::Collect {
            workspace,
            ingest_dir,
            out,
            record_images,
        } => {
            let metadata = tokio::task::spawn_blocking(move || {
                let view = WorkspaceView::load(&workspace)?;
                let image_digests = if record_images.is_empty() {
                    None
                } else {
                    Some(archive::resolve_image_digests(&record_images))
                };
                archive::collect(&view, &ingest_dir, &out, image_digests)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&metadata)?);
        }
    }
    Ok(())
}
