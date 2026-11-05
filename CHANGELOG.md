# Changelog

All notable changes to this repo are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/) — see `docs/VERSIONING.md` in the `docs` repo.

## [Unreleased]

### Changed

- A workspace's `vxp.yaml` (the `vxp/1` package descriptor, vxedge 0.6.0) is read as the package
  the archive records, not as a document, so a `vxp/1` workspace no longer fails with ``missing
  field `kind` ``. Legacy `<name>.vxp` descriptors are read as before.

## [0.3.0] - 2026-11-04

An archive is enough to check a run and to re-run it. It now carries every file the workspace
identity covers, and `vxtelemetry verify` re-hashes them to the recorded identity. The identity is
computed under `vxws/2`, as vxedge 0.5.3 computes it, so values differ from 0.2.1's; older archives
stay verifiable as `vxws/1`. Archives grow by the size of the workspace's `parts/`, `assets/`,
`scripts/` and `tools/`.

### Changed

- The workspace identity in `metadata.json` is now computed under `vxws/2` (`ARCHITECTURE.md`
  §13). It skips `.git` at any depth, editor swap and backup files, and whatever a root
  `.vxpignore` names in `.gitignore` syntax; `results/` and `.git` cannot be re-included, and
  `.vxpignore` is itself hashed. Values differ from 0.2.1's by design.
- `metadata.json` gains `workspace_identity_algorithm`. An archive without it was written before
  `vxws/2` and holds a `vxws/1` value, which `identity::compute(root, "vxws/1")` still
  reproduces. Both rules are pinned to the same test vectors as `vxedge-core`.
- The archive's `workspace/` carries every file the workspace identity covers (documents,
  `parts/`, `assets/`, `scripts/`, `tools/`), not only the documents, so an archive is enough to
  re-run from and re-hashes to its own `workspace_sha256`. Archives grow by the size of those
  directories.

### Added

- `vxtelemetry verify <archive>`: re-hashes `workspace/` under the recorded algorithm, prints the
  result as JSON and exits 1 on a mismatch. An archive naming no algorithm is refused with the
  reason. `archive::verify` is the library form.

## [0.2.1] - 2026-11-04

An archive tells a host process from a missing image.

### Fixed

- `image_digests` records a `local/...` part as a host process instead of
  `unresolved: podman image inspect failed`, which read as missing provenance. Found in the vxedge
  0.5.0 release gate (#17).

### Security

- Lockfile update `tokio` 1.53.1 -> 1.54.1, which fixes RUSTSEC-2026-0386 (unsound `AsyncFd` constructors). This repo does not call `AsyncFd` directly.

## [0.2.0] - 2026-10-22

### Added

- Tag-triggered release workflow (`.github/workflows/release.yml`): builds `vxtelemetry` for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` (native Ubuntu 24.04 runners, `--locked`), publishes `vxtelemetry-<version>-<target>.tar.gz` archives with a `SHA256SUMS`, and attaches build provenance (`gh attestation verify`). `workflow_dispatch` is a dry run that creates no release.
- `collect`: opt-in image-digest recording into archive metadata (`ARCHITECTURE.md` §13).
- Open-source release preparation: `LICENSE` (Apache-2.0), `NOTICE`, rewritten `README.md`. Crate `license` metadata is now `Apache-2.0` (was `MIT OR Apache-2.0`).

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

### Security

- `zip` 8 -> 9, which fixes RUSTSEC-2026-0367, -0368 and -0369 (this tool only writes archives; it does not extract).

## [0.1.0] - 2026-07-13

**Phase-1 gate passed**: H1, H3, and H4 all have reproducible numbers (`vxperiments`).
Repositories were still private at this release.

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
