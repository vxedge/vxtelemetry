//! vxtelemetry — scrape agent metrics per experiment manifest; archive results.
//!
//! `vxtelemetry ingest  --manifest exp.yaml --ingest-dir /tmp/exp-001 --duration-s 60`
//! `vxtelemetry collect --manifest exp.yaml --ingest-dir /tmp/exp-001 --out results/exp-001.zip`

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;
use vxtelemetry::manifest_view::ManifestView;
use vxtelemetry::{archive, ingest};

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
    /// Scrape manifest targets on an interval; persist raw + structured samples.
    Ingest {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        ingest_dir: PathBuf,
        #[arg(long, default_value_t = 5)]
        interval_s: u64,
        #[arg(long, default_value_t = 60)]
        duration_s: u64,
    },
    /// Bundle ingest dir + manifest + reproducibility metadata into a zip.
    Collect {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        ingest_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Ingest {
            manifest,
            ingest_dir,
            interval_s,
            duration_s,
        } => {
            let yaml = tokio::fs::read_to_string(&manifest).await?;
            let view = ManifestView::from_yaml(&yaml)?;
            let targets = view.scrape_targets();
            if targets.is_empty() {
                return Err("manifest declares no telemetry.prometheus.scrape_targets".into());
            }
            tracing::info!(experiment = %view.experiment.id, targets = targets.len(), "ingest starting");
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
        Cmd::Collect {
            manifest,
            ingest_dir,
            out,
        } => {
            let yaml = tokio::fs::read_to_string(&manifest).await?;
            let view = ManifestView::from_yaml(&yaml)?;
            let metadata = tokio::task::spawn_blocking(move || {
                archive::collect(&yaml, &view.experiment.id, &ingest_dir, &out)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&metadata)?);
        }
    }
    Ok(())
}
