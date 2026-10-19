//! Schema → Rust source code.
//!
//! This schema is generated from Rust types by ts-rs, so its shapes naturally map to
//! Serde's default serialization: internally tagged `oneOf` maps to `#[serde(tag = "…")]`,
//! while `oneOf` mixing strings and single-key objects maps to Serde's externally tagged
//! enums. The generator applies this mapping in reverse.

use crate::schema::{Schema, reference_name};
use std::collections::{BTreeMap, BTreeSet};

/// An inline type to generate: an object written directly on a schema field without its
/// own name.
pub struct Pending {
    pub name: String,
    pub schema: Schema,
}

pub struct Renderer {
    /// Named definitions used to determine whether a `$ref` can be resolved.
    pub known: BTreeSet<String>,
    /// Inline types derived during generation.
    pub pending: Vec<Pending>,
    /// Names already derived, to avoid generating them more than once.
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

    /// The Rust type expression corresponding to a schema. `hint` helps derive a name
    /// for inline types.
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
            // Named unions are handled through definitions; derive a type for this inline
            // union.
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

    /// Derive a named type for an inline schema and queue it for generation.
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

    /// Generate a named type definition.
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
        // Scalar alias, such as `AbsolutePathBuf = String`.
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
        // Derive Default only when every field type has one. Many protocol fields are
        // enums, and choosing an arbitrary default variant would invent semantics.
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
        // Omit optional fields during serialization instead of sending many null values.
        if ty.starts_with("Option<") {
            attributes
                .push_str("    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n");
        }
        let visibility = if public { "pub " } else { "" };
        format!("{doc}{attributes}    {visibility}{rust_name}: {ty},\n")
    }

    /// The three supported `oneOf` shapes.
    fn union(&mut self, name: &str, variants: &[Schema], doc: &str) -> String {
        if let Some(tag) = discriminator(variants) {
            return self.internally_tagged(name, variants, &tag, doc);
        }
        // Serde's default externally tagged shape: string values (possibly several in a
        // variant) mixed with single-key objects, such as `"never" | {"granular": {…}}`.
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
                // Fields inside enum variants need one additional indentation level.
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
            // A variant with multiple string values becomes one unit variant per value.
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

/// The field shared by every variant whose value is a single-value string enum.
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

/// Return the key and value schema for an object with exactly one required property.
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
            "    /// Serializes as `{value}`.\n    #[serde(rename = \"{value}\")]\n    {variant},\n"
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
            let line = rustdoc_safe_description(&line.trim().replace('\r', ""));
            // Emit `///` rather than `/// ` for blank lines; trailing whitespace would
            // make rustfmt produce a diff every time.
            if line.is_empty() {
                format!("{pad}///\n")
            } else {
                format!("{pad}/// {line}\n")
            }
        })
        .collect()
}

fn rustdoc_safe_description(description: &str) -> String {
    let mut plain = String::with_capacity(description.len());
    let mut rest = description;
    while let Some(open) = rest.find('[') {
        plain.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find(']') else {
            plain.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let label = &after_open[..close];
        let after_close = &after_open[close + 1..];
        if after_close.starts_with('(') {
            plain.push_str(&rest[open..open + close + 2]);
        } else {
            plain.push('`');
            plain.push_str(label);
            plain.push('`');
        }
        rest = after_close;
    }
    plain.push_str(rest);

    let mut linked = String::with_capacity(plain.len());
    let mut rest = plain.as_str();
    while let Some(start) = rest.find("https://") {
        linked.push_str(&rest[..start]);
        let end = rest[start..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |offset| start + offset);
        let mut url_end = end;
        while url_end > start && matches!(rest.as_bytes()[url_end - 1], b'.' | b',' | b';' | b':') {
            url_end -= 1;
        }
        if rest[..start].ends_with('<') {
            linked.push_str(&rest[start..end]);
        } else {
            linked.push('<');
            linked.push_str(&rest[start..url_end]);
            linked.push('>');
            linked.push_str(&rest[url_end..end]);
        }
        rest = &rest[end..];
    }
    linked.push_str(rest);
    linked
}

/// Convert a schema type name to PascalCase, removing characters such as `/`.
pub fn type_name(raw: &str) -> String {
    let cleaned = pascal(raw);
    if cleaned.is_empty() {
        "Unnamed".to_owned()
    } else {
        cleaned
    }
}

/// Convert an arbitrary identifier to PascalCase.
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

/// Convert a camelCase or snake_case property name to a valid Rust field name.
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

/// Whether this Rust type expression has a known `Default` implementation.
///
/// Recognize only shapes that are guaranteed to implement `Default` in the standard
/// library. Treat named types (which may be enums) as non-defaultable; omitting a derive
/// is safer than generating code that does not compile.
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

/// Deduplicate variant names.
///
/// Protocol tags can collide after PascalCase conversion (for example, `openai/form` and
/// `openaiForm` are distinct values). Append a numeric suffix on collision; the original
/// value remains in `#[serde(rename)]`.
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

/// Name an untagged enum variant.
///
/// Name variants after their contained types instead of `Variant0`/`Variant1`. The
/// protocol often uses `anyOf: [T, [T]]` for "one or many" values (for example, the cwd
/// filter in `thread/list`); `Variant0(String)` would be difficult for callers to use
/// without constructing raw JSON.
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
