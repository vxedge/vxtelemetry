# TASKS.md — vxedge/vxtelemetry

## Phase 1 (critical path: exp-001 needs collect)

- [x] Crate scaffold + CI (build/test/clippy/fmt + cargo-deny, Dependabot)
- [x] Ingestion v1: interval scrape of vxagents exporters per manifest
      `telemetry.prometheus.scrape_targets`; raw .prom + structured samples.jsonl
      *(per-slice aggregation happens downstream in the analysis notebook for now —
      samples carry target labels)*
- [x] `collect` backend: `results/<exp>.zip` with metadata.json (experiment ID, manifest
      SHA-256, tool version, ingest stats) + manifest + full ingest dir; e2e test in CI
      *(PCAPs/service logs join the bundle when vxpcap/services exist)*
- [x] Ingestion-rate + completeness self-metrics (persisted into the archive)
- [ ] Wire vxedged `TelemetryService.Collect` to invoke this (vxedge repo)
- [ ] Container image + service descriptor example (`scrape_interval_s` config)
- [ ] CI: build + publish image to GHCR with SHA256 digest recorded (release/artifact pipeline,
      phase 2)

## Phase 2

- [ ] GPU collectors: DCGM (preferred) / NVML fallback — utilization, VRAM, throttling
- [ ] Per-node resource sampling during runs: CPU/RSS of qemu PIDs and netns payloads
      (/proc + cgroup), archived per arm — required by exp-002b (§12 compute metrics)
- [ ] PCAP post-processing helpers: latency/jitter/loss extraction per slice (feeds
      vxperiments notebooks)

## Phase 3

- [ ] Kuksa/VSS databroker integration: subscribe + persist vehicle signals per vehicle
      manifest `signal_plane` block
- [ ] CAN→VSS ingestion via Kuksa CAN feeders (frames arriving over `type: can` link edges)
- [ ] Streaming path to vxedged `TelemetryService.StreamMetrics` (live dashboards/HMI)
