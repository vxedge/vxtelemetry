//! **The workspace identity** (`ARCHITECTURE.md` §13), implemented here independently of
//! `vxedge-core` and pinned to the same test vectors both sides assert. Two implementations of
//! one rule stay honest by agreeing on answers, not by sharing code.
//!
//! - `vxws/1`: every file under the root, recursively, except `results/`. Archives written
//!   before `vxws/2` carry this value with no algorithm key; it stays computable so they stay
//!   verifiable.
//! - `vxws/2` (current): as `vxws/1`, and also skips `.git` (file or directory, at any depth),
//!   [`DEFAULT_EXCLUDES`], and whatever a root `.vxpignore` names in `.gitignore` syntax.
//!   `results/` and `.git` are excluded before `.vxpignore` is read, so they cannot be
//!   re-included; `.vxpignore` itself is hashed. The preimage starts with `vxws/2\0`, so the
//!   two algorithms never produce the same value.
//!
//! Each file contributes its `/`-separated relative path, NUL, its bytes, NUL, in sorted path
//! order. The matcher is built from the workspace alone, never from global or environment
//! ignore rules: an identity that depended on the machine would not identify anything.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sha2::Digest;

use crate::TelemetryError;

/// The file at the workspace root that lists further exclusions.
pub const IGNORE_FILE: &str = ".vxpignore";

/// What `vxws/2` always excludes besides `results/` and `.git`. Byte-identical in `vxedge-core`.
pub const DEFAULT_EXCLUDES: &[&str] = &["*.swp", "*.swo", "*~", ".#*", "#*#", ".DS_Store"];

pub const VXWS1: &str = "vxws/1";
pub const VXWS2: &str = "vxws/2";
/// The algorithm every new identity is computed with.
pub const CURRENT: &str = VXWS2;

/// The identity of the workspace at `root` under `algorithm` (`vxws/1` or `vxws/2`), as
/// lower-case hex.
pub fn compute(root: &Path, algorithm: &str) -> Result<String, TelemetryError> {
    let inputs = read_inputs(root, algorithm)?;
    digest(algorithm, &inputs)
}

/// Every file `algorithm` covers under `root`, as `(relative path, bytes)` in sorted path order.
/// The archive carries exactly these, so the bytes hashed are the bytes shipped.
pub fn read_inputs(root: &Path, algorithm: &str) -> Result<Vec<(String, Vec<u8>)>, TelemetryError> {
    let rules = match algorithm {
        VXWS1 => None,
        VXWS2 => Some(rules(root)?),
        other => return Err(unknown(other)),
    };
    let mut files = Vec::new();
    walk(root, root, rules.as_ref(), &mut files)?;
    files.sort();
    files
        .into_iter()
        .map(|(rel, path)| Ok((rel, std::fs::read(&path)?)))
        .collect()
}

/// The identity of an already-selected input set: no exclusion is applied, every entry counts.
/// `inputs` must be in sorted path order, as [`read_inputs`] returns them.
pub fn digest(algorithm: &str, inputs: &[(String, Vec<u8>)]) -> Result<String, TelemetryError> {
    let mut h = sha2::Sha256::new();
    match algorithm {
        VXWS1 => {}
        VXWS2 => {
            h.update(VXWS2.as_bytes());
            h.update([0u8]);
        }
        other => return Err(unknown(other)),
    }
    for (rel, bytes) in inputs {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(bytes);
        h.update([0u8]);
    }
    // sha2 0.11 returns a `hybrid_array::Array`, which has no `LowerHex`; spell the hex out.
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn unknown(algorithm: &str) -> TelemetryError {
    TelemetryError::Manifest(format!(
        "unknown workspace identity algorithm `{algorithm}`; this build knows {VXWS1} and {VXWS2}"
    ))
}

fn rules(root: &Path) -> Result<Gitignore, TelemetryError> {
    let ignore_file = root.join(IGNORE_FILE);
    let bad = |e: String| TelemetryError::Manifest(format!("`{}`: {e}", ignore_file.display()));
    let mut b = GitignoreBuilder::new(root);
    for pattern in DEFAULT_EXCLUDES {
        b.add_line(None, pattern).map_err(|e| bad(e.to_string()))?;
    }
    if ignore_file.is_file()
        && let Some(e) = b.add(&ignore_file)
    {
        return Err(bad(e.to_string()));
    }
    b.build().map_err(|e| bad(e.to_string()))
}

fn walk(
    base: &Path,
    dir: &Path,
    rules: Option<&Gitignore>,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), TelemetryError> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let rel = path
            .strip_prefix(base)
            .expect("walked path under base")
            .to_string_lossy()
            .replace('\\', "/");
        let is_dir = path.is_dir();
        if is_dir && rel == "results" {
            continue;
        }
        if let Some(rules) = rules {
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            // Excluded directories are not entered, so nothing under them can be re-included.
            if rules.matched(Path::new(&rel), is_dir).is_ignore() {
                continue;
            }
        }
        if is_dir {
            walk(base, &path, rules, out)?;
        } else {
            out.push((rel, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vxtel-id-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (n, c) in files {
            let p = root.join(n);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, c).unwrap();
        }
        root
    }

    /// The frozen `vxws/1` **shared test vector**: `vxedge-core` asserts the same value.
    #[test]
    fn vxws1_matches_the_shared_vector() {
        let root = workspace(
            "v1",
            &[
                ("main.yaml", "kind: experiment\nid: id-test\n"),
                ("parts/local-thing", "payload\n"),
                ("scripts/run.sh", "#!/usr/bin/env bash\n"),
            ],
        );
        assert_eq!(
            compute(&root, VXWS1).unwrap(),
            "f095a9facfabea97828ac5ee48a0da5516bfe036b546f612bcf182856db0efac"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// The `vxws/2` **shared test vector**, over a fixture that exercises every exclusion.
    /// `vxedge-core` asserts the same value; if it changes, both sides move together.
    #[test]
    fn vxws2_matches_the_shared_vector() {
        let root = workspace(
            "v2",
            &[
                ("main.yaml", "kind: experiment\nid: id-test\n"),
                ("parts/local-thing", "payload\n"),
                ("scripts/run.sh", "#!/usr/bin/env bash\n"),
                (".vxpignore", "scratch/\n"),
                ("scratch/junk", "local build output\n"),
                (".git/HEAD", "ref: refs/heads/main\n"),
                ("main.yaml~", "backup\n"),
                ("results/rep1.zip", "not an input"),
            ],
        );
        assert_eq!(
            compute(&root, VXWS2).unwrap(),
            "ddd70ed261fc314c991d00961eda8c4661e5881685f8f7febbd05c151cd39090",
            "identity rule drifted from vxedge-core's"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_git_file_and_nested_git_dirs_are_excluded() {
        let root = workspace("gitfile", &[("main.yaml", "kind: experiment\nid: e\n")]);
        let before = compute(&root, VXWS2).unwrap();
        fs::write(root.join(".git"), "gitdir: /elsewhere\n").unwrap();
        fs::create_dir_all(root.join("parts/vendored/.git")).unwrap();
        fs::write(root.join("parts/vendored/.git/HEAD"), "x").unwrap();
        assert_eq!(compute(&root, VXWS2).unwrap(), before);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn vxpignore_cannot_reinclude_results() {
        let root = workspace(
            "reinclude",
            &[
                ("main.yaml", "kind: experiment\nid: e\n"),
                (".vxpignore", "!results/\n"),
            ],
        );
        let before = compute(&root, VXWS2).unwrap();
        fs::create_dir_all(root.join("results")).unwrap();
        fs::write(root.join("results/rep1.zip"), "x").unwrap();
        assert_eq!(compute(&root, VXWS2).unwrap(), before);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_unknown_algorithm_is_refused() {
        let root = workspace("unknown", &[("main.yaml", "kind: experiment\nid: e\n")]);
        assert!(compute(&root, "vxws/9").is_err());
        fs::remove_dir_all(&root).ok();
    }
}
