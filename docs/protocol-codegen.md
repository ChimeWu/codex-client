# Protocol code generation and maintenance

[Chinese (Simplified)](zh-CN/protocol-codegen.md)

This guide is the authoritative maintenance reference for generating the Codex App Server protocol types used by `codex-client-cw`. The README and the Codex/Claude skills point here.

Generated files are checked in. Consumers do not need the Codex CLI, and code generation is not part of the build.

## When to regenerate

- After upgrading the Codex CLI, including a minor or patch release.
- When a response field cannot be parsed or a method is missing from the generated request API.
- When `Event::Unparsed` becomes more common, which may mean the server is sending protocol values this crate does not know yet.

There is no need to regenerate when the protocol has not changed; identical input produces identical output.

## Upgrade workflow

Run these steps in order:

```sh
# 1. Record the CLI and daemon versions. They may differ.
codex --version
codex app-server daemon version

# 2. Export the schema.
codex app-server generate-json-schema --out /tmp/codex-schema

# 3. Regenerate the checked-in Rust types.
cargo run --bin protocol-codegen -- /tmp/codex-schema src/generated

# 4. Review the protocol surface before proceeding.
git diff --stat src/generated/
git diff src/generated/SURFACE.md

# 5. Compile and run local checks.
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings

# 6. Validate against the real daemon (this may consume quota).
export CODEX_APP_SERVER_ENDPOINT="unix://$HOME/.codex/app-server-control/app-server-control.sock"
cargo test real_daemon_ -- --ignored --nocapture
```

Real-daemon validation is the only step that checks generated request and response types against a live server. Unit tests cannot replace it. The integration tests connect only to an already-running daemon; they never start or stop one.

The generator runs `rustfmt` on each generated Rust file. If `rustfmt` is unavailable, it warns and continues; run `cargo fmt` manually in that case.

## Reviewing `SURFACE.md`

`src/generated/SURFACE.md` exists to make protocol-level changes easy to review:

| Diff | Meaning | Action |
| --- | --- | --- |
| A method was added | The protocol gained a method | Usually none; `client.request` accepts its generated parameter type. |
| A method was removed | The protocol removed a method | Check downstream callers. |
| A parameter or response type changed | A request shape changed | Review all affected call sites. |
| A response became `serde_json::Value` | Response inference failed | Add or correct an entry in `RESPONSE_OVERRIDES`. |
| The type count dropped sharply | The schema may be incomplete or generation failed | Stop and inspect the schema and generator output. |

Field-level changes are visible in the `types.rs` diff, not in `SURFACE.md`.

## Generator model

The schema is exported from Codex Rust types with `ts-rs`, so it uses a limited set of regular shapes. The generator maps those shapes to Rust and Serde:

| Schema shape | Generated Rust | Generator logic |
| --- | --- | --- |
| `$ref: "#/definitions/Foo"` | `Foo` | `rust_type` |
| `anyOf: [T, null]` | `Option<T>` | `nullable_inner` |
| `type: ["string", "null"]` | `Option<String>` | `is_nullable_scalar` |
| `allOf: [T]` (used by `ts-rs` for descriptions) | `T` | `single_all_of` |
| `oneOf` variants with a shared single-value enum field | Internally tagged enum | `internally_tagged` |
| `oneOf` with string values or single-key objects | Externally tagged enum | `externally_tagged` |
| Other `oneOf` / `anyOf` forms | `#[serde(untagged)]` enum | `untagged_union` |
| String enum | Unit variants with `#[serde(rename)]` | `string_enum` |
| Object with `additionalProperties` | `BTreeMap<String, T>` | `rust_type` |
| Inline anonymous object | Named type derived from its parent and field | `derive` / `pending` |

Additional rules:

- Struct fields are public; enum variant fields are not, as required by Rust syntax.
- `Default` is derived only when every field type has a safe default. Choosing arbitrary defaults for protocol enums would invent semantics.
- Variant names are deduplicated after PascalCase conversion. The original wire value remains in `#[serde(rename)]`.
- Bare boolean schemas are valid. `normalize()` converts schema-position booleans to empty schemas; `additionalProperties` booleans retain their separate meaning.

## Manually maintained mappings

### Response type inference

The schema exports types but does not map each request to its response type. The generator infers this from `XxxParams` → `XxxResponse` naming. Exceptions belong in `RESPONSE_OVERRIDES` in `src/bin/protocol-codegen/main.rs` (currently 13 entries).

Generation reports methods that fall back to `serde_json::Value`. When that happens, inspect the schema for a matching response type or query the real daemon, then add the correct override. A `Value` fallback compiles but removes the type guarantee, so do not ignore it.

### Parameter type collisions

`Request` is implemented on the parameter type itself, so one parameter type cannot represent two methods. The generator stops if two methods share a `*Params` type. Add a newtype for one method and forward `Serialize`, or redesign that parameter family; do not bypass the check and generate conflicting trait implementations.

Methods with `params: null` get a unit struct named from the method (for example, `account/logout` becomes `AccountLogoutParams`). Serde serializes it as `null`. Generation stops if that name collides with a schema type.

## Troubleshooting generator changes

| Symptom | Likely cause | Code to inspect |
| --- | --- | --- |
| `invalid type: boolean, expected struct Schema` | A bare boolean appeared in a new schema position | `normalize()` and the schema shape tables in `main.rs` |
| `visibility qualifiers are not permitted here` | An enum variant field was emitted as `pub` | `render.rs::field` |
| `the trait bound X: Default is not satisfied` | A type with a non-defaultable enum field derived `Default` | `render.rs::is_defaultable` |
| `the name X is defined multiple times` | Converted variant names collided | `render.rs::unique_variant` |
| Generated `Variant0(...)` / `Variant1(...)` names | A `oneOf` form was not recognized | `render.rs::union` |
| A type unexpectedly became `serde_json::Value` | A `$ref` target is absent from the known definitions | Check whether the schema is self-contained. |

## Breaking protocol changes

When Codex changes the shape of an existing type:

1. Review both `SURFACE.md` and `types.rs`; the surface file does not list field changes.
2. Fix compile errors, then review downstream adapters and real-daemon tests.
3. Update fixtures with newly required fields. Do not make fields optional just to avoid updating fixtures.
4. Use real-daemon tests as the final compatibility check.

If a protocol change prevents an adapter from preserving existing behavior, record the decision in an ADR instead of silently adding compatibility logic.

## Design notes

- The generator handles the schema constructs used here directly, which keeps naming, documentation, and Serde behavior predictable without a build-time dependency on a general-purpose schema compiler.
- Generated files are checked in so consumers do not need the Codex CLI and protocol changes remain visible as ordinary source diffs.
- Generated enums do not use an `Unknown` catch-all. `#[serde(other)]` does not safely handle the adjacent-tagged message shapes used here. Unparseable inbound messages instead become `Event::Unparsed(Value)` and are passed through without terminating the connection.
- The schema itself is not checked in: it contains hundreds of files and can be exported again from the CLI. `SURFACE.md` is the compact versioned record of the protocol surface.
