//! JSON Schema 的最小模型。
//!
//! 只覆盖 `codex app-server generate-json-schema` 实际产出的构造——它是 ts-rs
//! 从 Rust 类型生成的，形态有限且规整，因此不需要一个完整的 JSON Schema 实现。

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    /// 可以是 `"string"`，也可以是 `["string","null"]`。
    #[serde(rename = "type")]
    pub ty: Option<Value>,
    pub properties: Option<BTreeMap<String, Schema>>,
    #[serde(default)]
    pub required: Vec<String>,
    pub items: Option<Box<Schema>>,
    #[serde(rename = "enum")]
    pub enumeration: Option<Vec<Value>>,
    pub one_of: Option<Vec<Schema>>,
    pub any_of: Option<Vec<Schema>>,
    pub all_of: Option<Vec<Schema>>,
    /// `false`、`true` 或一个子 schema。
    pub additional_properties: Option<Value>,
    pub format: Option<String>,
    pub description: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub definitions: BTreeMap<String, Schema>,
}

impl Schema {
    /// `type` 字段里出现的全部类型名。
    pub fn types(&self) -> Vec<String> {
        match &self.ty {
            Some(Value::String(one)) => vec![one.clone()],
            Some(Value::Array(many)) => many
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn is_type(&self, name: &str) -> bool {
        self.types().iter().any(|ty| ty == name)
    }

    /// `type: ["string","null"]` 这种可空写法。
    pub fn is_nullable_scalar(&self) -> bool {
        let types = self.types();
        types.len() > 1 && types.iter().any(|ty| ty == "null")
    }

    /// 去掉 null 之后剩下的唯一类型。
    pub fn non_null_type(&self) -> Option<String> {
        self.types().into_iter().find(|ty| ty != "null")
    }

    /// `anyOf: [T, null]` 这种可空写法，返回 T。
    pub fn nullable_inner(&self) -> Option<&Schema> {
        let variants = self.any_of.as_ref()?;
        if variants.len() != 2 {
            return None;
        }
        let has_null = variants.iter().any(|variant| variant.is_type("null"));
        if !has_null {
            return None;
        }
        variants.iter().find(|variant| !variant.is_type("null"))
    }

    /// `allOf: [T]`——ts-rs 用它给一个 `$ref` 挂上 description/default。
    pub fn single_all_of(&self) -> Option<&Schema> {
        match self.all_of.as_deref() {
            Some([only]) => Some(only),
            _ => None,
        }
    }

    /// 单值字符串枚举（判别式），返回那个值。
    pub fn const_string(&self) -> Option<String> {
        match self.enumeration.as_deref() {
            Some([Value::String(only)]) => Some(only.clone()),
            _ => None,
        }
    }

    /// 全部取值都是字符串的枚举。
    pub fn string_enum(&self) -> Option<Vec<String>> {
        let values = self.enumeration.as_ref()?;
        if values.len() < 2 && self.const_string().is_some() {
            return None;
        }
        values
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect()
    }

    pub fn additional_properties_schema(&self) -> Option<Schema> {
        match &self.additional_properties {
            Some(Value::Object(_)) => {
                serde_json::from_value(self.additional_properties.clone().expect("checked above"))
                    .ok()
            }
            _ => None,
        }
    }
}

/// 把 `#/definitions/Foo` 解析成 `Foo`。
pub fn reference_name(reference: &str) -> Option<&str> {
    reference.strip_prefix("#/definitions/")
}
