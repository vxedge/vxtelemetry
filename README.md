# vxtelemetry

Telemetry ingestion, Prometheus export, and reproducible results archiving for vxedge experiments. Ingestion rate and completeness are reported as metrics in their own right.

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/vxedge/vxtelemetry/actions/workflows/ci.yml/badge.svg)](https://github.com/vxedge/vxtelemetry/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/tag/vxedge/vxtelemetry?label=release)](https://github.com/vxedge/vxtelemetry/tags)

vxedge is an open, all-software testbed for mapping 5G network slices onto zonal and central software-defined-vehicle (SDV) E/E architectures on a single machine. Every published number ships with the manifest and image digests that produced it.

## Commands

| Command | What |
|---|---|
| `vxtelemetry ingest` | Scrape the workspace's targets on an interval; persist raw and structured samples |
| `vxtelemetry sample` | Sample CPU% and RSS of processes matched by a cmdline regex |
| `vxtelemetry collect` | Bundle the ingest directory, the source workspace and reproducibility metadata (including image digests) into one zip archive |

## Quickstart

```bash
cargo build
cargo test          # includes an ingest → collect end-to-end test
./target/debug/vxtelemetry --help
```

## What it produces

These archives are the `results/*.zip` inputs that every analysis notebook in
[`vxperiments`](https://github.com/vxedge/vxperiments) reads. Each archive carries the
manifest sha256 that the frozen summaries quote.

## Links

- Site: <https://vxedge.dev>
- Architecture: [`docs/ARCHITECTURE.md`](https://github.com/vxedge/docs/blob/main/ARCHITECTURE.md)
- Results: [`docs/RESULTS.md`](https://github.com/vxedge/docs/blob/main/RESULTS.md) · Roadmap: [`docs/ROADMAP.md`](https://github.com/vxedge/docs/blob/main/ROADMAP.md)
- Contributing: [CONTRIBUTING](https://github.com/vxedge/.github/blob/main/CONTRIBUTING.md) · Security: [SECURITY](https://github.com/vxedge/.github/blob/main/SECURITY.md)
- License: Apache-2.0 ([`LICENSE`](LICENSE))
