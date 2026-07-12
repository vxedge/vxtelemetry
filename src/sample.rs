//! Per-process resource sampling (§12 compute metrics): CPU% and RSS of
//! processes matched by cmdline regex, sampled on an interval and archived
//! alongside the latency data. Built for exp-002b (packaging resource cost);
//! reused by exp-003 (OTA co-location footprint).

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use crate::TelemetryError;

#[derive(Debug, Clone)]
pub struct Matcher {
    pub label: String,
    pub regex: regex_lite::Regex,
}

impl Matcher {
    /// Parse `label=regex` CLI form.
    pub fn parse(spec: &str) -> Result<Self, TelemetryError> {
        let (label, pattern) = spec.split_once('=').ok_or_else(|| {
            TelemetryError::Manifest(format!("--match needs label=regex: {spec}"))
        })?;
        Ok(Self {
            label: label.to_string(),
            regex: regex_lite::Regex::new(pattern)
                .map_err(|e| TelemetryError::Manifest(format!("bad regex `{pattern}`: {e}")))?,
        })
    }
}

fn clk_tck() -> f64 {
    std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(100.0)
}

/// (utime+stime ticks, rss_kb) for one pid; None if it vanished.
fn read_proc(pid: u32) -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces: fields resume after the last ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some((utime + stime, rss_pages * 4)) // 4 KiB pages
}

fn matching_pids(matchers: &[Matcher]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    let me = std::process::id();
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        for m in matchers {
            if m.regex.is_match(&cmdline) {
                out.push((m.label.clone(), pid));
                break;
            }
        }
    }
    out
}

#[derive(Serialize)]
struct Sample<'a> {
    ts_unix_ms: i64,
    label: &'a str,
    pid: u32,
    cpu_pct: f64,
    rss_kb: u64,
}

#[derive(Default, Serialize)]
pub struct LabelSummary {
    pub samples: u64,
    pub cpu_pct_mean: f64,
    pub cpu_pct_max: f64,
    pub rss_mb_mean: f64,
    pub rss_mb_max: f64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}

/// Sample matched processes every `interval` for `duration`; write
/// `resource-samples.jsonl` + `resource-summary.json` into `out_dir`.
pub async fn run_sample(
    matchers: &[Matcher],
    out_dir: &Path,
    interval: Duration,
    duration: Duration,
) -> Result<HashMap<String, LabelSummary>, TelemetryError> {
    std::fs::create_dir_all(out_dir)?;
    // clk_tck() and the per-tick /proc scan below are blocking syscalls/exec;
    // spawn_blocking keeps them off the async runtime (rust-patterns skill).
    let tick_hz = tokio::task::spawn_blocking(clk_tck)
        .await
        .expect("clk_tck blocking task panicked");
    let mut prev_ticks: HashMap<u32, u64> = HashMap::new();
    let mut lines = String::new();
    let mut agg: HashMap<String, Vec<(f64, u64)>> = HashMap::new();

    let rounds = (duration.as_millis() / interval.as_millis().max(1)).max(1);
    let mut ticker = tokio::time::interval(interval);
    for round in 0..=rounds {
        ticker.tick().await;
        let ts = now_ms();
        let matchers_owned = matchers.to_vec();
        let observed = tokio::task::spawn_blocking(move || {
            matching_pids(&matchers_owned)
                .into_iter()
                .filter_map(|(label, pid)| {
                    read_proc(pid).map(|(ticks, rss_kb)| (label, pid, ticks, rss_kb))
                })
                .collect::<Vec<_>>()
        })
        .await
        .expect("proc-sampling blocking task panicked");
        for (label, pid, ticks, rss_kb) in observed {
            let prev = prev_ticks.insert(pid, ticks);
            // First observation of a pid has no delta — skip its cpu sample.
            if round == 0 || prev.is_none() {
                continue;
            }
            let dticks = ticks.saturating_sub(prev.unwrap_or(ticks)) as f64;
            let cpu_pct = 100.0 * dticks / tick_hz / interval.as_secs_f64();
            let s = Sample {
                ts_unix_ms: ts,
                label: &label,
                pid,
                cpu_pct,
                rss_kb,
            };
            lines.push_str(&serde_json::to_string(&s).expect("serializable sample"));
            lines.push('\n');
            agg.entry(label).or_default().push((cpu_pct, rss_kb));
        }
    }
    std::fs::write(out_dir.join("resource-samples.jsonl"), &lines)?;

    let mut summary: HashMap<String, LabelSummary> = HashMap::new();
    for (label, vals) in agg {
        let n = vals.len() as f64;
        let cpu: Vec<f64> = vals.iter().map(|v| v.0).collect();
        let rss: Vec<f64> = vals.iter().map(|v| v.1 as f64 / 1024.0).collect();
        summary.insert(
            label,
            LabelSummary {
                samples: vals.len() as u64,
                cpu_pct_mean: cpu.iter().sum::<f64>() / n,
                cpu_pct_max: cpu.iter().cloned().fold(0.0, f64::max),
                rss_mb_mean: rss.iter().sum::<f64>() / n,
                rss_mb_max: rss.iter().cloned().fold(0.0, f64::max),
            },
        );
    }
    std::fs::write(
        out_dir.join("resource-summary.json"),
        serde_json::to_vec_pretty(&summary).expect("serializable summary"),
    )?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_parses_label_and_regex() {
        let m = Matcher::parse("qemu=qemu-system.*vx-node").unwrap();
        assert_eq!(m.label, "qemu");
        assert!(m.regex.is_match("qemu-system-x86_64 -name vx-node-1"));
        assert!(Matcher::parse("noequals").is_err());
    }

    #[test]
    fn reads_own_proc_stat() {
        let (ticks, rss_kb) = read_proc(std::process::id()).expect("self readable");
        assert!(rss_kb > 100, "own RSS should exceed 100 kB, got {rss_kb}");
        let _ = ticks;
    }

    #[tokio::test]
    async fn samples_self_and_summarizes() {
        let tmp = std::env::temp_dir().join(format!("vxtel-sample-{}", std::process::id()));
        // Match this test binary by its own file-name fragment. read_proc
        // skips our own pid, so match the *parent* cargo/test harness too —
        // simplest reliable target: any process containing "cargo" or our exe.
        let exe = std::env::current_exe().unwrap();
        let frag = exe.file_name().unwrap().to_string_lossy().to_string();
        let m = vec![Matcher::parse(&format!("self={frag}|cargo")).unwrap()];
        let summary = run_sample(
            &m,
            &tmp,
            Duration::from_millis(100),
            Duration::from_millis(400),
        )
        .await
        .unwrap();
        let s = summary.get("self").expect("matched own test process");
        assert!(s.samples >= 2);
        assert!(s.rss_mb_max > 0.1);
        assert!(tmp.join("resource-samples.jsonl").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
