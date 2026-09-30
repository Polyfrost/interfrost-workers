# Changelog

## Unreleased

- Add Ornithe, kill Legacy Fabric
- Add `java_version` to `PartialVersionInfo`; `merge_partial_version` prefers it over the game's. Ornithe 1.8.9 profiles now state Java 25.
- Bump `CURRENT_ORNITHE_FORMAT_VERSION` to `1` so every Ornithe profile is regenerated with the new field.

## `1.3.1`

- Fix NeoForge and Forge inconsistently being sorted.
- `Box` the `rust_s3` error type for ease of transport.
- Add the new `logging` field to `VersionInfo` which allows `log4j2` XML to be configured.

## `1.3.0`

- Updated the MSRV and migrate to Rust `2024` edition, with `LazyLock`s.
- Bumped dependencies, no breaking changes.
