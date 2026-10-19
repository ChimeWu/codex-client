# AGENTS.md

[Chinese (Simplified)](docs/zh-CN/AGENTS.md)

## Project scope

`codex-client-cw` is a general-purpose Rust client for the Codex App Server. It connects over a WebSocket on a Unix domain socket to an **already-running** App Server. It does not start, stop, or supervise the daemon, and it contains no Telegram or other product-specific logic.

The client provides generic JSON-RPC calls, one receiver for all server events, and protocol types. Callers own message dispatch, thread subscriptions, approval and sandbox policy, message presentation, and decisions about which capabilities to expose. Keep those product decisions out of this crate.

## Repository layout

- `src/client/`: client actor, connection management, RPC handling, and the unified event stream.
- `src/generated/`: checked-in protocol types and protocol surface inventory generated from the App Server JSON Schema.
- `src/bin/protocol-codegen/`: protocol type generator.
- `src/tests.rs`: serialization tests and ignored tests that require a real daemon.
- `docs/protocol-codegen.md`: authoritative guide to protocol upgrades and code generation.
- `.agents/skills/protocol-codegen/`: Codex skill for protocol maintenance.

## Development guidelines

- Follow Rust 2024 and the MSRV declared in `Cargo.toml` (1.85).
- Treat `src/generated/` as generated output. Read `docs/protocol-codegen.md` before changing the schema or generator; do not hand-edit generated files to patch generation results.
- When protocol output changes, review `src/generated/SURFACE.md` and the Rust type diffs for method, response, and field changes.
- Keep dependencies and implementation focused on the client role. Check whether the standard library or an existing dependency is sufficient before adding a dependency.
- Preserve generated App Server message types in the public API. Do not add thread-based event classification or convenience routing; consumers dispatch the single event stream themselves.
- Keep the actor limited to transport, RPC response correlation, and event delivery. Do not restore subscriptions or replay requests after reconnect.
- The event stream is unbounded and single-consumer. Keep server requests paired with their one-shot reply handles; dropping a handle sends a conservative refusal.
- Do not add a fake App Server, fake-server test support, or a public `testing` feature. Default tests must not require a running Codex daemon.
- Keep real-daemon tests marked `#[ignore]`. They may connect only to a daemon started by the caller; tests must never start or stop it.
- Write project documentation, source comments, and log messages in English. Preserve API names, protocol method names, and identifiers.

## Validation commands

Run checks appropriate to the change. Rust tests are not required for documentation-only changes.

```sh
cargo fmt --all -- --check
cargo check --all-features
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

`cargo test` does not run the ignored real-daemon tests. To validate against a real daemon, set `CODEX_APP_SERVER_ENDPOINT` and explicitly run:

```sh
cargo test real_daemon_ -- --ignored --nocapture
```

These integration tests require a real environment and may consume quota. Do not unignore them in routine automated checks or start a daemon automatically.
