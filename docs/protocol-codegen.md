# protocol-codegen —— 协议类型生成与维护

> 同样的操作步骤也打包成了 Claude skill（`.claude/skills/protocol-codegen/SKILL.md`）和 Codex skill（`.agents/skills/protocol-codegen/SKILL.md`）。这份文档是权威版本，README 与两份 skill 都指向这里。

从 `codex app-server generate-json-schema` 的产物生成 `codex-client-cw` 的协议类型。**不是运行时依赖**：生成结果已 check-in，用那个 crate 的人既不需要装 codex CLI，也不在构建期跑代码生成。

codex 迭代很快，这份文档是给「下一次 codex 升级之后」的你看的。

## 什么时候需要跑

- codex CLI 升级之后（哪怕只是小版本）
- 发现某个响应字段解析不出来、或某个方法在 `requests::` 里不存在
- `Event::Unparsed` 在日志里变多——服务端在发本客户端不认识的通知

**不需要**每次都跑：协议没变时重新生成会得到逐字节相同的输出。

## 升级流程

```sh
# 1. 确认 CLI 与运行中的 daemon 版本（两者可能不同步！）
codex --version
codex app-server daemon version     # 看 appServerVersion

# 2. 导出 schema
codex app-server generate-json-schema --out /tmp/codex-schema

# 3. 重新生成
cargo run --bin protocol-codegen -- /tmp/codex-schema src/generated

# 4. 先看协议表面变了什么——这一步不要跳过
git diff --stat src/generated/
git diff src/generated/SURFACE.md

# 5. 编译 + 测试
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings

# 6. 对真实 daemon 验证（会消耗额度，但这是唯一能证明类型对得上真实响应的步骤）
export CODEX_APP_SERVER_ENDPOINT="unix://$HOME/.codex/app-server-control/app-server-control.sock"
cargo test real_daemon_ -- --ignored --nocapture
```

第 6 步很关键：fake server 测试只能证明客户端**自洽**，证明不了生成的类型能吃下真实服务端返回的完整响应。历史上每次协议改动都是这一步先炸。

生成器写完每个 `.rs` 会自己调 `rustfmt`，所以生成物永远是 fmt 干净的——重新生成后的 diff 只包含真正的协议变化，不会混进格式噪音。找不到 `rustfmt` 时它会警告并继续，那种情况下记得手动 `cargo fmt`。

### 读 SURFACE.md 的 diff

`generated/SURFACE.md` 存在的唯一目的就是让协议变化一眼可见：

| diff 里看到 | 意味着 | 要做什么 |
| --- | --- | --- |
| 新增一行方法 | 协议加了新方法 | 通常什么都不用做，`client.request(那个 Params)` 自动可用 |
| 删除一行方法 | 协议删了方法 | 检查下游适配层是否调用了该方法 |
| 参数/响应类型名变了 | 请求换了形状 | 检查适配层的调用点 |
| 响应列变成 `serde_json::Value` | 响应类型没推断出来 | 见下面「响应类型推断」 |
| 数据类型数量骤降 | schema 没导全，或生成器出错 | 先别提交，查生成器输出 |

字段级的变化看 `types.rs` 自己的 diff。

## 生成器怎么工作

schema 是 codex 用 [ts-rs](https://github.com/Aleph-Alpha/ts-rs) 从它自己的 Rust 类型导出的，所以形态有限且规整，**天然对应 serde 的默认序列化**。生成器就是把这个映射反过来走：

| Schema 形态 | 生成的 Rust | 处理位置 |
| --- | --- | --- |
| `$ref: "#/definitions/Foo"` | `Foo` | `rust_type` |
| `anyOf: [T, null]` | `Option<T>` | `nullable_inner` |
| `type: ["string","null"]` | `Option<String>` | `is_nullable_scalar` |
| `allOf: [T]`（ts-rs 用它挂 description） | `T` | `single_all_of` |
| `oneOf`，每个变体都有同名单值 enum 字段 | `#[serde(tag = "…")]` enum | `internally_tagged` |
| `oneOf`，变体是字符串取值或单键对象 | serde 默认 externally tagged enum | `externally_tagged` |
| 其它 `oneOf` / `anyOf` | `#[serde(untagged)]` enum | `untagged_union` |
| 多值字符串 `enum` | unit 变体 + `#[serde(rename)]` | `string_enum` |
| `type: "object"` + `additionalProperties` | `BTreeMap<String, T>` | `rust_type` |
| 内联的匿名 object | 派生一个 `父名+字段名` 的具名类型 | `derive` / `pending` |

几条不那么显然的规则：

- **字段可见性**：struct 字段带 `pub`，enum 变体里的字段不带——Rust 不允许。`field()` 的 `public` 参数控制。
- **`Default` 只在安全时 derive**：所有字段类型都自带 `Default` 才加（`is_defaultable`）。协议里大量字段是 enum，给 enum 硬选一个默认变体是在编造语义。
- **变体重名要去重**：协议里真的存在 PascalCase 之后撞名的取值（`openai/form` 与 `openaiForm`）。撞名时追加序号，真实取值由 `#[serde(rename)]` 保留。
- **裸 bool 是合法 schema**：`{"inputSchema": true}` 表示「任意 JSON」。`normalize()` 把 schema 位置上的裸 bool 换成 `{}`，`additionalProperties` 除外（那里的 bool 有独立含义）。

## 需要人工维护的地方

### 响应类型推断

schema 导出了所有类型，但**没有导出「哪个请求配哪个响应」**。生成器按 `XxxParams → XxxResponse` 的命名约定推断，推断不出来的写在 `main.rs` 的 `RESPONSE_OVERRIDES` 表里（当前 10 条，多为无参请求，例如 `account/logout → LogoutAccountResponse`）。

生成时会打印推断不出来的方法：

```
  注意：N 个方法没有匹配到响应类型，回退为 serde_json::Value：
    some/new/method
```

**看到这个就去补 `RESPONSE_OVERRIDES`**。判断响应类型的办法：在 schema 目录里按方法名找同族的 `*Response.json`，或者直接对真实 daemon 发一次该请求看返回什么。回退成 `serde_json::Value` 不会编译失败，但那个方法就失去了类型保障——属于静默降级，别放过。

### 参数类型撞车

`Request` 实现在参数类型本身上（`impl Request for ThreadStartParams`），因此一个参数类型只能属于一个方法。协议要是让两个方法共用同一个 `*Params`，生成器会**直接停下来**：

```
Error: `a/method` 与 `b/method` 共用参数类型 `SharedParams`，无法各自实现 Request
```

这时得给其中一个套 newtype（`struct BMethodParams(pub SharedParams)` + 转发 `Serialize`），或者把这一族改回 marker 类型。别绕过检查——绕过去生成的就是编译不了的 `conflicting implementations`。

无参方法（schema 里 `params` 是 `null`）没有参数类型可实现，生成器按方法名造一个 unit struct（`account/logout` → `AccountLogoutParams`）；serde 把 unit struct 序列化成 `null`，与协议一致。这个名字撞上 schema 里已有的类型时同样会停下来。

### 生成器自己需要改的信号

生成的代码编译不过时，先看是不是下面这几类（都遇到过）：

| 症状 | 原因 | 改哪里 |
| --- | --- | --- |
| `invalid type: boolean, expected struct Schema` | schema 新位置出现裸 bool | `main.rs::normalize` 的 `SCHEMA_VALUES`/`SCHEMA_MAPS`/`SCHEMA_LISTS` |
| `visibility qualifiers are not permitted here` | enum 变体字段带了 `pub` | `render.rs::field` 的 `public` 参数传错 |
| `the trait bound X: Default is not satisfied` | 给含 enum 字段的 struct derive 了 Default | `render.rs::is_defaultable` |
| `the name X is defined multiple times` | 变体重名没去重 | `render.rs::unique_variant` |
| 生成出 `Variant0(...)`/`Variant1(...)` | 某个 `oneOf` 形态没被识别 | `render.rs::union` 的判定分支 |
| 某类型变成 `serde_json::Value` | `$ref` 指向的名字不在 `known` 里 | 检查 schema 是否自包含 |

## 破坏性协议变更

codex 改了某个类型的形状（而不只是新增）时：

1. `SURFACE.md` 的 diff 会显示参数/响应类型名变化，但**字段变化不会显示**——务必也看 `types.rs` 的 diff。
2. 检查本 crate 的编译错误与 fake 测试；再检查各个下游适配层和真实 daemon 测试。
3. fake 测试数据会开始报 `missing field 'xxx'`。**这是好事**：类型化在强制假数据符合真实形状。照着新的必填字段补，不要图省事把字段改成 `Option`。
4. 真实 daemon 测试是最终裁判。

如果某个变化让适配层没法保持现有行为（例如审批决定的词汇变了），那是一次需要写 ADR 的改动，不要在适配层悄悄兼容过去。

## 几个设计选择的理由

**为什么不用 typify 之类的现成工具**：这份 schema 只用到有限几种构造，自己生成能完全控制命名、文档注释和 serde 属性，也不必赌第三方能否处理它的全部边角（`openai/form` 那种撞名、裸 bool schema、混合 oneOf 都是真实存在的）。生成器本身约 500 行，比调教一个通用工具便宜。

**为什么生成结果要 check-in**：使用本 crate 的项目不该被迫装 codex CLI。构建期跑生成还会让「协议什么时候变的」这件事变得不可追溯——现在它就是一次普通的 git diff。

**为什么生成的 enum 没有 `Unknown` 兜底变体**：试过 `#[serde(other)]`，实测它在 adjacently tagged enum（`tag="method", content="params"`）上遇到带 `params` 的消息会**直接失败**——是个假的安全网。真正的兜底在客户端：解析不了的入站消息作为 `Event::Unparsed(Value)` 原样交出，连接不会失败，但也不假装认识它。

**为什么 schema 本身不 check-in**：它有 260 多个文件、1MB 以上，且完全可以由 CLI 重新导出。要归档某个版本的 schema，归档 `SURFACE.md` 就够了。
