// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Wire-safe payload value type for hook events.
//!
//! A postcard-compatible alternative to serde_json::Value.
//! Converts to/from JSON at adapter boundaries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A single value in a hook event payload.
///
/// Postcard-serializable, round-trips cleanly to/from serde_json::Value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PayloadValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(SmolStr),
    Bytes(Vec<u8>),
    List(Vec<PayloadValue>),
    Map(BTreeMap<SmolStr, PayloadValue>),
}

/// A hook event payload: a map of named values.
///
/// Newtype wrapper (not a type alias) so we can implement From traits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HookPayload(pub BTreeMap<SmolStr, PayloadValue>);

impl HookPayload {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn insert(&mut self, key: impl Into<SmolStr>, value: PayloadValue) {
        self.0.insert(key.into(), value);
    }

    pub fn get(&self, key: &str) -> Option<&PayloadValue> {
        self.0.get(key)
    }
}

impl std::ops::Deref for HookPayload {
    type Target = BTreeMap<SmolStr, PayloadValue>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ---- Conversions to/from serde_json::Value ----------------------------------

impl From<serde_json::Value> for PayloadValue {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Self::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Self::Float(f)
                } else {
                    Self::Null
                }
            }
            serde_json::Value::String(s) => Self::String(SmolStr::from(s)),
            serde_json::Value::Array(arr) => {
                Self::List(arr.into_iter().map(PayloadValue::from).collect())
            }
            serde_json::Value::Object(obj) => {
                Self::Map(
                    obj.into_iter()
                        .map(|(k, v)| (SmolStr::from(k), PayloadValue::from(v)))
                        .collect(),
                )
            }
        }
    }
}

impl From<PayloadValue> for serde_json::Value {
    fn from(v: PayloadValue) -> Self {
        match v {
            PayloadValue::Null => Self::Null,
            PayloadValue::Bool(b) => Self::Bool(b),
            PayloadValue::Int(i) => Self::Number(i.into()),
            PayloadValue::Float(f) => {
                serde_json::Number::from_f64(f)
                    .map(Self::Number)
                    .unwrap_or(Self::Null)
            }
            PayloadValue::String(s) => Self::String(s.to_string()),
            PayloadValue::Bytes(b) => {
                // Encode bytes as base64 string in JSON representation.
                use base64::Engine;
                Self::String(base64::engine::general_purpose::STANDARD.encode(&b))
            }
            PayloadValue::List(arr) => {
                Self::Array(arr.into_iter().map(serde_json::Value::from).collect())
            }
            PayloadValue::Map(obj) => {
                Self::Object(
                    obj.into_iter()
                        .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
                        .collect(),
                )
            }
        }
    }
}

/// Build a payload from a serde_json::Value (typically json! macro).
impl From<serde_json::Value> for HookPayload {
    fn from(v: serde_json::Value) -> Self {
        match PayloadValue::from(v) {
            PayloadValue::Map(m) => Self(m),
            other => {
                let mut map = BTreeMap::new();
                map.insert(SmolStr::from("value"), other);
                Self(map)
            }
        }
    }
}

// Convenience constructors
impl PayloadValue {
    pub fn string(s: impl Into<SmolStr>) -> Self {
        Self::String(s.into())
    }

    pub fn int(i: i64) -> Self {
        Self::Int(i)
    }
}
