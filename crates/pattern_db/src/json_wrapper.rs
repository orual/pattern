// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Transparent JSON wrapper type for database columns stored as JSON TEXT.
//!
//! Replaces the former `sqlx::types::Json<T>` usage. Provides identical
//! public surface: `Deref<Target = T>`, `From<T>`, transparent serde,
//! and rusqlite `FromSql`/`ToSql` for round-tripping through SQLite TEXT
//! columns.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

/// A transparent wrapper that stores `T` as JSON TEXT in SQLite.
///
/// Semantically identical to the former `sqlx::types::Json<T>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T> Json<T> {
    /// Consume the wrapper and return the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Json<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> std::ops::Deref for Json<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> std::ops::DerefMut for Json<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

// Transparent serde: delegates directly to T.
impl<T: Serialize> Serialize for Json<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Json<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Json)
    }
}

// rusqlite integration: stored as JSON TEXT.
impl<T: Serialize> ToSql for Json<T> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        serde_json::to_string(&self.0)
            .map(ToSqlOutput::from)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    }
}

impl<T: for<'de> Deserialize<'de>> FromSql for Json<T> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        serde_json::from_str(s)
            .map(Json)
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_json_value() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (data TEXT)", []).unwrap();

        let val = Json(serde_json::json!({"key": "value", "n": 42}));
        conn.execute("INSERT INTO t (data) VALUES (?1)", [&val])
            .unwrap();

        let result: Json<serde_json::Value> = conn
            .query_row("SELECT data FROM t", [], |r| r.get(0))
            .unwrap();

        assert_eq!(result.0["key"], "value");
        assert_eq!(result.0["n"], 42);
    }

    #[test]
    fn round_trip_typed_json() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (data TEXT)", []).unwrap();

        let val = Json(vec!["alpha".to_string(), "beta".to_string()]);
        conn.execute("INSERT INTO t (data) VALUES (?1)", [&val])
            .unwrap();

        let result: Json<Vec<String>> = conn
            .query_row("SELECT data FROM t", [], |r| r.get(0))
            .unwrap();

        assert_eq!(result.0, vec!["alpha", "beta"]);
    }

    #[test]
    fn deref_works() {
        let j = Json(vec![1, 2, 3]);
        assert_eq!(j.len(), 3);
    }

    #[test]
    fn serde_transparent() {
        let j = Json(42u64);
        let s = serde_json::to_string(&j).unwrap();
        assert_eq!(s, "42");

        let j2: Json<u64> = serde_json::from_str(&s).unwrap();
        assert_eq!(j2.0, 42);
    }
}
