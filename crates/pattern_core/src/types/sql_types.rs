// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `FromSql`/`ToSql` implementations for domain enum types stored as TEXT
//! columns in SQLite.
//!
//! These impls live in `pattern_core` (behind the `sqlite` feature) so the
//! orphan rule is satisfied: the types are local to this crate while
//! `rusqlite` traits are foreign.  `pattern_db` enables the `sqlite` feature
//! and gets these impls for free.

#![cfg(feature = "sqlite")]

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

/// Implement `ToSql` and `FromSql` for an enum that has `as_str()` -> db format
/// and `FromStr` that parses the db format.
macro_rules! impl_text_sql_via_as_str {
    ($ty:ty) => {
        impl ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $ty {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let s = value.as_str()?;
                s.parse::<Self>().map_err(|e| {
                    FromSqlError::Other(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e.to_string(),
                    )))
                })
            }
        }
    };
}

// MemoryBlockType: as_str() returns "core"/"working".
impl_text_sql_via_as_str!(crate::types::memory_types::MemoryBlockType);

// MemoryPermission: as_str() returns "read_only"/"partner"/etc.
impl_text_sql_via_as_str!(crate::types::memory_types::MemoryPermission);

// TaskStatus: as_str() returns kebab-case "pending"/"in-progress"/etc.
impl_text_sql_via_as_str!(crate::types::memory_types::TaskStatus);
