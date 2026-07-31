# Changelog

All notable changes to this repo are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/) — see `docs/VERSIONING.md` in the `docs` repo.

## [Unreleased]

### Changed

- **BREAKING: `ingest` and `collect` read a §6.0 experiment workspace, not a §6.1 manifest.**
  `--workspace <dir>` is the flag; `--manifest` remains as an alias and takes the same
  directory. §6.1 manifests stopped being a provisioning input in `vxedge` on 2026-07-31, and
  this tool was the last consumer keeping them alive — `vxedge-core::lower` existed only to
  hand this tool one, and is now deleted.

  Done **without depending on `vxedge-core`**: the blocks read here (`telemetry`, `results`) are
  untyped passthrough in the domain model, so the dependency would share no schema, and `vxedge`
  is a separate private repo whose cross-repo checkout this repo's CI cannot do. What is shared
  is one structural convention — a flat folder of documents, each declaring its `kind` — and
  `workspace_view`'s tests assert it.

- **BREAKING: archive metadata records `workspace_sha256` and the `.vxp` identity** in place of
  `manifest_sha256`, and the archive bundles the source workspace under `workspace/` rather than
  a single `manifest.yaml`. An archive should be enough to re-run from, and a workspace is
  several files. The old hash covered a document the daemon generated and nobody authored.
  `vxperiments`' notebooks accept either key, so frozen archives keep analysing.


## [0.1.0] - 2026-07-13

**Phase-1 gate passed**: H1, H3, and H4 all have reproducible numbers (`vxperiments`).
Repos stay private for now — see `docs/STATUS.md` for the public-visibility punch list.

### Added
- `ingest`: manifest-driven Prometheus scraping (no system Prometheus needed), raw
  exposition + structured sample archival, ingestion-rate/completeness tracking
  (2026-07-10).
- `collect`/archive: bundles Prometheus data + reproducibility metadata (manifest SHA-256,
  tool version) into a results archive; e2e CI verified live against `vxprobe` (2026-07-10).
- `sample`: per-process CPU%/RSS resource sampling by cmdline regex match, built for
  exp-002b's packaging resource-cost measurement, reused by exp-003 (2026-07-11).
- `telemetry.prometheus.optional_scrape_targets` (2026-07-13): marks a manifest scrape
  target as legitimately absent under some experiment arms — tracked in separate
  `optional_scrapes_ok`/`optional_scrapes_failed` counters on `IngestStats`, never
  counted against `completeness_pct()`. Verified live (real listening exporter +
  deliberately-absent optional target) and with new unit tests on `scrape_round`.
- CI: build/test/clippy/fmt (incl. e2e), Dependabot.

### Fixed
- Round-count off-by-one in `ingest::run_ingest` that finished scraping measurably before
  the probe/sample window ended, silently truncating the last ~5s (up to 50% in `--quick`
  mode) of every campaign run — aligned to the same inclusive-range convention `sample`
  already used (2026-07-12, found by an 8-angle code review of the exp-002b feature).
- `sample::run_sample`'s per-tick `/proc` scan and `clk_tck`'s process spawn moved off the
  async runtime into `spawn_blocking` — was blocking the calling tokio worker thread for
  the duration of a full `/proc` scan every ~500ms (2026-07-12, same review).

### Known gaps
- Daemon wiring of `TelemetryService.Collect` (currently CLI-only), container image,
  GPU/DCGM metrics (phase 2) remain open — tracked in `TASKS.md`.
