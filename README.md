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
| `vxtelemetry verify` | Check an archive: every member against its index (v2), the recorded workspace identity and the package digest. Exits 0 when all pass, 1 when one fails, 3 when nothing failed but the identity is not checkable (see below) |
| `vxtelemetry records` | Print an archive's records, v1 or v2, as JSON Lines |

### Verify outcomes

`verify` prints its result as JSON on stdout and one summary line on stderr. For a v2 archive, the
index check is the integrity result. The identity is then one of three outcomes:

- **checked:** `package/`, re-hashed under the recorded `workspace_identity_algorithm`, gives the
  recorded `workspace_sha256`;
- **mismatch:** it gives a different value (exit 1);
- **not checkable**, with the reason (exit 3 when nothing else failed). An archive that records no
  algorithm was written before identity algorithms were recorded, and its `package/` holds only
  the documents. One that records only `manifest_sha256` was written before workspace
  identities. `verify` never assumes an algorithm.

Exit code 2 is a usage error.

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
- Results: [`docs/RESULTS.md`](https://github.com/vxedge/docs/blob/main/RESULTS.md)
- Contributing: [CONTRIBUTING](https://github.com/vxedge/.github/blob/main/CONTRIBUTING.md) · Security: [SECURITY](https://github.com/vxedge/.github/blob/main/SECURITY.md)
- License: Apache-2.0 ([`LICENSE`](LICENSE))
