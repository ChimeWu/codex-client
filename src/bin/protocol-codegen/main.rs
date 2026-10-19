//! 从 `codex app-server generate-json-schema` 的产物生成 Rust 协议类型。
//!
//! ```sh
//! codex app-server generate-json-schema --out /tmp/codex-schema
//! cargo run -p codex-client-cw --bin protocol-codegen -- /tmp/codex-schema src/generated
//! ```
//!
//! 生成结果是 check-in 的：使用这个 crate 的项目不需要装 codex CLI，也不需要
//! 在构建期跑代码生成。codex 升级后重跑本程序即可。

mod render;
mod schema;

use anyhow::{Context, Result};
use render::{Renderer, doc_comment, field_name, pascal, type_name};
use schema::{Schema, reference_name};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 无参请求（schema 里 `params` 是 `{"type":"null"}`）之外，命名约定失效的
/// 方法。左边是请求的 `params` 类型基名或方法名，右边是响应类型名。
///
/// 这些是 schema 本身没有表达的关联关系——ts-rs 只导出类型，不导出
/// 「哪个请求配哪个响应」，因此只能按名字推断，推断不出来的写在这里。
const RESPONSE_OVERRIDES: &[(&str, &str)] = &[
    ("account/gatewayOAuth/read", "GatewayOAuthReadResponse"),
    ("account/gatewayOAuth/login", "GatewayOAuthLoginResponse"),
    ("account/gatewayOAuth/cancel", "GatewayOAuthCancelResponse"),
    ("account/logout", "LogoutAccountResponse"),
    ("account/rateLimits/read", "GetAccountRateLimitsResponse"),
    ("account/usage/read", "GetAccountTokenUsageResponse"),
    (
        "account/workspaceMessages/read",
        "GetWorkspaceMessagesResponse",
    ),
    ("config/mcpServer/reload", "McpServerRefreshResponse"),
    ("config/batchWrite", "ConfigWriteResponse"),
    ("config/value/write", "ConfigWriteResponse"),
    ("configRequirements/read", "ConfigRequirementsReadResponse"),
    (
        "externalAgentConfig/import/readHistories",
        "ExternalAgentConfigImportHistoriesReadResponse",
    ),
    (
        "windowsSandbox/readiness",
        "WindowsSandboxReadinessResponse",
    ),
];

const HEADER: &str = "// 由 protocol-codegen 生成，请勿手工修改。\n\
                      // 重新生成：\n\
                      //   codex app-server generate-json-schema --out <dir>\n\
                      //   cargo run -p codex-client-cw --bin protocol-codegen -- <dir> src/generated\n\n";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let schema_dir = PathBuf::from(
        args.next()
            .context("用法：protocol-codegen <schema 目录> <输出目录>")?,
    );
    let out_dir = PathBuf::from(args.next().context("缺少输出目录参数")?);

    let (definitions, roots) = load(&schema_dir)?;
    println!("读入 {} 个类型定义", definitions.len());

    let known: BTreeSet<String> = definitions.keys().map(|name| type_name(name)).collect();
    let mut renderer = Renderer::new(known);

    // 1. 全部类型
    let mut body = String::new();
    for (name, schema) in &definitions {
        body.push_str(&renderer.definition(name, schema));
        body.push('\n');
    }
    // 渲染过程中派生出来的内联类型也要生成；它们自己可能再派生，因此循环到不动点。
    while !renderer.pending.is_empty() {
        let batch = std::mem::take(&mut renderer.pending);
        for pending in batch {
            body.push_str(&renderer.definition(&pending.name, &pending.schema));
            body.push('\n');
        }
    }
    let types = format!(
        "{HEADER}//! 协议里出现的全部数据类型。\n\n\
         #![allow(clippy::large_enum_variant)]\n\n\
         use serde::{{Deserialize, Serialize}};\n\n{body}"
    );

    // 2. 请求：method → (params, response)
    let client_request = roots
        .get("ClientRequest")
        .context("schema 目录里缺少 ClientRequest.json")?;
    let (methods, request_surface) = render_requests(client_request, &definitions)?;

    // 3. 服务端通知与服务端请求
    let (notifications, notification_surface) = render_incoming(
        "ServerNotification",
        roots
            .get("ServerNotification")
            .context("缺少 ServerNotification.json")?,
        "服务端推送的通知。不需要回复。",
    )?;
    let (server_requests, server_request_surface) = render_incoming(
        "ServerRequest",
        roots
            .get("ServerRequest")
            .context("缺少 ServerRequest.json")?,
        "服务端反向发起的请求。**必须回复**，否则服务端会一直阻塞等待。",
    )?;

    std::fs::create_dir_all(&out_dir)?;
    write(&out_dir.join("types.rs"), &types)?;
    write(&out_dir.join("methods.rs"), &methods)?;
    write(
        &out_dir.join("events.rs"),
        &format!(
            "{HEADER}//! 入站消息的类型化封装。\n\n\
             use super::types::*;\n\
             use serde::{{Deserialize, Serialize}};\n\n{notifications}\n{server_requests}"
        ),
    )?;
    write(
        &out_dir.with_extension("rs"),
        &format!(
            "{HEADER}//! 从 JSON Schema 生成的协议定义。\n\
             //!\n\
             //! 这些类型覆盖 App Server 协议的全部表面：`types` 是数据类型，\n\
             //! `methods` 是每个客户端请求的方法名与参数/响应类型，`events` 是\n\
             //! 服务端通知与服务端请求。\n\n\
             pub mod events;\n\
             pub mod methods;\n\
             pub mod types;\n"
        ),
    )?;
    write(
        &out_dir.join("SURFACE.md"),
        &surface(
            &request_surface,
            &notification_surface,
            &server_request_surface,
            &definitions,
        ),
    )?;
    println!("已写入 {}", out_dir.display());
    Ok(())
}

fn write(path: &Path, contents: &str) -> Result<()> {
    let contents = format!("{}\n", contents.trim_end());
    std::fs::write(path, &contents).with_context(|| format!("写入 {}", path.display()))?;
    // 生成物必须是 rustfmt 干净的，否则每次重新生成都会在 `cargo fmt` 下产生
    // 一堆与协议无关的 diff，把真正的协议变化淹掉。
    if path.extension().is_some_and(|ext| ext == "rs") {
        match std::process::Command::new("rustfmt")
            .args(["--edition", "2024"])
            .arg(path)
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => println!(
                "  警告：rustfmt 退出码 {status}，{} 未格式化",
                path.display()
            ),
            Err(error) => println!(
                "  警告：找不到 rustfmt（{error}），{} 未格式化",
                path.display()
            ),
        }
    }
    println!("  {} ({} 字节)", path.display(), contents.len());
    Ok(())
}

/// 读入 schema 目录下的全部单类型文件，合并它们的 definitions。
///
/// 每个文件都是自包含的（root schema + 它引用到的全部 definitions），因此同名
/// definition 会在多个文件里重复出现；它们来自同一个 Rust 类型，取先到的即可。
fn load(dir: &Path) -> Result<(BTreeMap<String, Schema>, BTreeMap<String, Schema>)> {
    let mut definitions: BTreeMap<String, Schema> = BTreeMap::new();
    let mut roots: BTreeMap<String, Schema> = BTreeMap::new();
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    files.sort();

    for file in files {
        let name = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned();
        // 两个聚合文件的内容已经被单类型文件覆盖，跳过以免重复。
        if name.starts_with("codex_app_server_protocol") {
            continue;
        }
        let text = std::fs::read_to_string(&file)?;
        let mut raw: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("读取 {}", file.display()))?;
        normalize(&mut raw);
        let schema: Schema =
            serde_json::from_value(raw).with_context(|| format!("解析 {}", file.display()))?;
        for (definition, sub) in &schema.definitions {
            definitions
                .entry(definition.clone())
                .or_insert_with(|| sub.clone());
        }
        let mut root = schema.clone();
        root.definitions = BTreeMap::new();
        let root_name = root.title.clone().unwrap_or_else(|| name.clone());
        definitions
            .entry(root_name.clone())
            .or_insert_with(|| root.clone());
        roots.insert(root_name, root);
    }
    Ok((definitions, roots))
}

/// JSON Schema 允许用裸 `true`/`false` 当作「任意」/「永不匹配」的 schema，
/// 例如 MCP 工具的 `inputSchema`。把 schema 位置上的裸 bool 归一成空对象，
/// 后续就能当普通 schema 处理（空 schema → `serde_json::Value`）。
///
/// `additionalProperties` 不在此列：那里的 bool 有独立含义，由渲染器自己看。
fn normalize(value: &mut serde_json::Value) {
    const SCHEMA_MAPS: &[&str] = &["properties", "definitions", "patternProperties"];
    const SCHEMA_VALUES: &[&str] = &["items", "additionalItems", "not", "if", "then", "else"];
    const SCHEMA_LISTS: &[&str] = &["oneOf", "anyOf", "allOf"];

    let serde_json::Value::Object(object) = value else {
        return;
    };
    for (key, child) in object.iter_mut() {
        if SCHEMA_MAPS.contains(&key.as_str()) {
            if let serde_json::Value::Object(entries) = child {
                for entry in entries.values_mut() {
                    normalize_schema(entry);
                }
            }
        } else if SCHEMA_VALUES.contains(&key.as_str()) {
            normalize_schema(child);
        } else if SCHEMA_LISTS.contains(&key.as_str()) {
            if let serde_json::Value::Array(items) = child {
                for item in items {
                    normalize_schema(item);
                }
            }
        } else if key == "additionalProperties" && child.is_object() {
            normalize(child);
        }
    }
}

fn normalize_schema(value: &mut serde_json::Value) {
    if value.is_boolean() {
        *value = serde_json::json!({});
        return;
    }
    normalize(value);
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path);
        }
    }
    Ok(())
}

/// 为每个客户端请求生成一个 `Request` 实现——实现在**参数类型本身**上。
///
/// 参数类型唯一确定方法名和响应类型，因此不需要另立一个 marker 类型来捆绑三
/// 者；调用方写 `client.request(ThreadStartParams { … })` 即可，无需 turbofish。
/// 这依赖「一个参数类型只属于一个方法」，schema 变化打破它时会在这里报错。
///
/// 无参方法（schema 里 `params` 是 `null`）没有参数类型可实现，为它们生成一个
/// unit struct；serde 把 unit struct 序列化成 `null`，与协议一致。
fn render_requests(
    client_request: &Schema,
    definitions: &BTreeMap<String, Schema>,
) -> Result<(String, Vec<String>)> {
    let overrides: BTreeMap<&str, &str> = RESPONSE_OVERRIDES.iter().copied().collect();
    let variants = client_request
        .one_of
        .as_ref()
        .context("ClientRequest 应该是一个 oneOf")?;

    let mut body = String::new();
    let mut listing = String::new();
    let mut surface = Vec::new();
    let mut unmatched = Vec::new();
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    let mut count = 0usize;

    for variant in variants {
        let properties = match &variant.properties {
            Some(properties) => properties,
            None => continue,
        };
        let Some(method) = properties.get("method").and_then(Schema::const_string) else {
            continue;
        };
        let params_schema = properties.get("params");

        // 参数类型：$ref 直接用；`{"type":"null"}` 表示无参；anyOf 表示可选。
        // 无参方法的参数类型由方法名造，其余从 schema 读。注意别用名字相等去
        // 判断哪种情况：`initialize` 的真实参数类型恰好就叫 `InitializeParams`。
        let mut nullary = false;
        let params = match params_schema {
            // 没有 params，或 `{"type":"null"}`：无参方法。
            None => {
                nullary = true;
                format!("{}Params", method_type_name(&method))
            }
            Some(schema) if schema.is_type("null") => {
                nullary = true;
                format!("{}Params", method_type_name(&method))
            }
            Some(schema) => match &schema.reference {
                Some(reference) => reference_name(reference)
                    .map(type_name)
                    .unwrap_or_else(|| "serde_json::Value".to_owned()),
                None => match schema
                    .nullable_inner()
                    .and_then(|inner| inner.reference.as_deref())
                {
                    Some(reference) => format!(
                        "Option<{}>",
                        reference_name(reference)
                            .map(type_name)
                            .unwrap_or_else(|| "serde_json::Value".to_owned())
                    ),
                    None => "serde_json::Value".to_owned(),
                },
            },
        };

        // 两个方法共用一个参数类型的话，`impl Request for` 会撞车。真出现了得给
        // 其中一个套 newtype，或者退回 marker 类型——总之不能默默生成不编译的
        // 代码，所以在这里停下。
        if let Some(previous) = owners.insert(params.clone(), method.clone()) {
            anyhow::bail!(
                "`{previous}` 与 `{method}` 共用参数类型 `{params}`，无法各自实现 Request；\
                 需要为其中一个套 newtype，见 docs/protocol-codegen.md"
            );
        }
        // 无参类型是我们凭空造的，撞上 schema 里已有的名字会让 `use types::*`
        // 被本地定义悄悄遮蔽。
        if nullary && definitions.contains_key(&params) {
            anyhow::bail!("为无参方法 `{method}` 生成的 `{params}` 与 schema 里的同名类型冲突");
        }

        // 响应类型：先按命名约定，再查覆盖表。
        let base = params
            .trim_start_matches("Option<")
            .trim_end_matches('>')
            .trim_end_matches("Params");
        let by_convention = format!("{base}Response");
        let response = if let Some(explicit) = overrides.get(method.as_str()) {
            (*explicit).to_owned()
        } else if definitions.contains_key(&by_convention) {
            by_convention
        } else {
            unmatched.push(method.clone());
            "serde_json::Value".to_owned()
        };

        let doc = doc_comment(variant.description.as_deref(), 0);
        if nullary {
            body.push_str(&format!(
                "{doc}/// `{method}` 的参数：这个方法没有参数，序列化为 `null`。\n\
                 #[derive(Clone, Copy, Debug, Default, Serialize)]\n\
                 pub struct {params};\n\n"
            ));
        }
        body.push_str(&format!(
            "{doc}/// `{method}`\n\
             impl Request for {params} {{\n\
             \x20   const METHOD: &'static str = \"{method}\";\n\
             \x20   type Response = {response};\n\
             }}\n\n"
        ));
        // `Option<T>` 不是一个能链接的名字，退化成代码。
        if params.contains('<') {
            listing.push_str(&format!("//! - `{method}` → `{params}`\n"));
        } else {
            listing.push_str(&format!("//! - `{method}` → [`{params}`]\n"));
        }
        surface.push(format!("| `{method}` | `{params}` | `{response}` |"));
        count += 1;
    }

    if !unmatched.is_empty() {
        println!(
            "  注意：{} 个方法没有匹配到响应类型，回退为 serde_json::Value：",
            unmatched.len()
        );
        for method in &unmatched {
            println!("    {method}");
        }
    }
    println!("  生成 {count} 个客户端请求");

    Ok((
        format!(
            "{HEADER}//! 每个客户端请求的参数类型与它对应的方法名、响应类型。\n//!\n\
         //! 共 {count} 个方法：\n\
         //!\n{listing}\n\
         use super::types::*;\n\
         use serde::Serialize;\n\
         use serde::de::DeserializeOwned;\n\n\
         /// 一个客户端 → 服务端的请求。\n\
         ///\n\
         /// 实现在参数类型上：参数类型唯一确定方法名和响应类型，因此\n\
         /// `client.request(ThreadStartParams {{ .. }})` 的返回类型是确定的，\n\
         /// 不需要额外指定方法。\n\
         pub trait Request: Serialize {{\n\
         \x20   const METHOD: &'static str;\n\
         \x20   type Response: DeserializeOwned;\n\
         }}\n\n{body}"
        ),
        surface,
    ))
}

/// 把 `ServerNotification` / `ServerRequest` 的 oneOf 生成成一个带 method 判别的 enum。
fn render_incoming(name: &str, schema: &Schema, doc: &str) -> Result<(String, Vec<String>)> {
    let variants = schema
        .one_of
        .as_ref()
        .with_context(|| format!("{name} 应该是一个 oneOf"))?;
    let mut body = String::new();
    let mut surface = Vec::new();
    let mut count = 0usize;

    for variant in variants {
        let Some(properties) = &variant.properties else {
            continue;
        };
        let Some(method) = properties.get("method").and_then(Schema::const_string) else {
            continue;
        };
        let variant_name = method_type_name(&method);
        let variant_doc = doc_comment(variant.description.as_deref(), 1);
        let payload = properties.get("params").and_then(|params| {
            params
                .reference
                .as_deref()
                .and_then(reference_name)
                .map(type_name)
        });
        match &payload {
            Some(ty) => body.push_str(&format!(
                "{variant_doc}    #[serde(rename = \"{method}\")]\n    {variant_name}({ty}),\n"
            )),
            None => body.push_str(&format!(
                "{variant_doc}    #[serde(rename = \"{method}\")]\n    {variant_name},\n"
            )),
        }
        surface.push(format!(
            "| `{method}` | `{}` |",
            payload.as_deref().unwrap_or("（无 params）")
        ));
        count += 1;
    }
    println!("  生成 {count} 个 {name} 变体");

    Ok((
        format!(
            "/// {doc}\n\
         ///\n\
         /// 只包含本客户端生成时已知的方法。服务端随时可能新增，因此解析失败\n\
         /// **不是**错误——调用方应当保留原始 JSON 继续走，而不是让连接失败。\n\
         #[allow(clippy::large_enum_variant)]\n\
         #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n\
         #[serde(tag = \"method\", content = \"params\")]\n\
         pub enum {name} {{\n{body}}}\n"
        ),
        surface,
    ))
}

/// 协议表面清单。
///
/// 它存在的唯一目的是**便于 diff**：codex 升级后重新生成，`git diff` 这个
/// 文件就能一眼看出协议增删了哪些方法、哪个请求换了参数或响应类型。字段级
/// 的变化看 `types.rs` 自己的 diff。
fn surface(
    requests: &[String],
    notifications: &[String],
    server_requests: &[String],
    definitions: &BTreeMap<String, Schema>,
) -> String {
    let mut types: Vec<_> = definitions.keys().map(|name| type_name(name)).collect();
    types.sort();
    types.dedup();
    let type_list = types
        .iter()
        .map(|name| format!("- `{name}`"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "# 协议表面\n\n\
         由 protocol-codegen 生成，请勿手工修改。**这个文件是用来 diff 的**：\n\
         codex 升级后重新生成，`git diff` 它就能看出协议的方法级变化。\n\n\
         - 客户端请求：{} 个\n\
         - 服务端通知：{} 个\n\
         - 服务端请求：{} 个\n\
         - 数据类型：{} 个\n\n\
         ## 客户端请求\n\n\
         | 方法 | 参数 | 响应 |\n| --- | --- | --- |\n{}\n\n\
         ## 服务端通知\n\n\
         | 方法 | payload |\n| --- | --- |\n{}\n\n\
         ## 服务端请求\n\n\
         这些**必须回复**，否则服务端一直阻塞。\n\n\
         | 方法 | payload |\n| --- | --- |\n{}\n\n\
         ## 数据类型\n\n{}\n",
        requests.len(),
        notifications.len(),
        server_requests.len(),
        types.len(),
        requests.join("\n"),
        notifications.join("\n"),
        server_requests.join("\n"),
        type_list,
    )
}

/// `thread/start` → `ThreadStart`，`item/agentMessage/delta` → `ItemAgentMessageDelta`。
fn method_type_name(method: &str) -> String {
    method.split('/').map(pascal).collect::<Vec<_>>().join("")
}

#[allow(dead_code)]
fn unused(_: fn(&str) -> String) {
    let _ = field_name;
}
