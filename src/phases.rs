//! Phase markers (`ARCHITECTURE.md` §11): when a trial entered provision, warmup, measure,
//! cooldown or teardown.
//!
//! The campaign runner appends one `phase` record per line to [`FILE`] in the ingest folder, as it
//! moves a trial on. [`drain`] moves the complete lines it has not moved yet into `records.jsonl`,
//! so this tool stays the one writer of the records a reader follows: ingest drains every round,
//! and `collect` drains once more before it archives, for the markers that came after the last
//! round, such as teardown. Each marker keeps the time the runner gave it.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::records::Record;

/// The file the campaign runner appends phase records to.
pub const FILE: &str = "phases.jsonl";
/// How far [`FILE`] has been moved into `records.jsonl`, in bytes.
pub const OFFSET: &str = "phases.offset";

/// Move the complete phase lines of `dir`'s [`FILE`] not moved yet into `records.jsonl`, and
/// return how many were moved. A line that is not a `phase` record is skipped; an incomplete last
/// line waits for the next drain.
pub fn drain(dir: &Path) -> std::io::Result<u64> {
    let mut file = match std::fs::File::open(dir.join(FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let from: u64 = std::fs::read_to_string(dir.join(OFFSET))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    file.seek(SeekFrom::Start(from))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let Some(end) = text.rfind('\n') else {
        return Ok(0);
    };
    let mut out = String::new();
    let mut moved = 0;
    for line in text[..end].lines() {
        if let Ok(rec @ Record::Phase { .. }) = serde_json::from_str::<Record>(line.trim()) {
            out.push_str(&serde_json::to_string(&rec).expect("serializable record"));
            out.push('\n');
            moved += 1;
        }
    }
    if !out.is_empty() {
        let mut records = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("records.jsonl"))?;
        records.write_all(out.as_bytes())?;
    }
    let tmp = dir.join(format!("{OFFSET}.tmp"));
    std::fs::write(&tmp, (from + end as u64 + 1).to_string())?;
    std::fs::rename(tmp, dir.join(OFFSET))?;
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_phase_lines_move_once_and_a_partial_line_waits() {
        let d = &std::env::temp_dir().join(format!("vxtel-phases-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(d);
        std::fs::create_dir_all(d).unwrap();
        assert_eq!(drain(d).unwrap(), 0);
        std::fs::write(
            d.join(FILE),
            "{\"record\":\"phase\",\"ts_ns\":5,\"trial\":\"r1-idle\",\"phase\":\"warmup\"}\n\
             {\"record\":\"sample\",\"ts_ns\":6,\"entity\":\"host\",\"metric\":\"x\",\"value\":1}\n\
             {\"record\":\"phase\",\"ts_ns\":7,\"trial\":\"r1-idle\",\"pha",
        )
        .unwrap();
        assert_eq!(drain(d).unwrap(), 1);
        assert_eq!(drain(d).unwrap(), 0);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(d.join(FILE))
            .unwrap();
        f.write_all(b"se\":\"measure\"}\n").unwrap();
        assert_eq!(drain(d).unwrap(), 1);
        let records = std::fs::read_to_string(d.join("records.jsonl")).unwrap();
        let phases: Vec<Record> = records
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(matches!(&phases[0], Record::Phase { ts_ns: 5, phase, .. } if phase == "warmup"));
        assert!(matches!(&phases[1], Record::Phase { ts_ns: 7, phase, .. } if phase == "measure"));
        std::fs::remove_dir_all(d).ok();
    }
}
