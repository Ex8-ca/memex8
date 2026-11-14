# Component release policy

Engine and Hermes plugin versions are separate release tracks. Do not synchronize their version numbers merely because they share a repository.

## Verified release baseline

Read-only remote checks during roadmap implementation found:

- Engine tag `v0.2.0`, peeled commit `e0469b142056bb3ce9cf3e0ebd742b91b3daac84`.
- That tagged `Cargo.toml` declares engine `0.2.0`.
- That tagged `plugins/memex8/plugin.yaml` declares plugin `1.2.0`.
- Local source baseline `a10528d` has engine manifest `0.1.0`, plugin `1.2.0`. Treat the engine manifest mismatch as stale release metadata to reconcile, not evidence that `v0.2.0` never shipped.

Sources inspected: `git ls-remote --tags origin` and raw files at the exact peeled release commit. This does not verify the deployed server version or current installed plugin.

## Before cutting a checkpoint

1. Review the combined source diff, excluding operator Compose overrides and secrets.
2. Capture real build/test/integration output and record known skipped/flaky tests.
3. Verify engine and plugin compatibility against the intended Hermes/API versions. Changes to either component require the corresponding version, artifact and changelog entry; do not reuse an existing published tag.
4. Reconcile Cargo package version, lockfile, plugin manifest and `_PLUGIN_VERSION` for their respective component changes. Record server schema capabilities separately from package versions.
5. Commit tests/implementation/docs in reviewable concerns, then create a local checkpoint tag only once acceptance criteria are met. Push/public release/deployment are separate actions.
6. Never present a local build or a GitHub release as proof that the live service is upgraded.

## Current status

No new release number has been assigned and no tag created. Milestone 0 behavior is locally verified; live authentication and deployment acceptance remain open. Milestone 1 evidence implementation is in progress behind opt-in behavior.
