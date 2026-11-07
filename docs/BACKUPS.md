# Backup before upgrading

For Docker deployments, use `scripts/docker_backup.py` on the Docker host. It
works against the old running containers and does not require an upgraded memex8
binary. Requires Python 3.10+, Docker, and Docker Compose v2 with `up --wait`.

## Create and verify a backup

From the existing deployment directory on `192.168.1.3`:

```sh
python3 scripts/docker_backup.py backup -f docker-compose.yml \
  --output /absolute/path/outside-the-repository/memex8-backups
```

The command saves the current app and Qdrant images, resolved Compose settings,
container configuration, and writable data mounts. It requires `/qdrant/storage`
and `/var/lib/memex8` to be persistent mounts. Other writable mounts such as the
digest directory are included; `/watch` source documents are excluded. Back up
your Obsidian/source files separately. Custom data in unmounted container paths,
read-only mounts, external databases, and other services such as Ollama are not
included. This is for a single memex8 + single Qdrant Compose deployment, not a
distributed Qdrant cluster or a database shared with other live writers.

During the data copy, memex8 stops first, then Qdrant. Both restart afterward if
they were running originally. Plan for downtime proportional to database size.
Do not run parallel upgrades/backups or another process that writes the volumes.

A successful backup directory has `COMPLETE`, SHA-256 checksums, volume tar files,
`images.tar`, and `manifest.json`. Files contain memories **and credentials**;
the command creates private directories/files. Keep backups outside the repository
and copy a verified backup to another disk/computer. There is no automatic
retention/deletion: users decide which recovery points to keep.

```sh
python3 scripts/docker_backup.py verify --backup /path/to/backup-directory
```

Checksums catch corruption, not malicious modifications. Restore only trusted
backups. A checksum pass is not a substitute for a restore drill.

## Test recovery without touching production

```sh
python3 scripts/docker_backup.py restore \
  --backup /path/to/backup-directory --output /path/to/new-recovery-directory
docker compose -f /path/to/new-recovery-directory/compose.recovery.json up -d
```

Restore loads the saved images and imports data into **new, uniquely named
volumes**. It creates a recovery Compose file but does not start it automatically.
The restored app is available only on `127.0.0.1:18080`; use the backed-up API key.
Check memory counts, representative IDs, tags, provenance, and search results.
The recovery file retains model-provider settings; those services must remain
reachable for semantic search. Review host mounts, external dependencies, and
custom ports for your deployment before starting the recovery stack.

To roll back after a failed upgrade, first preserve any post-backup writes you
need, then stop the new app. Once the recovery stack is verified, switch its port
mapping to your normal LAN endpoint. Do not point a newer Qdrant binary at old
data for a rollback: this recovery stack uses the saved Qdrant image. Original
production volumes remain available throughout. Stop the recovery stack when
finished; its external volumes are deliberately not automatically deleted.

## Build and upgrade the application

After the GitHub changes have passed CI and been reviewed, update the host's
source checkout to the chosen commit. Preserve local Compose/env overrides and
use the existing Compose project name and volumes. Build an explicitly named
image first; image construction does not replace the running app:

```sh
docker build -t memex8:CHOSEN_COMMIT .
python3 scripts/docker_backup.py upgrade -f docker-compose.yml \
  --output /absolute/path/outside-the-repository/memex8-backups \
  --image memex8:CHOSEN_COMMIT
```

The upgrade override preserves the running app container's environment, including
its embedding provider, model, dimensions, and credentials. This prevents stale
or conflicting Compose files from changing the embedding model during upgrade.
Keep the same embedding model and dimensions as the existing database; using a
different model requires a deliberate migration of the stored vectors.

Substitute a real commit/tag for `CHOSEN_COMMIT`. This makes a fresh verified
backup, then starts that image with the existing service configuration and waits
for its `/health` endpoint. Qdrant is not upgraded; volumes are never removed.
If backup fails, the upgrade does not run. If app startup fails, the command exits
with failure and leaves the backup available; there is no automatic data rollback.
Health confirms the process is responding, not that every memory query works.
Run authenticated search and a cross-client smoke test before calling it done.

The command prints the generated `upgrade.override.json` path. Include that
override in subsequent Compose commands, or merge its image/healthcheck settings
into your deployment configuration, so a later `up` does not revert the image.
Never use `docker compose down -v` during an upgrade.

## Portable logical backups

The existing CLI also provides `memex8 backup DIRECTORY`, `memex8 backup-list`,
and `memex8 restore ARCHIVE`. This revision preserves all serialized memory
metadata on restore, validates required JSON files before writes, and finalizes
the gzip archive before publishing it. The CLI retains seven archives per backup
directory. It exports memories with vectors, realms, graph edges, and config;
it is **not** a complete database snapshot, does not restore config automatically,
and does not include every auxiliary collection or Docker image.

CLI restore upserts matching IDs and leaves unrelated records in place. It is
not transactional and is not a clean replacement of the existing database. Use
an empty compatible instance and stop writers when making/testing a logical
backup. For upgrade rollback, prefer the Docker cold backup above.
