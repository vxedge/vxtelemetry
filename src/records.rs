//! **Telemetry records** (`ARCHITECTURE.md` §12): one JSON object per line, the same live and in
//! an archive. Each names the entity it is about, by the path `vxedged` derived from the model
//! and wrote to `entities.json`; a target the list does not bind belongs to `host`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One record. `record` names its shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    /// One value of one series at one time.
    Sample {
        ts_ns: i64,
        entity: String,
        metric: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        labels: BTreeMap<String, String>,
        value: f64,
    },
    /// A span with no data from an entity: a renderer draws a break, never a line across it.
    Gap {
        entity: String,
        /// The series, when the gap is one series; absent for a whole failed scrape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metric: Option<String>,
        from_ns: i64,
        to_ns: i64,
        reason: String,
    },
    /// A trial entering a phase (provision, warmup, measure, cooldown, teardown).
    Phase {
        ts_ns: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trial: Option<String>,
        phase: String,
    },
    /// One line of one of an entity's log files (`crate::logs`), stamped with the round that
    /// read it. Kept in `logs.jsonl`, apart from the metrics.
    Log {
        ts_ns: i64,
        entity: String,
        /// What the file is to its entity: `serial`, `console`, `stdout`, `traffic`, …
        stream: String,
        line: String,
    },
}

/// The entity path of the host, and of any target no entity claims.
pub const HOST: &str = "host";

/// Which entity each scrape target belongs to, as `entities.json` binds them.
#[derive(Debug, Clone, Default)]
pub struct Entities {
    by_target: BTreeMap<String, String>,
    logs: Vec<crate::logs::Source>,
}

impl Entities {
    /// Read `entities.json` from `dir`. A folder without one binds every target to `host` and
    /// names no logs. A log path that is relative is taken from `dir`.
    pub fn load(dir: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(dir.join("entities.json")) else {
            return Self::default();
        };
        let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            return Self::default();
        };
        let (mut by_target, mut logs) = (BTreeMap::new(), Vec::new());
        for e in &list {
            let Some(path) = e["path"].as_str() else {
                continue;
            };
            for t in e["scrape_targets"].as_array().into_iter().flatten() {
                if let Some(t) = t.as_str() {
                    by_target.insert(t.to_string(), path.to_string());
                }
            }
            for l in e["logs"].as_array().into_iter().flatten() {
                let (Some(stream), Some(file)) = (l["stream"].as_str(), l["path"].as_str()) else {
                    continue;
                };
                logs.push(crate::logs::Source {
                    entity: path.to_string(),
                    stream: stream.to_string(),
                    path: dir.join(file),
                });
            }
        }
        Self { by_target, logs }
    }

    /// Every log file the list names, with its entity.
    pub fn logs(&self) -> &[crate::logs::Source] {
        &self.logs
    }

    /// Every target the list binds.
    pub fn targets(&self) -> impl Iterator<Item = &str> {
        self.by_target.keys().map(String::as_str)
    }

    pub fn of(&self, target: &str) -> &str {
        self.by_target.get(target).map_or(HOST, String::as_str)
    }
}

/// A Prometheus label block (`{a="b",c="d"}`) as a map. Escapes `\"`, `\\` and `\n` are read;
/// a malformed block yields what parsed before the fault.
pub fn parse_labels(block: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let inner = block
        .trim()
        .strip_prefix('{')
        .and_then(|b| b.strip_suffix('}'))
        .unwrap_or("");
    let mut chars = inner.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let name: String = chars.by_ref().take_while(|c| *c != '=').collect();
        if name.is_empty() || chars.next() != Some('"') {
            return out;
        }
        let mut value = String::new();
        loop {
            match chars.next() {
                Some('\\') => match chars.next() {
                    Some('n') => value.push('\n'),
                    Some(c) => value.push(c),
                    None => return out,
                },
                Some('"') => break,
                Some(c) => value.push(c),
                None => return out,
            }
        }
        out.insert(name.trim().to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_tagged_json_lines() {
        let s = Record::Sample {
            ts_ns: 1,
            entity: "car/front/ecu1".into(),
            metric: "vxprobe_rtt_seconds".into(),
            labels: BTreeMap::from([("quantile".to_string(), "0.99".to_string())]),
            value: 0.5,
        };
        let line = serde_json::to_string(&s).unwrap();
        assert_eq!(
            line,
            r#"{"record":"sample","ts_ns":1,"entity":"car/front/ecu1","metric":"vxprobe_rtt_seconds","labels":{"quantile":"0.99"},"value":0.5}"#
        );
        assert_eq!(serde_json::from_str::<Record>(&line).unwrap(), s);
        let gap = Record::Gap {
            entity: HOST.into(),
            metric: None,
            from_ns: 1,
            to_ns: 2,
            reason: "scrape failed".into(),
        };
        assert!(
            serde_json::to_string(&gap)
                .unwrap()
                .starts_with(r#"{"record":"gap""#)
        );
    }

    #[test]
    fn entities_name_their_log_files() {
        let dir = std::env::temp_dir().join(format!("vxtel-entlogs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("entities.json"),
            r#"[{"path":"car/front/ecu1","kind":"node","logs":[
                {"stream":"serial","path":"/run/vx/vx-ecu1.serial.log"},
                {"stream":"console","path":"rel.log"},{"path":"no-stream.log"}]}]"#,
        )
        .unwrap();
        let logs = Entities::load(&dir).logs().to_vec();
        assert_eq!(logs.len(), 2, "{logs:?}");
        assert_eq!(
            logs[0].path,
            std::path::Path::new("/run/vx/vx-ecu1.serial.log")
        );
        assert_eq!(
            (logs[1].stream.as_str(), logs[1].path.clone()),
            ("console", dir.join("rel.log"))
        );
        assert!(logs.iter().all(|l| l.entity == "car/front/ecu1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn labels_parse_with_escapes() {
        assert_eq!(
            parse_labels(r#"{quantile="0.99", path="a\"b\\c"}"#),
            BTreeMap::from([
                ("quantile".to_string(), "0.99".to_string()),
                ("path".to_string(), "a\"b\\c".to_string())
            ])
        );
        assert!(parse_labels("").is_empty());
        assert_eq!(parse_labels(r#"{a="1",bad}"#).len(), 1);
    }

    #[test]
    fn targets_bind_to_their_entities_or_the_host() {
        let dir = std::env::temp_dir().join(format!("vxtel-entities-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("entities.json"),
            r#"[{"path":"host","kind":"host"},{"path":"car/front/ecu1","kind":"node","scrape_targets":["10.1.0.2:9801"]}]"#,
        )
        .unwrap();
        let e = Entities::load(&dir);
        assert_eq!(e.of("10.1.0.2:9801"), "car/front/ecu1");
        assert_eq!(e.of("127.0.0.1:1"), HOST);
        assert_eq!(Entities::load(Path::new("/nonexistent")).of("x"), HOST);
        std::fs::remove_dir_all(&dir).ok();
    }
}
