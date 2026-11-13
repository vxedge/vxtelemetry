//! What a results archive records and what checking one found: the reproducibility metadata
//! (§13: experiment ID, workspace identity, tool version, and — when the caller opts in via
//! `--record-image` — container image digests), and the outcome of `verify`. The archive itself
//! is written and read by [`crate::archive`].

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::ingest::IngestStats;
use crate::workspace_view::{PackageIdentity, WorkspaceView};

#[derive(Serialize)]
pub struct ArchiveMetadata {
    /// The archive format: `vxarchive/2`.
    pub archive: String,
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
    /// The trial this archive is, as the campaign runner wrote it to the ingest folder's
    /// `trial.json`: campaign id, trial id, arm, repetition, parameter values. Absent outside a
    /// campaign. Recorded as given, so the runner owns its shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial: Option<serde_json::Value>,
    /// The tool binaries the run's daemon runs, as it wrote them to the ingest folder's
    /// `tools.json`: per tool its `name`, `path`, `version`, `sha256` and `profile`. Absent when
    /// the daemon wrote none. Recorded as given, so the daemon owns its shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<serde_json::Value>,
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

/// The metadata an archive of `workspace` and `ingest_dir` records.
pub(crate) fn metadata_for(
    workspace: &WorkspaceView,
    ingest_dir: &Path,
    image_digests: Option<BTreeMap<String, String>>,
) -> ArchiveMetadata {
    // Phase markers that came after the ingest's last round (teardown) belong in this archive.
    let _ = crate::phases::drain(ingest_dir);
    let trial = std::fs::read(ingest_dir.join(TRIAL_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .filter(serde_json::Value::is_object);
    let tools = std::fs::read(ingest_dir.join(TOOLS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .filter(serde_json::Value::is_array);
    let ingest_stats: Option<IngestStats> = std::fs::read(ingest_dir.join("ingest-stats.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    ArchiveMetadata {
        archive: crate::archive::SCHEMA.to_string(),
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
        trial,
        tools,
    }
}

/// The file in the ingest folder a campaign runner writes the trial's identity to.
pub const TRIAL_FILE: &str = "trial.json";

/// The file in the ingest folder the daemon writes the provenance of its tool binaries to.
pub const TOOLS_FILE: &str = "tools.json";

/// What [`crate::archive::verify`] found: whether the members match the index, what could be
/// said about the recorded workspace identity, and the package digest check when the archive
/// records one.
#[derive(Debug, Serialize)]
pub struct Verification {
    /// Every check passed, the identity check among them. False when the identity is not
    /// checkable, even if nothing failed: see [`Verification::exit_code`].
    pub ok: bool,
    /// Every member matches `index.json`, and nothing is added or missing.
    pub index: bool,
    pub identity: IdentityCheck,
    /// The identity algorithm the archive records; absent when it records none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
    /// The recorded `workspace_sha256`; absent when the archive records none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded: Option<String>,
    /// The identity the archived workspace re-hashes to; absent when it is not checkable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computed: Option<String>,
    /// Files under `package/`.
    pub files: usize,
    /// Present when `metadata.json` records `package.digest` (a `vxp/1` package).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_digest: Option<DigestCheck>,
}

/// The identity outcome: re-hashed and equal, re-hashed and different, or not checkable, with the
/// reason. `verify` never assumes an algorithm the archive does not record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityCheck {
    pub outcome: IdentityOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityOutcome {
    Checked,
    Mismatch,
    NotCheckable,
}

/// `vxtelemetry verify`'s exit code when every check that could run passed but the identity is
/// not checkable. Not 2, which is the usage-error code.
pub const EXIT_NOT_CHECKABLE: i32 = 3;

impl Verification {
    fn package_ok(&self) -> bool {
        self.package_digest
            .as_ref()
            .is_none_or(|d| d.recorded == d.computed)
    }

    /// 0: verified. 1: the index, the identity or the package digest does not match.
    /// [`EXIT_NOT_CHECKABLE`]: nothing failed, but the identity is not checkable.
    pub fn exit_code(&self) -> i32 {
        if self.ok {
            0
        } else if self.index
            && self.identity.outcome == IdentityOutcome::NotCheckable
            && self.package_ok()
        {
            EXIT_NOT_CHECKABLE
        } else {
            1
        }
    }

    /// One line for a person: what passed, what failed, or why the identity is not checkable.
    pub fn summary(&self) -> String {
        let mut parts = vec![if self.index {
            "index OK".to_string()
        } else {
            "index MISMATCH".to_string()
        }];
        parts.push(match self.identity.outcome {
            IdentityOutcome::Checked => "identity OK".to_string(),
            IdentityOutcome::Mismatch => "identity MISMATCH".to_string(),
            IdentityOutcome::NotCheckable => format!(
                "identity not checkable: {}",
                self.identity.reason.as_deref().unwrap_or("no reason given")
            ),
        });
        if self.package_digest.is_some() {
            parts.push(if self.package_ok() {
                "package digest OK".into()
            } else {
                "package digest MISMATCH".into()
            });
        }
        parts.join("; ")
    }
}

#[derive(Debug, Serialize)]
pub struct DigestCheck {
    pub recorded: String,
    pub computed: String,
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
            archive: "vxarchive/2".into(),
            experiment_id: "t".into(),
            workspace_sha256: "0".repeat(64),
            workspace_identity_algorithm: "vxws/2".into(),
            package: None,
            collected_at_unix_ms: 0,
            vxtelemetry_version: "0".into(),
            ingest: None,
            image_digests: None,
            kernel: None,
            trial: None,
            tools: None,
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert!(json.get("image_digests").is_none());
        assert!(json.get("tools").is_none());

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

    #[test]
    fn a_realtime_kernel_is_read_from_sysfs_first() {
        assert!(is_preempt_rt(Some("1\n"), ""));
        assert!(!is_preempt_rt(Some("0\n"), "#1 SMP PREEMPT_RT"));
        assert!(is_preempt_rt(None, "#1 SMP PREEMPT_RT Thu"));
        assert!(!is_preempt_rt(None, "#1 SMP PREEMPT_DYNAMIC"));
    }
}
