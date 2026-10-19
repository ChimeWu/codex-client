//! Schema → Rust 源码。
//!
//! 这份 schema 是 ts-rs 从 Rust 类型生成的，因此它的形态天然对应 serde 的默认
//! 序列化：内部判别的 `oneOf` 对应 `#[serde(tag = "…")]`，混合字符串/单键对象的
//! `oneOf` 对应 serde 默认的 externally tagged enum。生成器就是把这个映射反过来走。

use crate::schema::{Schema, reference_name};
use std::collections::{BTreeMap, BTreeSet};

/// 待生成的内联类型：schema 里直接写在字段上、没有自己名字的 object。
pub struct Pending {
    pub name: String,
    pub schema: Schema,
}

pub struct Renderer {
    /// 已经有名字的 definitions，用于判断 `$ref` 是否可解析。
    pub known: BTreeSet<String>,
    /// 生成过程中派生出来的内联类型。
    pub pending: Vec<Pending>,
    /// 已经派生过的名字，避免重复生成。
    pub derived: BTreeSet<String>,
}

impl Renderer {
    pub fn new(known: BTreeSet<String>) -> Self {
        Self {
            known,
            pending: Vec::new(),
            derived: BTreeSet::new(),
        }
    }

    /// 一个 schema 对应的 Rust 类型表达式。`hint` 用于给内联类型派生名字。
    pub fn rust_type(&mut self, schema: &Schema, hint: &str) -> String {
        if let Some(reference) = &schema.reference {
            if let Some(name) = reference_name(reference)
                && self.known.contains(name)
            {
                return type_name(name);
            }
            return "serde_json::Value".to_owned();
        }
        if let Some(inner) = schema.nullable_inner() {
            return format!("Option<{}>", self.rust_type(inner, hint));
        }
        if let Some(inner) = schema.single_all_of() {
            return self.rust_type(inner, hint);
        }
        if schema.any_of.is_some() || schema.one_of.is_some() {
            // 有名字的联合类型走 definitions；这里是内联联合，给它派生一个类型。
            return self.derive(hint, schema);
        }
        if schema.is_nullable_scalar() {
            let mut bare = schema.clone();
            bare.ty = schema.non_null_type().map(serde_json::Value::String);
            return format!("Option<{}>", self.rust_type(&bare, hint));
        }
        if let Some(values) = schema.string_enum()
            && values.len() > 1
        {
            return self.derive(hint, schema);
        }
        match schema.non_null_type().as_deref() {
            Some("string") => "String".to_owned(),
            Some("boolean") => "bool".to_owned(),
            Some("integer") => integer_type(schema.format.as_deref()),
            Some("number") => "f64".to_owned(),
            Some("null") => "()".to_owned(),
            Some("array") => {
                let item = schema
                    .items
                    .as_deref()
                    .map(|items| self.rust_type(items, &format!("{hint}Item")))
                    .unwrap_or_else(|| "serde_json::Value".to_owned());
                format!("Vec<{item}>")
            }
            Some("object") => {
                if let Some(values) = schema.additional_properties_schema() {
                    let value = self.rust_type(&values, &format!("{hint}Value"));
                    return format!("std::collections::BTreeMap<String, {value}>");
                }
                if schema.properties.is_some() {
                    return self.derive(hint, schema);
                }
                "serde_json::Value".to_owned()
            }
            _ => "serde_json::Value".to_owned(),
        }
    }

    /// 为一个内联 schema 派生具名类型，并排队等待生成。
    fn derive(&mut self, hint: &str, schema: &Schema) -> String {
        let name = type_name(hint);
        if self.derived.insert(name.clone()) {
            self.known.insert(name.clone());
            self.pending.push(Pending {
                name: name.clone(),
                schema: schema.clone(),
            });
        }
        name
    }

    /// 生成一个具名类型的定义。
    pub fn definition(&mut self, name: &str, schema: &Schema) -> String {
        let name = type_name(name);
        let doc = doc_comment(schema.description.as_deref(), 0);

        if let Some(variants) = &schema.one_of {
            return self.union(&name, variants, &doc);
        }
        if let Some(inner) = schema.nullable_inner() {
            let inner = self.rust_type(inner, &format!("{name}Inner"));
            return format!("{doc}pub type {name} = Option<{inner}>;\n");
        }
        if let Some(inner) = schema.single_all_of() {
            let inner = self.rust_type(inner, &format!("{name}Inner"));
            return format!("{doc}pub type {name} = {inner};\n");
        }
        if schema.any_of.is_some() {
            let variants = schema.any_of.clone().unwrap_or_default();
            return self.untagged_union(&name, &variants, &doc);
        }
        if let Some(values) = schema.string_enum()
            && values.len() > 1
        {
            return string_enum(&name, &values, &doc);
        }
        if schema.is_type("object") || schema.properties.is_some() {
            if let Some(values) = schema.additional_properties_schema() {
                let value = self.rust_type(&values, &format!("{name}Value"));
                return format!(
                    "{doc}pub type {name} = std::collections::BTreeMap<String, {value}>;\n"
                );
            }
            return self.structure(&name, schema, &doc);
        }
        // 标量别名，例如 `AbsolutePathBuf = String`。
        let inner = self.rust_type(schema, &format!("{name}Inner"));
        format!("{doc}pub type {name} = {inner};\n")
    }

    fn structure(&mut self, name: &str, schema: &Schema, doc: &str) -> String {
        let empty = BTreeMap::new();
        let properties = schema.properties.as_ref().unwrap_or(&empty);
        if properties.is_empty() {
            return format!(
                "{doc}#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]\n\
                 pub struct {name} {{}}\n"
            );
        }
        let mut fields = String::new();
        let mut types = Vec::new();
        for (property, property_schema) in properties {
            let rendered = self.field(name, property, property_schema, &schema.required, true);
            if let Some(ty) = rendered.rsplit(": ").next() {
                types.push(ty.trim_end_matches(",\n").to_owned());
            }
            fields.push_str(&rendered);
        }
        // 只有全部字段类型自带 Default 时才 derive：协议里大量字段是 enum，
        // 给 enum 硬选一个默认变体是在编造语义。
        let derives = if types.iter().all(|ty| is_defaultable(ty)) {
            "Clone, Debug, Default, PartialEq, Serialize, Deserialize"
        } else {
            "Clone, Debug, PartialEq, Serialize, Deserialize"
        };
        format!("{doc}#[derive({derives})]\npub struct {name} {{\n{fields}}}\n")
    }

    fn field(
        &mut self,
        owner: &str,
        property: &str,
        schema: &Schema,
        required: &[String],
        public: bool,
    ) -> String {
        let hint = format!("{owner}{}", pascal(property));
        let mut ty = self.rust_type(schema, &hint);
        let is_required = required.iter().any(|name| name == property);
        let already_optional = ty.starts_with("Option<");
        if !is_required && !already_optional {
            ty = format!("Option<{ty}>");
        }
        let rust_name = field_name(property);
        let doc = doc_comment(schema.description.as_deref(), 1);
        let mut attributes = format!("    #[serde(rename = \"{property}\")]\n");
        // 可选字段序列化时省略，避免把一堆 null 发给服务端。
        if ty.starts_with("Option<") {
            attributes
                .push_str("    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n");
        }
        let visibility = if public { "pub " } else { "" };
        format!("{doc}{attributes}    {visibility}{rust_name}: {ty},\n")
    }

    /// `oneOf` 的三种形态。
    fn union(&mut self, name: &str, variants: &[Schema], doc: &str) -> String {
        if let Some(tag) = discriminator(variants) {
            return self.internally_tagged(name, variants, &tag, doc);
        }
        // serde 默认的 externally tagged 形态：字符串取值（可能一个变体里列了
        // 好几个）与单键对象混在一起，例如 `"never" | {"granular": {…}}`。
        if variants.iter().all(|variant| {
            variant.const_string().is_some()
                || variant.string_enum().is_some()
                || single_key(variant).is_some()
        }) {
            return self.externally_tagged(name, variants, doc);
        }
        self.untagged_union(name, variants, doc)
    }

    fn internally_tagged(
        &mut self,
        name: &str,
        variants: &[Schema],
        tag: &str,
        doc: &str,
    ) -> String {
        let mut body = String::new();
        let mut used = BTreeSet::new();
        for variant in variants {
            let empty = BTreeMap::new();
            let properties = variant.properties.as_ref().unwrap_or(&empty);
            let Some(value) = properties.get(tag).and_then(Schema::const_string) else {
                continue;
            };
            let variant_name = unique_variant(&mut used, pascal(&value));
            let variant_doc = doc_comment(variant.description.as_deref(), 1);
            let payload: Vec<_> = properties
                .iter()
                .filter(|(property, _)| property.as_str() != tag)
                .collect();
            if payload.is_empty() {
                body.push_str(&format!(
                    "{variant_doc}    #[serde(rename = \"{value}\")]\n    {variant_name},\n"
                ));
                continue;
            }
            let mut fields = String::new();
            for (property, property_schema) in payload {
                let rendered = self.field(
                    &format!("{name}{variant_name}"),
                    property,
                    property_schema,
                    &variant.required,
                    false,
                );
                // enum 变体里的字段要多缩进一层。
                for line in rendered.lines() {
                    fields.push_str("    ");
                    fields.push_str(line);
                    fields.push('\n');
                }
            }
            body.push_str(&format!(
                "{variant_doc}    #[serde(rename = \"{value}\")]\n    {variant_name} {{\n{fields}    }},\n"
            ));
        }
        format!(
            "{doc}#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n\
             #[serde(tag = \"{tag}\")]\n\
             pub enum {name} {{\n{body}}}\n"
        )
    }

    fn externally_tagged(&mut self, name: &str, variants: &[Schema], doc: &str) -> String {
        let mut body = String::new();
        let mut used = BTreeSet::new();
        for variant in variants {
            let variant_doc = doc_comment(variant.description.as_deref(), 1);
            if let Some(value) = variant.const_string() {
                let variant_name = unique_variant(&mut used, pascal(&value));
                body.push_str(&format!(
                    "{variant_doc}    #[serde(rename = \"{value}\")]\n    {variant_name},\n"
                ));
                continue;
            }
            // 一个变体里列了多个字符串取值：每个取值都是一个独立的 unit 变体。
            if let Some(values) = variant.string_enum() {
                for value in values {
                    let variant_name = unique_variant(&mut used, pascal(&value));
                    body.push_str(&format!(
                        "{variant_doc}    #[serde(rename = \"{value}\")]\n    {variant_name},\n"
                    ));
                }
                continue;
            }
            let Some((key, payload)) = single_key(variant) else {
                continue;
            };
            let variant_name = unique_variant(&mut used, pascal(&key));
            let ty = self.rust_type(&payload, &format!("{name}{variant_name}"));
            body.push_str(&format!(
                "{variant_doc}    #[serde(rename = \"{key}\")]\n    {variant_name}({ty}),\n"
            ));
        }
        format!(
            "{doc}#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n\
             pub enum {name} {{\n{body}}}\n"
        )
    }

    fn untagged_union(&mut self, name: &str, variants: &[Schema], doc: &str) -> String {
        let mut body = String::new();
        let mut used = BTreeSet::new();
        for (index, variant) in variants.iter().enumerate() {
            if variant.is_type("null") {
                continue;
            }
            let hint = format!("{name}Variant{index}");
            let ty = self.rust_type(variant, &hint);
            let variant_name = unique_variant(&mut used, untagged_name(&ty, index));
            body.push_str(&format!("    {variant_name}({ty}),\n"));
        }
        format!(
            "{doc}#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n\
             #[serde(untagged)]\n\
             pub enum {name} {{\n{body}}}\n"
        )
    }
}

/// 全部变体共有、且取值都是单值字符串枚举的那个字段名。
fn discriminator(variants: &[Schema]) -> Option<String> {
    let first = variants.first()?;
    let properties = first.properties.as_ref()?;
    for candidate in properties.keys() {
        let all_const = variants.iter().all(|variant| {
            variant
                .properties
                .as_ref()
                .and_then(|properties| properties.get(candidate))
                .and_then(Schema::const_string)
                .is_some()
        });
        if all_const {
            return Some(candidate.clone());
        }
    }
    None
}

/// 只有一个必填属性的 object，返回 (键, 值 schema)。
fn single_key(schema: &Schema) -> Option<(String, Schema)> {
    let properties = schema.properties.as_ref()?;
    if properties.len() != 1 {
        return None;
    }
    let (key, value) = properties.iter().next()?;
    Some((key.clone(), value.clone()))
}

fn string_enum(name: &str, values: &[String], doc: &str) -> String {
    let mut body = String::new();
    let mut used = BTreeSet::new();
    for value in values {
        let variant = unique_variant(&mut used, pascal(value));
        body.push_str(&format!(
            "    /// 判别值 `{value}`\n    #[serde(rename = \"{value}\")]\n    {variant},\n"
        ));
    }
    format!(
        "{doc}#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]\n\
         pub enum {name} {{\n{body}}}\n"
    )
}

fn integer_type(format: Option<&str>) -> String {
    match format {
        Some("int8") => "i8",
        Some("int16") => "i16",
        Some("int32") => "i32",
        Some("uint8") => "u8",
        Some("uint16") => "u16",
        Some("uint32") => "u32",
        Some("uint64") | Some("uint") => "u64",
        _ => "i64",
    }
    .to_owned()
}

pub fn doc_comment(description: Option<&str>, indent: usize) -> String {
    let Some(description) = description else {
        return String::new();
    };
    let pad = "    ".repeat(indent);
    description
        .lines()
        .map(|line| {
            let line = line.trim().replace('\r', "");
            // 空行写成 `///` 而不是 `/// `：尾随空格会让 rustfmt 每次都产生 diff。
            if line.is_empty() {
                format!("{pad}///\n")
            } else {
                format!("{pad}/// {line}\n")
            }
        })
        .collect()
}

/// 类型名：去掉 schema 里出现的 `/` 等字符，转成 PascalCase。
pub fn type_name(raw: &str) -> String {
    let cleaned = pascal(raw);
    if cleaned.is_empty() {
        "Unnamed".to_owned()
    } else {
        cleaned
    }
}

/// 把任意标识符转成 PascalCase。
pub fn pascal(raw: &str) -> String {
    let mut out = String::new();
    let mut upper_next = true;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper_next {
                out.extend(ch.to_uppercase());
                upper_next = false;
            } else {
                out.push(ch);
            }
        } else {
            upper_next = true;
        }
    }
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    out
}

/// 把 camelCase / snake_case 属性名转成合法的 Rust 字段名。
pub fn field_name(raw: &str) -> String {
    let mut out = String::new();
    let mut previous_lower = false;
    for ch in raw.chars() {
        if ch.is_ascii_uppercase() {
            if previous_lower {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
            previous_lower = false;
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
            previous_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            out.push('_');
            previous_lower = false;
        }
    }
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    escape_keyword(&out)
}

fn escape_keyword(name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
        "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final",
        "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ];
    if KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}

/// 这个 Rust 类型表达式是否自带 `Default`。
///
/// 只认标准库里确定有 Default 的形状；具名类型（可能是 enum）一律当作没有，
/// 宁可少 derive 一个 Default，也不要生成编译不过的代码。
fn is_defaultable(ty: &str) -> bool {
    const SCALARS: &[&str] = &[
        "String",
        "bool",
        "f32",
        "f64",
        "i8",
        "i16",
        "i32",
        "i64",
        "u8",
        "u16",
        "u32",
        "u64",
        "()",
        "serde_json::Value",
    ];
    let ty = ty.trim();
    SCALARS.contains(&ty)
        || ty.starts_with("Option<")
        || ty.starts_with("Vec<")
        || ty.starts_with("std::collections::BTreeMap<")
}

/// 变体名去重。
///
/// 协议里确实存在 PascalCase 之后会撞名的判别值（例如 `openai/form` 与
/// `openaiForm` 是两个不同的取值）。撞名时追加序号——真实取值由 `#[serde(rename)]`
/// 保留，不会因为改名而错。
fn unique_variant(used: &mut BTreeSet<String>, base: String) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 2..100 {
        let candidate = format!("{base}{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    base
}

/// untagged 变体的名字。
///
/// 按承载的类型取名而不是 `Variant0`/`Variant1`：协议里「一个或一组」这种
/// `anyOf: [T, [T]]` 很常见（例如 `thread/list` 的 cwd 过滤），生成成
/// `Variant0(String)` 调用方根本没法读，只能绕回 JSON 去构造。
fn untagged_name(ty: &str, index: usize) -> String {
    let ty = ty.trim();
    if let Some(item) = ty
        .strip_prefix("Vec<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        // Vec<String> → List；Vec<Foo> → FooList
        return match item {
            "String" => "List".to_owned(),
            other if other.chars().next().is_some_and(char::is_uppercase) => {
                format!("{}List", pascal(other))
            }
            _ => "List".to_owned(),
        };
    }
    match ty {
        "String" => "Text".to_owned(),
        "bool" => "Boolean".to_owned(),
        "f32" | "f64" => "Number".to_owned(),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => "Integer".to_owned(),
        "serde_json::Value" => "Raw".to_owned(),
        "()" => "Null".to_owned(),
        other if other.chars().next().is_some_and(char::is_uppercase) => pascal(other),
        _ => format!("Variant{index}"),
    }
}
