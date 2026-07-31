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
- [x] Image-digest recording (2026-07-18): `collect --record-image <ref>` (repeatable)
      resolves each reference to its content-addressed identity via `podman image
      inspect` (repo digest preferred, local image ID as fallback for locally-built
      images) and records the map in `metadata.json` (§13). Deliberately caller-driven —
      which images matter is the campaign runner's knowledge, not this tool's; guessing
      would hardcode another repo's image naming here. Absent entirely (not `null`) when
      unused, so pre-existing archives' metadata shape is unchanged; a failed lookup is
      recorded as an honest `unresolved: …` value, never an abort of a completed
      campaign point's archive. Live-verified against real podman (vx5g-core/vx5g-ran
      localhost repo digests + mongo:7 registry digest + a nonsense ref correctly
      recorded as unresolved); wired into `vxperiments/scripts/run-exp-010.sh`. Closes
      the "image-digest recording into results metadata" item sized 2026-07-17 in
      `vxedge/TASKS.md` — the investigation it called for landed here: archive-writing
      is owned by this repo's `archive.rs`, not vxedge-core
- [x] Ingestion-rate + completeness self-metrics (persisted into the archive)
- [x] `optional_scrape_targets` (2026-07-13): a manifest scrape target can be marked
      legitimately-absent-under-some-arms; its failures track in separate
      `optional_scrapes_ok`/`optional_scrapes_failed` counters and never lower
      `completeness_pct()`. Fixes exp-003's baseline arm always showing ~50% aggregate
      completeness for a target (the OTA burst driver's exporter) that only exists during
      the ota-burst arm by design — see docs/ARCHITECTURE.md §6
- [ ] Wire vxedged `TelemetryService.Collect` to invoke this (vxedge repo)
- [ ] Container image + service descriptor example (`scrape_interval_s` config)
- [ ] CI: build + publish image to GHCR with SHA256 digest recorded (release/artifact pipeline,
      phase 2)

## Phase 2

- [x] **Read §6.0 workspaces; cut every tie to §6.1 files** (2026-07-31). `ingest` and `collect`
      take `--workspace <dir>` (`--manifest` kept as an alias, same meaning), read the
      `kind: experiment` document's `telemetry.prometheus` and `results` blocks, and take
      identity from the `.vxp`. This was the last consumer keeping §6.1 alive, and
      `vxedge-core::lower` — which existed only so the daemon could hand this tool a §6.1 file —
      is deleted.

      **Done without depending on `vxedge-core`**, against the 2026-07-30 note that said it
      would. Two reasons the earlier decision did not account for: `vxedge` is a separate
      private repository, so the dependency needs cross-repo checkout credentials this repo's CI
      does not have — a measurement tool would stop building for an authorisation reason. And
      the blocks read here are *untyped passthrough* in the domain model, so the dependency
      would not share one line of schema. What is shared is one structural convention (a flat
      folder of documents, each declaring its `kind`), asserted in `workspace_view`'s tests.

      Archive metadata records `workspace_sha256` (over every file, names included) and the
      `.vxp` identity in place of `manifest_sha256`, and the archive bundles the workspace
      itself under `workspace/` rather than one generated `manifest.yaml` — an archive should be
      enough to re-run from. `vxperiments`' notebooks accept either key, so frozen archives keep
      analysing.

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
