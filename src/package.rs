//! **The package digest** (`ARCHITECTURE.md` §6.0.7, §13): the SHA-256 of a `vxp/1` package's
//! packed bytes, implemented here independently of `vxedge-core` and pinned to the same
//! packed-bytes test vectors both sides assert, as the workspace identity is.
//!
//! The packed form is an uncompressed ustar archive of exactly the `vxws/2` identity inputs, in
//! sorted path order. Every header is fixed: mode `0644`, uid and gid 0, empty user and group
//! names, mtime 0, type `0`, empty device numbers, and a checksum of six octal digits, NUL, space.
//! A path over 100 bytes is split at the **shortest** `/`-prefix (up to 155 bytes) that leaves a
//! name of up to 100 bytes. Each file's data is padded to 512 bytes and the archive ends with two
//! zero blocks, with no further padding. Nothing about the host reaches the bytes, so any host
//! computes the same digest, and `sha256sum` of a packed file gives it too.

use sha2::Digest;

use crate::TelemetryError;

const BLOCK: usize = 512;
const NAME_MAX: usize = 100;
const PREFIX_MAX: usize = 155;
/// The largest size ustar's 11 octal digits hold.
const SIZE_MAX: u64 = 0o77_777_777_777;

/// The package digest of `inputs`, as `sha256:<hex>`. `inputs` are the identity inputs in sorted
/// path order, as [`crate::identity::read_inputs`] returns them under `vxws/2`.
pub fn digest(inputs: &[(String, Vec<u8>)]) -> Result<String, TelemetryError> {
    let packed = pack(inputs)?;
    let hash = sha2::Sha256::digest(&packed);
    Ok(format!(
        "sha256:{}",
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

/// The packed bytes of `inputs`, which must be in sorted path order with each path once.
pub fn pack(inputs: &[(String, Vec<u8>)]) -> Result<Vec<u8>, TelemetryError> {
    let mut out = Vec::new();
    for (i, (path, bytes)) in inputs.iter().enumerate() {
        if i > 0 && inputs[i - 1].0 >= *path {
            return Err(refused(
                path,
                "entries must be in sorted path order, each once",
            ));
        }
        out.extend_from_slice(&header(path, bytes.len() as u64)?);
        out.extend_from_slice(bytes);
        let pad = (BLOCK - bytes.len() % BLOCK) % BLOCK;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out.extend(std::iter::repeat_n(0u8, 2 * BLOCK));
    Ok(out)
}

fn refused(path: &str, why: &str) -> TelemetryError {
    TelemetryError::Manifest(format!("`{path}` cannot be packed: {why}"))
}

/// ustar's `(prefix, name)` for `path`, or `None` when no split fits.
fn split(path: &str) -> Option<(&str, &str)> {
    if path.len() <= NAME_MAX {
        return Some(("", path));
    }
    // Scan the slashes left to right, so the first fit is the shortest prefix.
    for (at, _) in path.match_indices('/') {
        let (prefix, name) = (&path[..at], &path[at + 1..]);
        if prefix.len() > PREFIX_MAX {
            return None;
        }
        if !name.is_empty() && name.len() <= NAME_MAX {
            return Some((prefix, name));
        }
    }
    None
}

fn header(path: &str, size: u64) -> Result<[u8; BLOCK], TelemetryError> {
    let well_formed = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..");
    if !well_formed {
        return Err(refused(
            path,
            "paths are relative and `/`-separated, with no empty, `.` or `..` component",
        ));
    }
    let Some((prefix, name)) = split(path) else {
        return Err(refused(
            path,
            "ustar holds a name of up to 100 bytes after a `/`-split prefix of up to 155",
        ));
    };
    if size > SIZE_MAX {
        return Err(refused(path, "larger than ustar's size field holds"));
    }

    // Field offsets and widths from POSIX ustar.
    let mut h = [0u8; BLOCK];
    let mut put =
        |offset: usize, bytes: &[u8]| h[offset..offset + bytes.len()].copy_from_slice(bytes);
    put(0, name.as_bytes()); //                 name[100]
    put(100, b"0000644\0"); //                  mode[8]
    put(108, b"0000000\0"); //                  uid[8]
    put(116, b"0000000\0"); //                  gid[8]
    put(124, format!("{size:011o}\0").as_bytes()); // size[12]
    put(136, b"00000000000\0"); //              mtime[12]
    put(148, b"        "); //                   chksum[8], spaces while summing
    put(156, b"0"); //                          typeflag
    put(257, b"ustar\0"); //                    magic[6]
    put(263, b"00"); //                         version[2]
    // uname[32], gname[32], devmajor[8] and devminor[8] stay empty.
    put(345, prefix.as_bytes()); //             prefix[155]
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity;
    use std::fs;
    use std::path::PathBuf;

    fn workspace(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vxtel-pkg-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (n, c) in files {
            let p = root.join(n);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, c).unwrap();
        }
        root
    }

    fn entries(files: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = files
            .iter()
            .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
            .collect();
        v.sort();
        v
    }

    /// The packed-bytes **shared test vector**, over the `vxws/2` fixture with a `vxp/1`
    /// descriptor and a literal lock. `vxedge-core` asserts the same value; if it changes, both
    /// sides move together, and every published package digest moves with them.
    #[test]
    fn packed_bytes_shared_vector() {
        let root = workspace(
            "vector",
            &[
                (
                    "vxp.yaml",
                    "schema: vxp/1\nid: vxedge/id-test\nversion: 1.0.0\nentry: main.yaml\n",
                ),
                ("vxp.lock", "schema: vxp-lock/1\n"),
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
        let inputs = identity::read_inputs(&root, identity::VXWS2).unwrap();
        assert_eq!(pack(&inputs).unwrap().len(), (6 + 6 + 2) * BLOCK);
        assert_eq!(
            digest(&inputs).unwrap(),
            "sha256:1888e766b259786d5d065a4a7c85f3d3ddc6bb589a30cb257156b85e9155a5c6",
            "packed form drifted from vxedge-core's"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// The second **shared test vector**: a path over 100 bytes split at the shortest prefix, a
    /// name of exactly 100 bytes, an empty file and a file of exactly one block. `vxedge-core`
    /// asserts the same value.
    #[test]
    fn packed_bytes_split_vector() {
        let long = format!(
            "parts/{}/{}/{}/file.txt",
            "a".repeat(40),
            "b".repeat(40),
            "c".repeat(40)
        );
        let exact = format!("parts/{}", "d".repeat(94));
        let block = "x".repeat(BLOCK);
        let inputs = entries(&[
            (
                "vxp.yaml",
                "schema: vxp/1\nid: vxedge/split-test\nversion: 1.0.0\nentry: main.yaml\n",
            ),
            ("vxp.lock", "schema: vxp-lock/1\n"),
            ("main.yaml", "kind: experiment\nid: split-test\n"),
            (&long, "split at the shortest prefix\n"),
            (&exact, "a name of exactly 100 bytes\n"),
            ("parts/empty", ""),
            ("parts/block", &block),
        ]);
        assert_eq!(pack(&inputs).unwrap().len(), (7 + 6 + 2) * BLOCK);
        assert_eq!(
            digest(&inputs).unwrap(),
            "sha256:c1e0906cd96c16c02c3bdc814982767c582573d4b3707cc56f20048c6d1c76b3",
            "packed form drifted from vxedge-core's"
        );
    }

    #[test]
    fn the_shortest_prefix_wins_and_an_unsplittable_path_is_refused() {
        // Both `a…` and `a…/b…` would fit as the prefix; the shorter one is taken.
        let path = format!("{}/{}/{}", "a".repeat(30), "b".repeat(40), "c".repeat(40));
        assert_eq!(split(&path).unwrap().0, "a".repeat(30));
        assert!(split(&"x".repeat(101)).is_none(), "no slash to split at");
        let long_prefix = format!("{}/{}", "p".repeat(156), "n");
        assert!(split(&long_prefix).is_none(), "prefix over 155 bytes");
        let err = digest(&entries(&[(&"x".repeat(101), "")])).unwrap_err();
        assert!(err.to_string().contains("100 bytes"), "{err}");
    }

    #[test]
    fn unsorted_duplicate_or_malformed_paths_are_refused() {
        let unsorted = vec![("b".to_string(), vec![]), ("a".to_string(), vec![])];
        assert!(pack(&unsorted).is_err());
        let twice = vec![("a".to_string(), vec![]), ("a".to_string(), vec![])];
        assert!(pack(&twice).is_err());
        for bad in ["", "/abs", "a//b", "./a", "a/../b", "a\\b"] {
            assert!(pack(&[(bad.to_string(), vec![])]).is_err(), "{bad:?}");
        }
    }

    /// A standard tool reads the bytes: GNU tar lists the entries as regular files, 0644, 0/0.
    #[test]
    fn tar_reads_the_packed_form() {
        let inputs = entries(&[("main.yaml", "x"), ("a/b/c.txt", "y")]);
        let file = std::env::temp_dir().join(format!("vxtel-pkg-{}.vxp", std::process::id()));
        fs::write(&file, pack(&inputs).unwrap()).unwrap();
        let Ok(out) = std::process::Command::new("tar")
            .arg("-tvf")
            .arg(&file)
            .output()
        else {
            return; // no tar on this host
        };
        fs::remove_file(&file).ok();
        let listing = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = listing
            .lines()
            .filter_map(|l| l.split_whitespace().last())
            .collect();
        assert_eq!(names, ["a/b/c.txt", "main.yaml"], "{listing}");
        assert!(
            listing.lines().all(|l| l.starts_with("-rw-r--r-- 0/0")),
            "{listing}"
        );
    }
}
