//! Partial, tolerant view of a §6.0 experiment **workspace**: only the keys telemetry needs.
//!
//! This replaces the §6.1 `ManifestView` and is the change that cuts the last tie to §6.1 —
//! `vxedge-core`'s `lower.rs` existed only so the daemon could hand this tool a §6.1 file to
//! read.
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
use std::path::{Path, PathBuf};

use crate::TelemetryError;

/// The `vxp/1` package descriptor's fixed name; the same string as in `vxedge-core`.
pub const DESCRIPTOR_FILE: &str = "vxp.yaml";

/// A loaded workspace, reduced to what telemetry acts on.
#[derive(Debug)]
pub struct WorkspaceView {
    pub root: PathBuf,
    pub experiment_id: String,
    /// The `.vxp` package descriptor's identity, when the workspace has one.
    pub package: Option<PackageIdentity>,
    /// Every file the identity covers, `(relative path, bytes)` in sorted path order: the
    /// documents plus the sandbox's `parts/`, `assets/`, `scripts/` and `tools/`, without what
    /// [`crate::identity`] excludes. Read once, so the archive ships the bytes that were hashed.
    pub inputs: Vec<(String, Vec<u8>)>,
    /// The workspace identity under [`crate::identity::CURRENT`], over `inputs`. See
    /// [`WorkspaceView::content_sha256`].
    identity: String,
    telemetry: Option<TelemetryBlock>,
    results: Option<ResultsBlock>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct PackageIdentity {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    /// The package digest (`ARCHITECTURE.md` §6.0.7), computed here from the identity inputs for
    /// a `vxp/1` package. Never read from the descriptor.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Whether the daemon checked the package's lock at `up`, as it told `collect`. `None` when
    /// it said nothing: this tool never derives it.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// The descriptor's `schema`: `vxp/1`.
    #[serde(default, skip_serializing)]
    pub schema: Option<String>,
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
    /// `results/` archive containing YAML must not join the experiment) plus the descriptor,
    /// `vxp.yaml`. A legacy `<name>.vxp` descriptor is refused, as vxedge refuses it.
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
        names.sort();

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

            // A legacy `<name>.vxp` descriptor is no longer read (decision X-30).
            if path.extension().and_then(|e| e.to_str()) == Some("vxp") {
                return Err(bad(format!(
                    "`{name}` in `{}` is a legacy `<name>.vxp` descriptor, which is no longer \
                     read; the package format is `vxp.yaml` with `schema: vxp/1` (vxedge \
                     `pkg lock` writes its lock)",
                    root.display()
                )));
            }
            // The descriptor: `vxp.yaml` (`ARCHITECTURE.md` §6.0.3). It is not a document, so it
            // never reaches the `kind` parse below.
            if name == DESCRIPTOR_FILE {
                package = serde_yaml_ng::from_slice::<PackageIdentity>(&bytes).ok();
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

        let inputs = crate::identity::read_inputs(root, crate::identity::CURRENT)?;
        let identity = crate::identity::digest(crate::identity::CURRENT, &inputs)?;
        if let Some(p) = package
            .as_mut()
            .filter(|p| p.schema.as_deref() == Some("vxp/1"))
        {
            p.digest = Some(crate::package::digest(&inputs)?);
        }

        Ok(WorkspaceView {
            root: root.to_path_buf(),
            experiment_id: experiment.id,
            package,
            inputs,
            identity,
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

    /// **The workspace identity**, per `ARCHITECTURE.md` §13, under
    /// [`WorkspaceView::identity_algorithm`]. The rule lives in [`crate::identity`].
    ///
    /// It answers "did every repetition of this campaign run from the same input?" about a
    /// thing that exists. `manifest_sha256` hashed a document the daemon generated and nobody
    /// authored, so two workspaces differing in a vehicle file could hash identically if
    /// lowering flattened the difference away.
    pub fn content_sha256(&self) -> String {
        self.identity.clone()
    }

    /// The algorithm behind [`WorkspaceView::content_sha256`]. Recorded beside it, because
    /// identities under different algorithms are not comparable.
    pub fn identity_algorithm(&self) -> &'static str {
        crate::identity::CURRENT
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
    const VXP: &str = "schema: vxp/1\nid: vxedge/exp-001\nversion: 0.1.0\nentry: main.yaml\n";

    #[test]
    fn reads_the_experiment_document_and_ignores_the_rest_of_the_workspace() {
        let root = workspace(
            "read",
            &[
                ("main.yaml", MAIN),
                ("vehicle.yaml", VEHICLE),
                ("vxp.yaml", VXP),
            ],
        );
        let v = WorkspaceView::load(&root).unwrap();
        assert_eq!(v.experiment_id, "exp-001");
        assert_eq!(v.scrape_targets().len(), 2);
        assert_eq!(v.artifact_dir(), Some("results/exp-001"));
        let p = v.package.expect("vxp.yaml names the package");
        assert_eq!(p.id, "vxedge/exp-001");
        assert!(p.digest.is_some(), "a vxp/1 package has a digest");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Decision X-30: a legacy `<name>.vxp` descriptor is refused, with the current format named.
    #[test]
    fn a_legacy_descriptor_is_refused() {
        let root = workspace(
            "legacy",
            &[
                ("main.yaml", MAIN),
                ("vehicle.yaml", VEHICLE),
                (
                    "exp-001.vxp",
                    "id: exp-001\nversion: 0.1.0\nentry: main.yaml\n",
                ),
            ],
        );
        let e = WorkspaceView::load(&root).unwrap_err().to_string();
        assert!(
            e.contains("legacy `<name>.vxp` descriptor") && e.contains("vxp.yaml"),
            "{e}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A `vxp/1` package (vxedge 0.6.0): `vxp.yaml` is the descriptor, not a document, and it
    /// names the package the archive records.
    #[test]
    fn a_vxp_yaml_descriptor_is_read_as_the_package_not_as_a_document() {
        let root = workspace(
            "vxp-yaml",
            &[
                ("main.yaml", MAIN),
                ("vehicle.yaml", VEHICLE),
                (
                    "vxp.yaml",
                    "schema: vxp/1\nid: vxedge/exp-001\nversion: 1.2.0\nentry: main.yaml\n\
                     x-oem: { a: 1 }\n",
                ),
            ],
        );
        let v = WorkspaceView::load(&root).unwrap();
        assert_eq!(v.experiment_id, "exp-001");
        assert_eq!(
            v.package,
            Some(PackageIdentity {
                id: "vxedge/exp-001".into(),
                version: Some("1.2.0".into()),
                digest: Some(crate::package::digest(&v.inputs).unwrap()),
                locked: None,
                schema: Some("vxp/1".into()),
            })
        );
        let recorded = serde_json::to_value(v.package.as_ref().unwrap()).unwrap();
        assert!(recorded["digest"].as_str().unwrap().starts_with("sha256:"));
        assert!(recorded.get("locked").is_none(), "never guessed");
        assert!(recorded.get("schema").is_none());
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

    /// What the identity covers. The shared test vectors that pin its exact values live with
    /// the rule, in `crate::identity`.
    #[test]
    fn the_workspace_identity_covers_what_ran() {
        let root = workspace(
            "identity",
            &[("main.yaml", "kind: experiment\nid: id-test\n")],
        );
        std::fs::create_dir_all(root.join("parts")).unwrap();
        std::fs::write(root.join("parts/local-thing"), "payload\n").unwrap();
        let view = WorkspaceView::load(&root).unwrap();
        assert_eq!(view.identity_algorithm(), "vxws/2");
        let id = view.content_sha256();

        // Outputs are not inputs: an archive appearing must not restate the identity of the run
        // that produced it.
        std::fs::create_dir_all(root.join("results")).unwrap();
        std::fs::write(root.join("results/rep1.zip"), "not an input").unwrap();
        assert_eq!(WorkspaceView::load(&root).unwrap().content_sha256(), id);

        // A local part is part of what ran.
        std::fs::write(root.join("parts/local-thing"), "different\n").unwrap();
        assert_ne!(WorkspaceView::load(&root).unwrap().content_sha256(), id);
        std::fs::remove_dir_all(&root).ok();
    }

    /// G2: version-control metadata and editor droppings are not the experiment.
    #[test]
    fn vcs_metadata_and_editor_files_do_not_change_the_identity() {
        let root = workspace("noise", &[("main.yaml", "kind: experiment\nid: id-test\n")]);
        let before = WorkspaceView::load(&root).unwrap().content_sha256();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join("main.yaml~"), "backup").unwrap();
        std::fs::write(root.join(".main.yaml.swp"), "swap").unwrap();
        assert_eq!(WorkspaceView::load(&root).unwrap().content_sha256(), before);
        std::fs::remove_dir_all(&root).ok();
    }

    /// G2: a root `.vxpignore` excludes what it names, and is itself an input.
    #[test]
    fn vxpignore_excludes_what_it_names_and_is_itself_hashed() {
        let root = workspace(
            "vxpignore",
            &[
                ("main.yaml", "kind: experiment\nid: id-test\n"),
                (".vxpignore", "scratch/\n*.log\n"),
            ],
        );
        let before = WorkspaceView::load(&root).unwrap().content_sha256();
        std::fs::create_dir_all(root.join("scratch")).unwrap();
        std::fs::write(root.join("scratch/junk"), "local build output").unwrap();
        std::fs::write(root.join("build.log"), "noise").unwrap();
        assert_eq!(WorkspaceView::load(&root).unwrap().content_sha256(), before);
        std::fs::write(root.join(".vxpignore"), "scratch/\n").unwrap();
        assert_ne!(WorkspaceView::load(&root).unwrap().content_sha256(), before);
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
