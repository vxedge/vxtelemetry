//! Partial, tolerant view of an experiment manifest: only the keys telemetry
//! needs. Unknown fields are ignored on purpose (schema validation lives in
//! vxedge-core; duplicating it here would create a second validator).

use serde::Deserialize;

use crate::TelemetryError;

#[derive(Debug, Deserialize)]
pub struct ManifestView {
    pub experiment: ExperimentView,
    #[serde(default)]
    pub telemetry: Option<TelemetryBlock>,
    #[serde(default)]
    pub results: Option<ResultsBlock>,
}

#[derive(Debug, Deserialize)]
pub struct ExperimentView {
    pub id: String,
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
}

#[derive(Debug, Default, Deserialize)]
pub struct ResultsBlock {
    #[serde(default)]
    pub artifact_dir: Option<String>,
}

impl ManifestView {
    pub fn from_yaml(yaml: &str) -> Result<Self, TelemetryError> {
        serde_yaml_ng::from_str(yaml).map_err(|e| TelemetryError::Manifest(e.to_string()))
    }

    pub fn scrape_targets(&self) -> Vec<String> {
        self.telemetry
            .as_ref()
            .and_then(|t| t.prometheus.as_ref())
            .map(|p| p.scrape_targets.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_needed_slice_and_ignores_the_rest() {
        let yaml = r#"
experiment: { id: exp-001, name: whatever, duration_s: 10 }
network: { slices: [] }
zones: []
telemetry:
  prometheus:
    scrape_targets: ["127.0.0.1:9801", "127.0.0.1:9802"]
results:
  artifact_dir: results/exp-001
"#;
        let v = ManifestView::from_yaml(yaml).unwrap();
        assert_eq!(v.experiment.id, "exp-001");
        assert_eq!(v.scrape_targets().len(), 2);
        assert_eq!(
            v.results.unwrap().artifact_dir.as_deref(),
            Some("results/exp-001")
        );
    }

    #[test]
    fn missing_telemetry_block_means_no_targets() {
        let v = ManifestView::from_yaml("experiment: { id: e }\n").unwrap();
        assert!(v.scrape_targets().is_empty());
    }
}
