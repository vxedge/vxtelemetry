//! Minimal Prometheus text-format parsing: enough to turn agent expositions
//! into structured samples for the analysis notebooks. Comments/TYPE lines
//! are skipped; histograms arrive pre-quantiled from vxprobe.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    pub metric: String,
    /// Raw label block including braces (e.g. `{quantile="0.99"}`), empty if none.
    pub labels: String,
    pub value: f64,
}

/// Parse one exposition body into samples; malformed lines are skipped
/// (a partial scrape is better than no scrape — completeness is itself
/// a reported metric).
pub fn parse(body: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name_part, value_part)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(value) = value_part.parse::<f64>() else {
            continue;
        };
        let (metric, labels) = match name_part.find('{') {
            Some(i) => (&name_part[..i], &name_part[i..]),
            None => (name_part, ""),
        };
        if metric.is_empty() {
            continue;
        }
        out.push(Sample {
            metric: metric.to_string(),
            labels: labels.to_string(),
            value,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_labeled_lines() {
        let body = "\
# HELP vxprobe_sent_total probes sent
vxprobe_sent_total 200
vxprobe_rtt_seconds{quantile=\"0.99\"} 0.000107583
garbage line without value x
";
        let samples = parse(body);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].metric, "vxprobe_sent_total");
        assert_eq!(samples[0].value, 200.0);
        assert_eq!(samples[1].labels, "{quantile=\"0.99\"}");
    }
}
