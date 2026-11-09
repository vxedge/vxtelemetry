//! **Log records** (`ARCHITECTURE.md` §12, decision X-18): the ingest turns the log files
//! `entities.json` names into `log` records, so a node's serial console, a service's output or a
//! traffic generator's report sits in the same run, on the same clock and under the same entity
//! paths, as its metrics.
//!
//! Each round reads what each file gained since the last, up to [`ROUND_CAP`] bytes, and appends
//! one record per complete line to `logs.jsonl`. A line still being written waits for the next
//! round. Where each file was read up to is kept in `logs.offsets.json`, so an ingest that is
//! restarted resumes instead of repeating. A file that shrinks was replaced or truncated and is
//! read again from its start.
//!
//! A record's `ts_ns` is the round that read the line, not the moment it was written: the files
//! carry no common clock, and the round is the bound the ingest can stand behind.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::records::Record;

/// The file log records are appended to.
pub const FILE: &str = "logs.jsonl";
/// Where each source was read up to.
pub const OFFSETS: &str = "logs.offsets.json";
/// The most one round reads from one file. A file growing faster than this per round is read
/// over several rounds; nothing is skipped.
pub const ROUND_CAP: u64 = 1 << 20;

/// One log file of one entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub entity: String,
    /// What the file is to its entity: `serial`, `console`, `stdout`, `traffic`, …
    pub stream: String,
    pub path: PathBuf,
}

/// Reads the sources round by round.
#[derive(Debug)]
pub struct Tailer {
    sources: Vec<Source>,
    offsets: BTreeMap<String, u64>,
    dir: PathBuf,
}

impl Tailer {
    /// The sources `entities.json` names for `dir`, from where an earlier ingest left them.
    pub fn load(dir: &Path, sources: Vec<Source>) -> Self {
        let offsets = std::fs::read(dir.join(OFFSETS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            sources,
            offsets,
            dir: dir.to_path_buf(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Read what every source gained, append it to `logs.jsonl` as records stamped `ts_ns`, and
    /// return how many lines were read. A source that cannot be read yet (not created, removed)
    /// is skipped this round.
    pub fn round(&mut self, ts_ns: i64) -> std::io::Result<u64> {
        let mut out = Vec::new();
        let mut lines = 0;
        for s in &self.sources {
            let key = s.path.to_string_lossy().into_owned();
            let from = self.offsets.get(&key).copied().unwrap_or(0);
            let Ok((read, next)) = read_lines(&s.path, from) else {
                continue;
            };
            for line in read {
                let rec = Record::Log {
                    ts_ns,
                    entity: s.entity.clone(),
                    stream: s.stream.clone(),
                    line,
                };
                serde_json::to_writer(&mut out, &rec).expect("serializable record");
                out.push(b'\n');
                lines += 1;
            }
            self.offsets.insert(key, next);
        }
        if !out.is_empty() {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(FILE))?
                .write_all(&out)?;
        }
        // Written after the records: a crash between the two repeats lines, never loses them.
        let tmp = self.dir.join(format!("{OFFSETS}.tmp"));
        std::fs::write(
            &tmp,
            serde_json::to_vec(&self.offsets).expect("serializable offsets"),
        )?;
        std::fs::rename(tmp, self.dir.join(OFFSETS))?;
        Ok(lines)
    }
}

/// The complete lines of `path` from byte `from`, at most [`ROUND_CAP`] bytes of them, and the
/// offset after the last one. A file shorter than `from` is read from 0. A run of
/// [`ROUND_CAP`] bytes with no newline is taken as one line, so one endless line cannot stall the
/// source.
fn read_lines(path: &Path, from: u64) -> std::io::Result<(Vec<String>, u64)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let from = if len < from { 0 } else { from };
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.take(ROUND_CAP).read_to_end(&mut buf)?;
    let complete = match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => i + 1,
        None if buf.len() as u64 == ROUND_CAP => buf.len(),
        None => 0,
    };
    let taken = &buf[..complete];
    let lines = match taken.strip_suffix(b"\n") {
        _ if taken.is_empty() => Vec::new(),
        Some(body) => body.split(|b| *b == b'\n').map(text).collect(),
        None => vec![text(taken)],
    };
    Ok((lines, from + complete as u64))
}

fn text(line: &[u8]) -> String {
    String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned()
}

/// The archive member that holds `entity`'s logs: `logs/<entity>.jsonl.zst`, each path segment
/// kept when it is a plain name and replaced by `_` otherwise, so no entity escapes `logs/`.
pub fn member(entity: &str) -> String {
    let segments: Vec<String> = entity
        .split('/')
        .map(|s| {
            let plain = !s.is_empty()
                && s != "."
                && s != ".."
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if plain { s.to_string() } else { "_".into() }
        })
        .collect();
    format!("logs/{}.jsonl.zst", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vxtel-logs-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn logged(dir: &Path) -> Vec<(String, String)> {
        std::fs::read_to_string(dir.join(FILE))
            .unwrap_or_default()
            .lines()
            .map(|l| match serde_json::from_str(l).unwrap() {
                Record::Log { entity, line, .. } => (entity, line),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn complete_lines_are_read_once_and_a_partial_line_waits() {
        let dir = tmp("partial");
        let serial = dir.join("vx-ecu1.serial.log");
        std::fs::write(&serial, "boot\r\nready\npart").unwrap();
        let src = vec![Source {
            entity: "car/front/ecu1".into(),
            stream: "serial".into(),
            path: serial.clone(),
        }];
        let mut t = Tailer::load(&dir, src.clone());
        assert_eq!(t.round(1).unwrap(), 2);
        assert_eq!(t.round(2).unwrap(), 0, "a partial line waits");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&serial)
            .unwrap()
            .write_all(b"ial\n\nlast\n")
            .unwrap();
        // A restarted ingest resumes where the last left off.
        let mut t = Tailer::load(&dir, src);
        assert_eq!(t.round(3).unwrap(), 3);
        let lines: Vec<String> = logged(&dir).into_iter().map(|(_, l)| l).collect();
        assert_eq!(lines, ["boot", "ready", "partial", "", "last"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shrunken_file_is_read_again_and_a_missing_one_waits() {
        let dir = tmp("shrink");
        let log = dir.join("svc-telemetry.log");
        let src = vec![Source {
            entity: "host/services/telemetry".into(),
            stream: "stdout".into(),
            path: log.clone(),
        }];
        let mut t = Tailer::load(&dir, src);
        assert_eq!(t.round(1).unwrap(), 0, "not created yet");
        std::fs::write(&log, "one\ntwo\n").unwrap();
        assert_eq!(t.round(2).unwrap(), 2);
        std::fs::write(&log, "new\n").unwrap();
        assert_eq!(t.round(3).unwrap(), 1);
        assert_eq!(logged(&dir).last().unwrap().1, "new");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_longer_than_a_round_is_cut_not_stuck() {
        let dir = tmp("long");
        let log = dir.join("x.log");
        let mut body = vec![b'a'; ROUND_CAP as usize + 10];
        body.push(b'\n');
        std::fs::write(&log, &body).unwrap();
        let (first, at) = read_lines(&log, 0).unwrap();
        assert_eq!((first.len(), at), (1, ROUND_CAP));
        let (rest, end) = read_lines(&log, at).unwrap();
        assert_eq!((rest[0].len(), end), (10, ROUND_CAP + 11));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn members_stay_under_logs() {
        assert_eq!(member("car/front/ecu1"), "logs/car/front/ecu1.jsonl.zst");
        assert_eq!(member("host"), "logs/host.jsonl.zst");
        assert_eq!(member("../../etc/x y"), "logs/_/_/etc/_.jsonl.zst");
    }
}
