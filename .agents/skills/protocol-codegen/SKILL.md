---
name: protocol-codegen
description: Generate and maintain codex-client-cw protocol types after a Codex CLI or App Server upgrade, when generated code fails to compile, when response fields stop parsing, when request methods are missing, or when Event::Unparsed becomes more common. Covers the upgrade workflow, SURFACE.md review, RESPONSE_OVERRIDES, parameter type collisions, and generator troubleshooting.
---

# protocol-codegen — protocol type generation and maintenance

Authoritative guide: [docs/protocol-codegen.md](../../../docs/protocol-codegen.md). Chinese guide: [docs/zh-CN/protocol-codegen.md](../../../docs/zh-CN/protocol-codegen.md). A Chinese copy of this skill is also available in [docs/zh-CN/protocol-codegen-skill.md](../../../docs/zh-CN/protocol-codegen-skill.md).

Use this skill after upgrading Codex, when response fields no longer parse, when a method is missing from the generated API, or when `Event::Unparsed` becomes more common. Generated files are checked in: consumers do not need the Codex CLI, and generation does not run during builds.

## Workflow

Follow the complete procedure in the [protocol maintenance guide](../../../docs/protocol-codegen.md). The essential steps are:

1. Record the CLI and running daemon versions with `codex --version` and `codex app-server daemon version`.
2. Export the schema with `codex app-server generate-json-schema --out /tmp/codex-schema`.
3. Regenerate with `cargo run --bin protocol-codegen -- /tmp/codex-schema src/generated`.
4. Review `git diff --stat src/generated/`, `src/generated/SURFACE.md`, and field changes in `types.rs`.
5. Run `cargo test --all-features` and `cargo clippy --all-targets --all-features -- -D warnings`.
6. When a caller-managed daemon is available, run `cargo test real_daemon_ -- --ignored --nocapture`. These tests connect only to an already-running daemon and may consume quota.

Do not hand-edit generated files to patch generator output. If new protocol shapes require generator changes, consult the guide's troubleshooting table. In particular, fix any response type that falls back to `serde_json::Value`, and resolve parameter type collisions without bypassing the generator's checks.
