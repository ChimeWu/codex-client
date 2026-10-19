//! Minimal JSON Schema model.
//!
//! Covers only the shapes emitted by `codex app-server generate-json-schema`. They are
//! generated from Rust types by ts-rs and are regular enough that a full JSON Schema
//! implementation is unnecessary.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    /// Either `"string"` or `["string", "null"]`.
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
    /// `false`, `true`, or a nested schema.
    pub additional_properties: Option<Value>,
    pub format: Option<String>,
    pub description: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub definitions: BTreeMap<String, Schema>,
}

impl Schema {
    /// All type names listed in the `type` field.
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

    /// Nullable form such as `type: ["string", "null"]`.
    pub fn is_nullable_scalar(&self) -> bool {
        let types = self.types();
        types.len() > 1 && types.iter().any(|ty| ty == "null")
    }

    /// The sole type remaining after removing `null`.
    pub fn non_null_type(&self) -> Option<String> {
        self.types().into_iter().find(|ty| ty != "null")
    }

    /// Return `T` from a nullable form such as `anyOf: [T, null]`.
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

    /// `allOf: [T]`; ts-rs uses this to attach a description or default to a `$ref`.
    pub fn single_all_of(&self) -> Option<&Schema> {
        match self.all_of.as_deref() {
            Some([only]) => Some(only),
            _ => None,
        }
    }

    /// Return the value of a single-value string enum (a discriminator).
    pub fn const_string(&self) -> Option<String> {
        match self.enumeration.as_deref() {
            Some([Value::String(only)]) => Some(only.clone()),
            _ => None,
        }
    }

    /// An enum whose values are all strings.
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

/// Resolve `#/definitions/Foo` to `Foo`.
pub fn reference_name(reference: &str) -> Option<&str> {
    reference.strip_prefix("#/definitions/")
}
