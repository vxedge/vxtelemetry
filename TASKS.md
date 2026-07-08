# TASKS.md — vxedge/vxtelemetry

## Phase 1 (critical path: exp-001 needs collect)

- [ ] Crate scaffold + CI
- [ ] Ingestion service v1: Prometheus scrape of vxagents exporters per manifest
      `telemetry.prometheus.scrape_targets`; per-slice/per-node aggregation labels
- [ ] `collect` backend: archive Prometheus TSDB slice + PCAPs + logs + metadata
      (experiment ID, manifest checksum, CLI version) → `results/<exp>.zip`
- [ ] Ingestion-rate + completeness self-metrics
- [ ] Container image + service descriptor example (`scrape_interval_s` config)

## Phase 2

- [ ] GPU collectors: DCGM (preferred) / NVML fallback — utilization, VRAM, throttling
- [ ] PCAP post-processing helpers: latency/jitter/loss extraction per slice (feeds
      vxperiments notebooks)

## Phase 3

- [ ] Kuksa/VSS databroker integration: subscribe + persist vehicle signals per vehicle
      manifest `signal_plane` block
- [ ] Streaming path to vxedged `TelemetryService.StreamMetrics` (live dashboards/HMI)
