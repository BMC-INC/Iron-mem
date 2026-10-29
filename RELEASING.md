# Cutting a release

GitHub Releases are created by [`.github/workflows/release.yml`](.github/workflows/release.yml) when a version tag is pushed. This document does not publish a release. Do not tag until the maintainer has reviewed the commit that should ship.

The workflow does not publish to crates.io. Crate publication is a separate step and is not automated.

## What the workflow builds

A tag matching `v*.*.*` (for example `v0.4.0`) runs `cargo build --release --locked` and attaches these archives plus `SHA256SUMS`:

| Archive | Platform |
| --- | --- |
| `ironmem-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 (glibc) |
| `ironmem-x86_64-apple-darwin.tar.gz` | macOS Intel |
| `ironmem-aarch64-apple-darwin.tar.gz` | macOS Apple silicon |
| `ironmem-x86_64-pc-windows-msvc.zip` | Windows x86_64 |

Each archive contains the `ironmem` binary (`ironmem.exe` on Windows), README and LICENSE. The default feature set is used, so the binaries do not include the optional `local-onnx` or `gpu` features. Linux aarch64 is not built.

The release job refuses to publish if any of those four archives is missing. `SHA256SUMS` is generated from the staged files on the release runner.

## How to cut a release

1. Set `version` in `Cargo.toml` and `Cargo.lock` to the version you are shipping, and add `docs/releases/vX.Y.Z.md`. The workflow verifies that the tag matches the package version. CI on `main` should already be green (`cargo test` and `cargo clippy --all-targets -- -D warnings`).
2. Merge the changes that belong in the release.
3. Tag that commit and push the tag:

```bash
git checkout main
git pull
git tag vX.Y.Z
git push origin vX.Y.Z
```

4. Watch the Release workflow. On success it opens a GitHub Release for that tag and uploads the archives and `SHA256SUMS`.
5. Check that all four archives and the checksum file are on the release before announcing it.

If a published build is wrong, do not move the existing tag. Fix `main`, bump the version, and tag the new commit.

## Install from a release binary

Download the archive for the machine, check it against `SHA256SUMS`, and put `ironmem` or `ironmem.exe` on `PATH`. The source installers (`install.sh`, `install.ps1`) still build from a git checkout and print how to add `~/.ironmem/bin` to `PATH`. macOS and Windows may warn about an unsigned binary the first time it runs.
