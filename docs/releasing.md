# Releasing

Every milestone in `ROADMAP.md` ships as a GitHub release. The release
workflow (`.github/workflows/release.yml`) runs on any `v*` tag and builds:

| Asset | Contents |
|---|---|
| `crabboy-<version>-linux-amd64.tar.gz` | `CrabBoy`, `crab`, README, LICENSE, CHANGELOG |
| `crabboy-<version>-linux-arm64.tar.gz` | same, built on `ubuntu-24.04-arm` |
| `crabboy-<version>-windows-amd64.zip` | `CrabBoy.exe`, `crab.exe`, README, LICENSE, CHANGELOG |
| `crabboy-<version>-windows-arm64.zip` | same, built on `windows-11-arm` |
| `crabboy-<version>-web.zip` | the browser demo with generated `pkg/` bindings |
| `SHA256SUMS` | checksums of every archive |

Stable tags also deploy the browser demo to GitHub Pages. Tags with a
suffix (`v0.2.0-rc.1`) are published as pre-releases and skip Pages.

## Checklist

1. `main` is green in CI (all four native runners, wasm, determinism, msrv).
2. Bump `version` under `[workspace.package]` in `Cargo.toml`; every crate
   inherits it. Run `cargo build --workspace` so `Cargo.lock` follows.
3. In `CHANGELOG.md`, rename the `[Unreleased]` section to
   `[X.Y.Z] - YYYY-MM-DD` and start a fresh empty `[Unreleased]` above it.
   The release notes are taken verbatim from that section.
4. Update the status table in `README.md` and `docs/accuracy.md` (once the
   harness exists) if the milestone changed what passes.
5. Commit: `chore: release vX.Y.Z`.
6. Tag and push:

   ```sh
   git tag -a vX.Y.Z -m "CrabBoy vX.Y.Z"
   git push --follow-tags
   ```

7. Watch the Release workflow. It refuses to build if the tag does not match
   the workspace version.
8. Verify on the Releases page: six archives plus `SHA256SUMS`, and
   `sha256sum -c SHA256SUMS` passes on a downloaded set. Open the Pages URL
   and load a ROM.

Never move or delete a published tag. If a release is broken, fix forward
with a patch version.

## One-time repository setup

- Settings → Pages → Source: **GitHub Actions**.
- Settings → Actions → General → Workflow permissions: read is enough; the
  release and pages jobs request the write scopes they need themselves.
