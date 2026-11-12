# Changelog

All notable changes to this repo are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/) — see `docs/VERSIONING.md` in the `docs` repo.

## [Unreleased]

## [0.7.1] - 2026-11-13

`verify` tells a checked identity, a mismatch and an identity it cannot check apart, and never
assumes an algorithm.

### Fixed

- `vxtelemetry verify` no longer reports a mismatch for a v2 archive whose identity it cannot
  check. It used to assume `vxws/2` when an archive recorded no `workspace_identity_algorithm`,
  and stopped with an error when it recorded only `manifest_sha256`. The index check is now the
  integrity result, and the identity is `checked`, `mismatch` or `not_checkable` with the reason:
  "written before identity algorithms were recorded; package/ holds only the documents", or
  "written before workspace identities; records a manifest digest". No algorithm is assumed.
  The JSON gains `index` and `identity` (`outcome`, `reason`); `algorithm`, `recorded` and
  `computed` are left out when the archive gives no value for them.
- `verify` exits 3 when nothing failed but the identity is not checkable, and prints a summary
  line on stderr. 0 still means every check passed, 1 that one failed, and 2 is a usage error.

## [0.7.0] - 2026-11-12

Archives name the tools behind them. `collect` records the tool binaries the run's daemon runs as
`tools` in `metadata.json`, for vxedge 0.24.0. Additive: nothing else in the archive changes.

### Added

- `collect` records the ingest folder's `tools.json` as `tools` in `metadata.json`, in archive v1
  and v2 alike: the tool binaries the run's daemon runs, each with its `name`, `path`, `version`,
  `sha256` and `profile`. vxedged writes the file at every collect (X9.14). It is recorded as
  given, and absent when there is no such file or it is not a list.

## [0.6.0] - 2026-11-10

Archives know which campaign trial they are. `collect` records the trial's identity from the
ingest folder's `trial.json`, and the phase markers a campaign runner writes reach the records,
with the late ones included. This is for vxedge 0.13.0's `CampaignService`. Nothing changes for
an archive outside a campaign.

### Added

- Campaign trials (vxedge's `CampaignService`):
  - `collect` records the ingest folder's `trial.json`, the trial's campaign id, trial id, arm,
    repetition and parameter values, as `trial` in `metadata.json`, in archive v1 and v2 alike.
  - Phase markers the runner appends to `phases.jsonl` (`phase` records: `ts_ns`, `trial`,
    `phase`) move into `records.jsonl` with the time the runner gave them. Ingest moves them
    every round, and `collect` once more before it archives, for the markers that came after the
    last round, such as teardown. This tool stays the one writer of `records.jsonl`.
  - Both are absent outside a campaign, and nothing changes for an archive without them.

## [0.5.0] - 2026-11-09

Records are named by entity, a run's logs are archived, and archive v2 packs both.
- **Records by entity.** Ingest writes `records.jsonl`: `sample`, `gap` and `phase` records, each
  naming the vehicle, zone, node, slice or service it is about, as vxedged's `entities.json` maps
  them.
- **Logs.** Ingest reads each entity's log files into `logs.jsonl`.
- **Archive v2.** `collect --archive-version 2` packs both, zstd-compressed and per entity, with
  an index that `verify` checks. `records` prints any archive's records, v1 or v2.
- **Fix.** Each round now flushes what it writes, so the last round of a run is no longer at risk
  on exit.

**Upgrading from 0.4.x.** Archive v1 stays the default, so existing notebooks read new archives
unchanged. Set `--archive-version 2` (or `VXTELEMETRY_ARCHIVE_VERSION=2`) for the new shape. Logs
and entity names need vxedge 0.12.0's `entities.json`; without it, every target binds to `host` and
no logs are read.

### Fixed

- An ingest round could return before its `records.jsonl` and `samples.jsonl` lines were
  written: tokio file writes finish on a background thread, and the round never flushed. A reader
  could find an empty file, and the last round of a run could be lost on exit. Each round now
  flushes both files. Found as an intermittent CI failure of
  `a_round_writes_entity_samples_and_gaps`.

### Added

- Telemetry records (`ARCHITECTURE.md` §12): each ingest round also appends to `records.jsonl` a
  `sample` record per value (`ts_ns`, `entity`, `metric`, `labels` as a map, `value`), named by the
  entity `entities.json` binds the target to (`host` otherwise), and a `gap` record for a required
  target that did not answer. `samples.jsonl` keeps its v1 shape.
- A target that serves many entities, such as vxedged's own exporter, names each sample's entity
  in an `entity` label, which wins over the target's entity and is dropped from `labels`. Ingest
  also scrapes the targets `entities.json` lists beside the workspace's own, as optional targets.
- Archive v2, `vxtelemetry collect --archive-version 2` (or `VXTELEMETRY_ARCHIVE_VERSION=2`): a zip
  with `index.json` (schema `vxarchive/2`, every member with its size and sha256), `metadata.json`,
  `package/` (the identity inputs), `entities.json`, `metrics/samples.jsonl.zst` (samples and gaps,
  zstd) and `phases.jsonl`. It leaves out the raw ingest folder. Python's standard library reads it
  from 3.14 (`zipfile`, `compression.zstd`). v1 stays the default.
- `vxtelemetry verify` checks a v2 archive's index too: a changed, added or missing member fails
  it. `vxtelemetry records <archive>` prints the records of a v1 or v2 archive as JSON Lines; a v1
  archive's samples read as `sample` records.
- Log records (decision X-18): ingest reads the log files `entities.json` names for each entity
  (`logs: [{stream, path}]`) and appends one `log` record per complete line (`ts_ns` of the round
  that read it, `entity`, `stream`, `line`) to `logs.jsonl`. A line still being written waits for
  the next round; a round reads at most 1 MiB per file; where each file was read up to is kept in
  `logs.offsets.json`, so a restarted ingest resumes; a file that shrinks is read again from its
  start. `logs.jsonl` exists from the first round, empty when nothing logged; a file that cannot be
  read for a reason other than not existing yet is reported once and skipped.
  `ingest-stats.json` counts `log_lines_total`. Archive v2 packs them as
  `logs/<entity>.jsonl.zst`, one member per entity, and `vxtelemetry records --logs` prints them.

## [0.4.0] - 2026-11-06

An archive of a `vxp/1` package now names the package by its digest and can prove it.
`metadata.json` records `package.digest`, computed here independently of vxedge, whether the run
checked its lock (`package.locked`, as vxedge 0.7.0 reports it), and the kernel the run ran on.
`vxtelemetry verify` re-packs the archived workspace and checks the digest as well as the identity.
vxtelemetry and vxedge agree on both packed-bytes test vectors and with Python's `tarfile`. An
archive without a digest verifies as before. Also the Rust 1.99.1 toolchain.

### Added

- `package::digest`, the package digest of a `vxp/1` package (`ARCHITECTURE.md` §6.0.7), computed
  from its identity inputs independently of vxedge. It agrees with vxedge on both packed-bytes test
  vectors, which cover long paths split at the shortest prefix, and with Python's `tarfile`.
- `metadata.json` records what a run of a `vxp/1` package came from and ran on
  (`ARCHITECTURE.md` §13): `package.digest`, computed from the archived inputs; `package.locked`,
  only when the daemon says whether `up` checked the lock (`--package-locked`, or
  `VXTELEMETRY_PACKAGE_LOCKED` from the daemon), never derived; and `kernel` (`release`,
  `preempt_rt`) of the collecting host, the run's host, for every archive. A legacy descriptor
  records no digest.
- `vxtelemetry verify` re-packs `workspace/` and compares it with `package.digest` when the archive
  records one; `ok` needs both the identity and the digest to match. Archives without a digest
  verify as before.

### Changed

- Lockfile update: the semver-compatible dependency updates `cargo update` offers (31 crates). No `Cargo.toml` change and no source change; build, test, clippy `-D warnings`, fmt and `cargo deny` pass.
- The toolchain moves from Rust 1.96.1 to 1.99.1 (`rust-toolchain.toml`, `rust-version` 1.99). Build, test, clippy `-D warnings`, fmt and `cargo deny` pass unchanged; no source changes were needed.
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
