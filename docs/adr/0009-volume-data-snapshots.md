# ADR-0009: data snapshots — labeled volumes, copied cold

- **Status:** accepted
- **Date:** 2026-08-31
- **Applies to:** `mast-engine::volumes`, `Action::{SnapshotServiceData, RestoreServiceData, RemoveServiceDataSnapshot}`, `MastClient::volume_snapshots`, `ServiceState::data_volumes`

## Question

One flag separates a developer from a database they spent a week seeding:
`sail down -v`, a compose "reset", a version-locked volume that has to be
recreated. Every fix for those is destructive, and Mast's own repair arsenal
includes destructive volume operations too. What is missing is the insurance
policy: a copy of the data that is cheap to take, obvious to find, and boring
to store — without turning Mast into a backup product.

Two architectures were on the table: database-aware dumps (`mysqldump`,
`pg_dump` — what native-PHP tools ship) or volume-level copies.

## Decision

**Volume copies, cold.** A snapshot copies every named volume a service
mounts (`ServiceState::data_volumes`, straight from the resolved model) into
fresh volumes named `mast-snap-<group>-<key>`, using a throwaway
`alpine:latest` container and `cp -a`. The service's container is stopped
for the copy and restarted after — a few seconds of downtime buys
crash-free consistency for every engine (MySQL, Postgres, Redis, Mongo,
Meilisearch…) with zero per-engine knowledge, which dumps could never offer.

**Docker is the store.** Snapshot volumes carry `mast.*` labels (project,
service, group, source volume, compose project, timestamp); listing is
`docker volume ls --filter label=mast.snapshot=1`. No sidecar database
means snapshots survive an app-data wipe, are auditable by anyone with the
docker CLI, and clean up with ordinary `docker volume rm`.

**Restore is the guarded half.** It wipes the live volume
(`find /to -mindepth 1 -delete`) and copies back, so: the client arms it in
two steps, offers a fresh snapshot of the current data first (checked by
default — the person restoring is usually mid-mistake already), and the
engine refuses a group whose `mast.project` label names another project. A
deleted original volume is recreated with the `com.docker.compose.*` labels
compose demands before it will adopt a volume it did not make.

Both verbs hold the project's operation lock — they stop and start
containers and must not race a lifecycle op — and restart the container even
when the copy fails. A failed snapshot removes its half-written volumes:
nothing half-copied may ever be mistaken for insurance.

## Consequences

- Cold copies mean seconds of service downtime per snapshot; that is the
  price of engine-agnostic consistency and is stated in the dialog.
- Snapshots live on the same docker host and disk. This is insurance
  against destructive operations, not backup — the ADR title says volumes,
  not vaults.
- `alpine:latest` is pulled on first use through the same streamed operation
  the user is already watching.
- Docker-gated integration coverage for the copy/restore arms is parked with
  the repair-apply suite (same class of test, same reason).
