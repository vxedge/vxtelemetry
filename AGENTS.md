# AGENTS.md — vxedge/vxtelemetry

Telemetry ingestion and Prometheus export; the collection/archival half of every experiment.
Service code lives here; per-experiment runtime descriptors live in the experiment manifest.
Master spec: `../docs/ARCHITECTURE.md` §11, §12 (metrics), §6 (`telemetry:` block).

## What is here

- `ingest`: scrapes the workspace's Prometheus targets on an interval and persists raw and
  structured samples. Per-slice aggregation happens downstream, in the analysis notebooks.
- `sample`: CPU% and RSS of processes matched by a cmdline regex.
- `collect`: the results-archive backend. Bundles the ingest directory, the source workspace
  and reproducibility metadata (experiment ID, workspace checksum, tool version, kernel,
  image digests) into one zip archive.

## Rules

- Ingestion rate and metric completeness are themselves reported metrics (§12).
- Vehicle signal names follow COVESA VSS. Never add the AWS IoT FleetWise edge agent as a
  dependency; the AWS service is closed to new customers.
- Transport for off-host telemetry rides the WireGuard link (§7).
- Rust for services (§15); container-first, digest-tagged, non-root.

## Project skills

AI/contributor skills in `.claude/skills/`: **rust-patterns** (house Rust rules, per the
Microsoft Rust Patterns book) and **testbed-quality** (the OEM-adoption bar). Apply both to
every change.

## Work tracking

Work items are tracked on the maintainers' internal board; the public record of changes is
`CHANGELOG.md`.
