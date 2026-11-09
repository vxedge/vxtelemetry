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
        /// Whether the daemon checked the package's lock at `up` (`false` for `up --unlocked`),
        /// recorded as `package.locked` for a `vxp/1` package. Left out, nothing is recorded:
        /// this tool never derives it. The daemon passes it in the environment, so an older
        /// vxtelemetry without this option still collects.
        #[arg(long, env = "VXTELEMETRY_PACKAGE_LOCKED")]
        package_locked: Option<bool>,
        /// The archive format: 1 (the default, which today's notebooks read) or 2
        /// (`vxarchive/2`: entity-named records, zstd-compressed, an index of every member).
        #[arg(long, env = "VXTELEMETRY_ARCHIVE_VERSION", default_value_t = 1,
              value_parser = clap::value_parser!(u8).range(1..=2))]
        archive_version: u8,
    },
    /// Re-hash an archive's `workspace/` under its recorded algorithm and compare with its
    /// recorded `workspace_sha256`. Prints the result as JSON; exits 1 on a mismatch.
    Verify {
        /// The results archive (`.zip`) written by `collect`.
        archive: PathBuf,
    },
    /// Print an archive's records, v1 or v2, as JSON Lines: samples and gaps, then phases.
    Records {
        /// The results archive (`.zip`).
        archive: PathBuf,
        /// Print its `log` records instead, entity by entity (v2 only).
        #[arg(long)]
        logs: bool,
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
            let mut targets = view.scrape_targets();
            // Targets the daemon added beside the workspace's own, such as its exporter of
            // per-zone and per-slice metrics, are listed in `entities.json`. They are optional:
            // a run without them is not incomplete.
            for extra in vxtelemetry::records::Entities::load(&ingest_dir).targets() {
                if !targets.iter().any(|t| t.address == extra) {
                    targets.push(vxtelemetry::workspace_view::ScrapeTarget {
                        address: extra.to_string(),
                        optional: true,
                    });
                }
            }
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
            package_locked,
            archive_version,
        } => {
            let metadata = tokio::task::spawn_blocking(move || {
                let mut view = WorkspaceView::load(&workspace)?;
                if let Some(p) = view.package.as_mut().filter(|p| p.digest.is_some()) {
                    p.locked = package_locked;
                }
                let image_digests = if record_images.is_empty() {
                    None
                } else {
                    Some(archive::resolve_image_digests(&record_images))
                };
                if archive_version == 2 {
                    vxtelemetry::archive_v2::collect(&view, &ingest_dir, &out, image_digests)
                } else {
                    archive::collect(&view, &ingest_dir, &out, image_digests)
                }
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&metadata)?);
        }
        Cmd::Records { archive, logs } => {
            let recs = tokio::task::spawn_blocking(move || {
                if logs {
                    vxtelemetry::archive_v2::logs(&archive)
                } else {
                    vxtelemetry::archive_v2::records(&archive)
                }
            })
            .await??;
            let mut out = std::io::stdout().lock();
            for r in &recs {
                use std::io::Write;
                writeln!(out, "{}", serde_json::to_string(r)?)?;
            }
        }
        Cmd::Verify { archive } => {
            let v = tokio::task::spawn_blocking(move || {
                if vxtelemetry::archive_v2::is_v2(&archive) {
                    vxtelemetry::archive_v2::verify(&archive)
                } else {
                    archive::verify(&archive)
                }
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&v)?);
            if !v.ok {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
