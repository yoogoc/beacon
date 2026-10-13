# Beacon patch to kube-client 4.2.0

This directory contains the crates.io `kube-client` 4.2.0 source, from kube-rs
commit `526f1f2cfa996ed0e92510feabb2d2327df5e49f`, with its normalized manifest.
Its Apache-2.0 license is preserved in `LICENSE`.

`src/client/auth/mod.rs` adds `CREATE_NO_WINDOW` for noninteractive exec
credential plugins on Windows. These plugins already use captured stderr,
so this does not affect the stderr inheritance needed by interactive plugins
([upstream issue #1901](https://github.com/kube-rs/kube/issues/1901)). Interactive
plugins retain upstream behavior and its existing environment-variable opt-in.

Beacon marks `aws eks get-token` and `aws-iam-authenticator token` as noninteractive
on its in-memory Windows client configuration, unless the kubeconfig explicitly
requires `Always`.
Arguments, environment, token caching and refresh stay with kube-rs. The user's
kubeconfig is never written, and no process-wide environment override is used.

The workspace patches only kube-client, retaining kube/core/runtime 4.2.0.
Remove this snapshot when upstream offers the same noninteractive spawn policy.

Regression checks run with `cargo test --locked -p beacon-kube --lib credentials`.
On Windows this also launches a fixture credential plugin, verifies that
`GetConsoleWindow()` returns null, checks the noninteractive exec request, and
checks that stderr remains available in authentication failure diagnostics.
