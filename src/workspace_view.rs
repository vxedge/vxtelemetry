//! Partial, tolerant view of a §6.0 experiment **workspace**: only the keys telemetry needs.
//!
//! This replaces the §6.1 `ManifestView` and is the change that cuts the last tie to §6.1 —
//! `vxedge-core`'s `lower.rs` existed only so the daemon could hand this tool a §6.1 file to
//! read (`vxedge/TASKS.md`, task 5f).
//!
//! # Why this parses the workspace itself rather than depending on `vxedge-core`
//!
//! The obvious alternative is `Workspace::load_validated` + `model::build` from `vxedge-core`.
//! Two things argue against it. `vxedge` is a separate repository, so the dependency would be a
//! cross-repo path or git reference that this repo's CI cannot resolve without cross-repo
//! credentials — a measurement tool would stop building for an authorisation reason. And the
//! blocks read here (`telemetry`, `results`) are **untyped passthrough** in the domain model:
//! `vxedge-core` carries them as raw YAML rather than parsing them, so depending on it would
//! not share a single line of schema. What is shared is one structural convention — a workspace
//! is a flat folder of YAML documents, each declaring its `kind` — and that is asserted here
//! against real workspaces in the tests.
//!
//! Unknown fields are ignored on purpose: cross-file validation is the vxedge CLI's job, and a
//! second validator that can disagree is worse than none.

use serde::Deserialize;
use sha2::Digest;
use std::path::{Path, PathBuf};

use crate::TelemetryError;

/// A loaded workspace, reduced to what telemetry acts on.
#[derive(Debug)]
pub struct WorkspaceView {
    pub root: PathBuf,
    pub experiment_id: String,
    /// The `.vxp` package descriptor's identity, when the workspace has one.
    pub package: Option<PackageIdentity>,
    /// Every file that makes up the workspace, in name order: `(name, bytes)`. Carried so the
    /// archive can record *what was run from* rather than a summary of it.
    pub files: Vec<(String, Vec<u8>)>,
    telemetry: Option<TelemetryBlock>,
    results: Option<ResultsBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct PackageIdentity {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Enough of any workspace document to route it, matching `vxedge-core`'s own loader: every
/// file declares its `kind`, and nothing is referenced by path. The `id` is read from the
/// experiment document itself rather than from here, so it is not repeated in this header.
#[derive(Debug, Deserialize)]
struct Header {
    kind: String,
}

/// The `kind: experiment` document, reduced to the two blocks telemetry reads.
#[derive(Debug, Deserialize)]
struct ExperimentDocView {
    id: String,
    #[serde(default)]
    telemetry: Option<TelemetryBlock>,
    #[serde(default)]
    results: Option<ResultsBlock>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TelemetryBlock {
    #[serde(default)]
    pub prometheus: Option<PrometheusBlock>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PrometheusBlock {
    #[serde(default)]
    pub scrape_targets: Vec<String>,
    /// Targets that are legitimately absent under some experiment arms (a burst driver's
    /// exporter that only exists while that arm is active, say) — a failed scrape here never
    /// counts against completeness. See ARCHITECTURE.md §6.
    #[serde(default)]
    pub optional_scrape_targets: Vec<String>,
}

/// One Prometheus scrape target, with whether a failure to reach it counts against
/// `IngestStats::completeness_pct()`.
#[derive(Debug, Clone)]
pub struct ScrapeTarget {
    pub address: String,
    pub optional: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ResultsBlock {
    #[serde(default)]
    pub artifact_dir: Option<String>,
}

impl WorkspaceView {
    /// Read a workspace directory: every `*.yaml` (non-recursive, as `vxedge-core` does — a
    /// `results/` archive containing YAML must not join the experiment) plus the `.vxp`.
    pub fn load(root: &Path) -> Result<Self, TelemetryError> {
        let bad = |msg: String| TelemetryError::Manifest(msg);
        if !root.is_dir() {
            return Err(bad(format!(
                "`{}` is not a directory. Telemetry reads a §6.0 experiment workspace — the \
                 folder holding `main.yaml`, the vehicle and the baseline — not a single \
                 manifest file.",
                root.display()
            )));
        }

        let mut names: Vec<PathBuf> = std::fs::read_dir(root)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("yaml") | Some("vxp")
                )
            })
            .collect();
        // Sorted so the content hash below is a property of the workspace rather than of the
        // order the filesystem happened to hand the entries back.
        names.sort();

        let mut files = Vec::new();
        let mut experiment: Option<ExperimentDocView> = None;
        let mut experiment_count = 0usize;
        let mut package: Option<PackageIdentity> = None;

        for path in &names {
            let bytes = std::fs::read(path)?;
            let name = path
                .file_name()
                .expect("read_dir yields named files")
                .to_string_lossy()
                .to_string();

            if path.extension().and_then(|e| e.to_str()) == Some("vxp") {
                package = serde_yaml_ng::from_slice::<PackageIdentity>(&bytes).ok();
                files.push((name, bytes));
                continue;
            }

            let header: Header =
                serde_yaml_ng::from_slice(&bytes).map_err(|e| bad(format!("`{name}`: {e}")))?;
            if header.kind == "experiment" {
                experiment_count += 1;
                experiment = Some(
                    serde_yaml_ng::from_slice(&bytes).map_err(|e| bad(format!("`{name}`: {e}")))?,
                );
            }
            files.push((name, bytes));
        }

        let experiment = match (experiment, experiment_count) {
            (Some(e), 1) => e,
            (_, 0) => {
                return Err(bad(format!(
                    "no `kind: experiment` document in `{}` — a workspace needs exactly one",
                    root.display()
                )));
            }
            (_, n) => {
                return Err(bad(format!(
                    "{n} `kind: experiment` documents in `{}` — a workspace needs exactly one",
                    root.display()
                )));
            }
        };

        Ok(WorkspaceView {
            root: root.to_path_buf(),
            experiment_id: experiment.id,
            package,
            files,
            telemetry: experiment.telemetry,
            results: experiment.results,
        })
    }

    pub fn scrape_targets(&self) -> Vec<ScrapeTarget> {
        let Some(p) = self.telemetry.as_ref().and_then(|t| t.prometheus.as_ref()) else {
            return Vec::new();
        };
        p.scrape_targets
            .iter()
            .map(|address| ScrapeTarget {
                address: address.clone(),
                optional: false,
            })
            .chain(
                p.optional_scrape_targets
                    .iter()
                    .map(|address| ScrapeTarget {
                        address: address.clone(),
                        optional: true,
                    }),
            )
            .collect()
    }

    pub fn artifact_dir(&self) -> Option<&str> {
        self.results.as_ref()?.artifact_dir.as_deref()
    }

    /// SHA-256 over the whole workspace: each file's name and bytes, in name order.
    ///
    /// This is what `manifest_sha256` used to be, and it answers the same question — "did every
    /// repetition in this campaign run from the same input?" — about a thing that actually
    /// exists. The §6.1 hash covered a document the daemon generated and nobody authored; two
    /// workspaces could differ in a vehicle file and hash identically if lowering flattened the
    /// difference away. Names are hashed alongside contents so renaming a file is a change.
    pub fn content_sha256(&self) -> String {
        let mut h = sha2::Sha256::new();
        for (name, bytes) in &self.files {
            h.update(name.as_bytes());
            h.update([0u8]);
            h.update(bytes);
            h.update([0u8]);
        }
        format!("{:x}", h.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vxtel-ws-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (n, c) in files {
            std::fs::write(root.join(n), c).unwrap();
        }
        root
    }

    const MAIN: &str = r#"
kind: experiment
id: exp-001
vehicles: [veh]
baseline: bl
network:
  slices:
    - { id: adas, mode: vlan, vlan: 101 }
telemetry:
  prometheus:
    scrape_targets: ["127.0.0.1:9801", "127.0.0.1:9802"]
results:
  artifact_dir: results/exp-001
"#;

    const VEHICLE: &str = "kind: vehicle\nid: veh\ndevices: {}\n";
    const VXP: &str = "id: exp-001\nversion: 0.1.0\nentry: main.yaml\n";

    #[test]
    fn reads_the_experiment_document_and_ignores_the_rest_of_the_workspace() {
        let root = workspace(
            "read",
            &[
                ("main.yaml", MAIN),
                ("vehicle.yaml", VEHICLE),
                ("exp-001.vxp", VXP),
            ],
        );
        let v = WorkspaceView::load(&root).unwrap();
        assert_eq!(v.experiment_id, "exp-001");
        assert_eq!(v.scrape_targets().len(), 2);
        assert_eq!(v.artifact_dir(), Some("results/exp-001"));
        assert_eq!(
            v.package,
            Some(PackageIdentity {
                id: "exp-001".into(),
                version: Some("0.1.0".into())
            })
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn optional_scrape_targets_are_flagged() {
        let main = MAIN.replace(
            "    scrape_targets: [\"127.0.0.1:9801\", \"127.0.0.1:9802\"]",
            "    scrape_targets: [\"127.0.0.1:9801\"]\n    optional_scrape_targets: [\"127.0.0.1:9902\"]",
        );
        let root = workspace(
            "optional",
            &[("main.yaml", &main), ("vehicle.yaml", VEHICLE)],
        );
        let targets = WorkspaceView::load(&root).unwrap().scrape_targets();
        assert_eq!(targets.len(), 2);
        assert!(!targets[0].optional && targets[0].address == "127.0.0.1:9801");
        assert!(targets[1].optional && targets[1].address == "127.0.0.1:9902");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_workspace_with_no_telemetry_block_has_no_targets() {
        let root = workspace("empty", &[("main.yaml", "kind: experiment\nid: e\n")]);
        assert!(
            WorkspaceView::load(&root)
                .unwrap()
                .scrape_targets()
                .is_empty()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The hash has to move when *anything* in the workspace moves — the whole point of hashing
    /// the workspace rather than one generated document is that a vehicle-file change is a
    /// change to what ran.
    #[test]
    fn the_content_hash_covers_every_file_not_just_the_experiment() {
        let root = workspace("hash", &[("main.yaml", MAIN), ("vehicle.yaml", VEHICLE)]);
        let before = WorkspaceView::load(&root).unwrap().content_sha256();
        assert_eq!(before.len(), 64);

        std::fs::write(
            root.join("vehicle.yaml"),
            "kind: vehicle\nid: veh\ndevices: { a: { role: ecu } }\n",
        )
        .unwrap();
        let after = WorkspaceView::load(&root).unwrap().content_sha256();
        assert_ne!(before, after, "a vehicle change left the hash unmoved");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Handed a §6.1 manifest — which is what every campaign script passed until this change —
    /// the error has to name the fix rather than report a parse failure ten frames down.
    #[test]
    fn a_file_instead_of_a_workspace_directory_says_so() {
        let root = workspace("file", &[("main.yaml", MAIN)]);
        let err = WorkspaceView::load(&root.join("main.yaml")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not a directory"), "{msg}");
        assert!(msg.contains("workspace"), "{msg}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_workspace_without_an_experiment_document_is_refused() {
        let root = workspace("noexp", &[("vehicle.yaml", VEHICLE)]);
        let msg = WorkspaceView::load(&root).unwrap_err().to_string();
        assert!(msg.contains("exactly one"), "{msg}");
        std::fs::remove_dir_all(&root).ok();
    }
}
