# Packaging Snow

This document is intended for distribution maintainers packaging Snow.

## Package name

The package should be called **`snowemu`**.

## Upstream information

- Homepage: https://snowemu.com/
- Documentation: https://docs.snowemu.com/
- Source: https://github.com/twvd/snow
- Issue tracker: https://github.com/twvd/snow/issues
- Donation link: https://ko-fi.com/twvd
- License: MIT
- Creator/developer: Thomas W.
- AppStream ID: `dev.thomasw.snow`
- Desktop file ID: `snow.desktop`

## Building

See [BUILDING.md](docs/BUILDING.md) for build prerequisites, instructions, and
available feature flags.

Distribution packages should use the default feature flags.

Packaged builds must be built in release mode with LTO enabled. Debug symbols
may be stripped if preferred.

## Installed files

| Source | Install to | Description |
|---|---|---|
| `target/release/snowemu` | `/usr/bin/snowemu` | Main binary |
| `assets/snow.desktop` | `/usr/share/applications/snow.desktop` | Desktop entry |
| `assets/snow_icon.png` | `/usr/share/icons/hicolor/1024x1024/apps/snow_icon.png` | Application icon |
| `assets/dev.thomasw.snow.metainfo.xml` | `/usr/share/metainfo/dev.thomasw.snow.metainfo.xml` | AppStream metadata |

## Versioning

The version is tracked in `frontend_egui/Cargo.toml`. Releases are tagged in
the git repository and are the recommended packaging points.

## Notes

- The workspace `Cargo.toml` patches several `egui` crates with a custom fork
  (see `[patch.crates-io]`). This is intentional and should not be overridden
  as there are Snow-specific fixes in there.
- Snow pins a specific Rust toolchain version via `rust-toolchain.toml`.
  Using a different version may cause build failures or behavioral differences.
