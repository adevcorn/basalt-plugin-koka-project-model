# koka-project-model

Koka project-model, build-system, and test-runner WASM plugin for Basalt.

Provides `project-model:koka` and activates on `koka.toml`, `koka.yaml`, or `**/*.kk`.
Exposes `koka-check` (`koka -c`) and `koka-test` (`koka -e`) review actions, plus `.koka` shadow exclusions.

## Build

```sh
cargo build --release --target wasm32-unknown-unknown
```
