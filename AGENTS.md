# AGENTS.md — vxedge/vxtelemetry

Telemetry ingestion and Prometheus export; the collection/archival half of every experiment.
Service code lives here; per-experiment runtime descriptors live in the experiment manifest.
Master spec: `../docs/ARCHITECTURE.md` §11, §12 (metrics), §6 (`telemetry:` block).

## What lands here

- Ingestion service (Rust): scrapes/receives agent metrics, aggregates per slice + per node.
- Prometheus export + scrape-target management from the experiment manifest.
- `vxedge telemetry collect` backend: bundles Prometheus data, PCAPs, service logs, and
  results metadata (experiment ID, manifest checksum, CLI version) into the results archive.
- **GPU metrics hooks** (workspace NVIDIA rule): DCGM/NVML collectors for GPU utilization,
  VRAM boundaries, and thermal/power throttling, exported alongside standard metrics.

## Rules

- Ingestion rate and metric completeness are themselves reported metrics (§12).
- Phase 3: vehicle-signal telemetry follows COVESA VSS via the Kuksa databroker. The
  open-source AWS IoT FleetWise edge agent (C++) is a *design reference only* — the AWS
  service is closed to new customers; never add it as a dependency.
- Transport for off-host telemetry rides the WireGuard link (§7).
- Rust for services (§15); container-first, digest-tagged, non-root.
