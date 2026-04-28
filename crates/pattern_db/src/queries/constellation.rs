//! pattern_db implementation of `pattern_core::ConstellationRegistry`.
//!
//! Reads/writes against the `agents`, `persona_relationships`, `persona_groups`,
//! and `persona_group_members` tables (migrations 0014, 0015, 0017).
//!
//! `agents.persona_status` (added in migration 0017) holds the lifecycle status
//! independent of the pre-v3 `agents.status` column (which still tracks runtime
//! state — Active/Hibernated/Archived).
//!
//! Project filtering uses SQLite's JSON1 `json_each` to scan
//! `agents.project_attachments` (a JSON array of paths).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};

use pattern_core::PersonaId;
use pattern_core::constellation::{
    ConstellationRegistry, EdgeDirection, PersonaGroup, PersonaRecord, PersonaStatus,
    RegistryError, RegistryScope, RelationshipEdge, RelationshipSpec,
};
use pattern_core::spawn::RelationshipKind;
use pattern_core::types::ids::{GroupId, new_id};

use crate::ConstellationDb;
use crate::error::DbError;

// ── DB encoding helpers ──────────────────────────────────────────────────────

fn persona_status_to_str(s: PersonaStatus) -> &'static str {
    match s {
        PersonaStatus::Active => "active",
        PersonaStatus::Draft => "draft",
        PersonaStatus::Inactive => "inactive",
    }
}

fn persona_status_from_str(s: &str) -> Result<PersonaStatus, RegistryError> {
    match s {
        "active" => Ok(PersonaStatus::Active),
        "draft" => Ok(PersonaStatus::Draft),
        "inactive" => Ok(PersonaStatus::Inactive),
        _ => Err(RegistryError::BackendUnavailable),
    }
}

fn relationship_kind_to_str(k: RelationshipKind) -> &'static str {
    match k {
        RelationshipKind::SupervisorOf => "supervisor_of",
        RelationshipKind::SpecialistFor => "specialist_for",
        RelationshipKind::PeerWith => "peer_with",
        RelationshipKind::ObserverOf => "observer_of",
    }
}

fn relationship_kind_from_str(s: &str) -> Option<RelationshipKind> {
    match s {
        "supervisor_of" => Some(RelationshipKind::SupervisorOf),
        "specialist_for" => Some(RelationshipKind::SpecialistFor),
        "peer_with" => Some(RelationshipKind::PeerWith),
        "observer_of" => Some(RelationshipKind::ObserverOf),
        _ => None,
    }
}

fn map_db_err(e: DbError) -> RegistryError {
    tracing::warn!(target: "pattern_db::constellation", error = %e, "registry backend error");
    RegistryError::BackendUnavailable
}

fn map_sqlite_err(e: rusqlite::Error) -> RegistryError {
    tracing::warn!(target: "pattern_db::constellation", error = %e, "registry sqlite error");
    RegistryError::BackendUnavailable
}

// ── PersonaRowSlim ───────────────────────────────────────────────────────────

/// Minimal projection of `agents` columns the registry needs.
struct PersonaRowSlim {
    id: String,
    name: String,
    status: PersonaStatus,
    config_path: Option<String>,
    project_attachments_json: String,
}

impl PersonaRowSlim {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let status_str: String = row.get("persona_status")?;
        let status = persona_status_from_str(&status_str).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown persona_status '{status_str}'"),
                )),
            )
        })?;
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            status,
            config_path: row.get("config_path")?,
            project_attachments_json: row.get("project_attachments")?,
        })
    }
}

const PERSONA_SELECT: &str =
    "SELECT id, name, persona_status, config_path, project_attachments FROM agents";

// ── ConstellationRegistryDb ──────────────────────────────────────────────────

/// rusqlite-backed `ConstellationRegistry`.
#[derive(Debug, Clone)]
pub struct ConstellationRegistryDb {
    db: Arc<ConstellationDb>,
}

impl ConstellationRegistryDb {
    pub fn new(db: Arc<ConstellationDb>) -> Self {
        Self { db }
    }

    /// Hydrate a slim row into a full `PersonaRecord` with relationships and
    /// group memberships loaded.
    fn hydrate_record(
        conn: &Connection,
        slim: PersonaRowSlim,
    ) -> Result<PersonaRecord, RegistryError> {
        let project_attachments: Vec<std::path::PathBuf> =
            serde_json::from_str(&slim.project_attachments_json).map_err(|e| {
                tracing::warn!(target: "pattern_db::constellation", error = %e, "bad project_attachments JSON");
                RegistryError::BackendUnavailable
            })?;

        let relationships = load_relationships_for(conn, &slim.id)?;
        let group_memberships = load_group_memberships_for(conn, &slim.id)?;

        Ok(PersonaRecord::from_parts(
            PersonaId::new(slim.id.as_str()),
            slim.name,
            slim.status,
            slim.config_path.map(Into::into),
            project_attachments,
            relationships,
            group_memberships,
        ))
    }
}

// ── Relationship + group helpers ─────────────────────────────────────────────

fn load_relationships_for(
    conn: &Connection,
    persona_id: &str,
) -> Result<Vec<RelationshipEdge>, RegistryError> {
    let mut stmt = conn
        .prepare(
            "SELECT from_persona, to_persona, kind FROM persona_relationships
             WHERE from_persona = ?1 OR to_persona = ?1",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![persona_id], |row| {
            let from: String = row.get(0)?;
            let to: String = row.get(1)?;
            let kind: String = row.get(2)?;
            Ok((from, to, kind))
        })
        .map_err(map_sqlite_err)?;

    let mut edges = Vec::new();
    for row in rows {
        let (from, to, kind_str) = row.map_err(map_sqlite_err)?;
        let Some(kind) = relationship_kind_from_str(&kind_str) else {
            tracing::warn!(target: "pattern_db::constellation", kind = %kind_str, "unknown relationship kind in DB");
            continue;
        };
        let (other, direction) = if from == persona_id {
            (to, EdgeDirection::Outgoing)
        } else {
            (from, EdgeDirection::Incoming)
        };
        edges.push(RelationshipEdge {
            other: PersonaId::new(other.as_str()),
            kind,
            direction,
        });
    }
    Ok(edges)
}

fn load_group_memberships_for(
    conn: &Connection,
    persona_id: &str,
) -> Result<Vec<GroupId>, RegistryError> {
    let mut stmt = conn
        .prepare("SELECT group_id FROM persona_group_members WHERE persona_id = ?1")
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![persona_id], |row| {
            let id: String = row.get(0)?;
            Ok(id)
        })
        .map_err(map_sqlite_err)?;

    let mut out = Vec::new();
    for row in rows {
        out.push(GroupId::new(row.map_err(map_sqlite_err)?.as_str()));
    }
    Ok(out)
}

fn load_group_members(conn: &Connection, group_id: &str) -> Result<Vec<PersonaId>, RegistryError> {
    let mut stmt = conn
        .prepare("SELECT persona_id FROM persona_group_members WHERE group_id = ?1")
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![group_id], |row| {
            let id: String = row.get(0)?;
            Ok(id)
        })
        .map_err(map_sqlite_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(PersonaId::new(row.map_err(map_sqlite_err)?.as_str()));
    }
    Ok(out)
}

// ── ConstellationRegistry impl ───────────────────────────────────────────────

#[async_trait]
impl ConstellationRegistry for ConstellationRegistryDb {
    async fn list(&self, scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            list_blocking(&conn, &scope)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "list join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn get(&self, id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError> {
        let db = self.db.clone();
        let id = id.as_str().to_string();
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            get_blocking(&conn, &id)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "get join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn find(
        &self,
        project: Option<&Path>,
        kind: Option<RelationshipKind>,
    ) -> Result<Vec<PersonaRecord>, RegistryError> {
        let db = self.db.clone();
        let project = project.map(|p| p.to_string_lossy().into_owned());
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            find_blocking(&conn, project.as_deref(), kind)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "find join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn register(&self, record: PersonaRecord) -> Result<(), RegistryError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.get().map_err(map_db_err)?;
            register_blocking(&mut conn, record)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "register join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn set_status(&self, id: &PersonaId, status: PersonaStatus) -> Result<(), RegistryError> {
        let db = self.db.clone();
        let id_str = id.as_str().to_string();
        let id_owned = id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            let updated = conn
                .execute(
                    "UPDATE agents SET persona_status = ?1, updated_at = datetime('now')
                     WHERE id = ?2",
                    params![persona_status_to_str(status), id_str],
                )
                .map_err(map_sqlite_err)?;
            if updated == 0 {
                Err(RegistryError::PersonaNotFound(id_owned))
            } else {
                Ok(())
            }
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "set_status join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn set_config_path(
        &self,
        id: &PersonaId,
        config_path: Option<std::path::PathBuf>,
    ) -> Result<(), RegistryError> {
        let db = self.db.clone();
        let id_str = id.as_str().to_string();
        let id_owned = id.clone();
        let path_str = config_path.map(|p| p.to_string_lossy().into_owned());
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            let updated = conn
                .execute(
                    "UPDATE agents SET config_path = ?1, updated_at = datetime('now')
                     WHERE id = ?2",
                    params![path_str, id_str],
                )
                .map_err(map_sqlite_err)?;
            if updated == 0 {
                Err(RegistryError::PersonaNotFound(id_owned))
            } else {
                Ok(())
            }
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "set_config_path join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn add_relationship(&self, edge: RelationshipSpec) -> Result<bool, RegistryError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.get().map_err(map_db_err)?;
            add_relationship_blocking(&mut conn, edge)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "add_relationship join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn groups(&self, scope: RegistryScope) -> Result<Vec<PersonaGroup>, RegistryError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            groups_blocking(&conn, &scope)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "groups join error");
            RegistryError::BackendUnavailable
        })?
    }

    async fn create_group(
        &self,
        name: String,
        project_id: Option<String>,
    ) -> Result<PersonaGroup, RegistryError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.get().map_err(map_db_err)?;
            create_group_blocking(&conn, name, project_id)
        })
        .await
        .map_err(|e| {
            tracing::warn!(target: "pattern_db::constellation", error = %e, "create_group join error");
            RegistryError::BackendUnavailable
        })?
    }
}

// ── Blocking helpers (run inside spawn_blocking) ─────────────────────────────

fn list_blocking(
    conn: &Connection,
    scope: &RegistryScope,
) -> Result<Vec<PersonaRecord>, RegistryError> {
    let slims = match scope {
        RegistryScope::All => {
            let mut stmt = conn
                .prepare(&format!("{PERSONA_SELECT} ORDER BY name"))
                .map_err(map_sqlite_err)?;
            let rows = stmt
                .query_map([], PersonaRowSlim::from_row)
                .map_err(map_sqlite_err)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite_err)?
        }
        RegistryScope::Project(p) => {
            let p_str = p.to_string_lossy().into_owned();
            let mut stmt = conn
                .prepare(&format!(
                    "{PERSONA_SELECT} a
                     WHERE EXISTS (
                        SELECT 1 FROM json_each(a.project_attachments) j
                        WHERE j.value = ?1
                     )
                     ORDER BY name"
                ))
                .map_err(map_sqlite_err)?;
            let rows = stmt
                .query_map(params![p_str], PersonaRowSlim::from_row)
                .map_err(map_sqlite_err)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite_err)?
        }
    };

    slims
        .into_iter()
        .map(|s| ConstellationRegistryDb::hydrate_record(conn, s))
        .collect()
}

fn get_blocking(conn: &Connection, id: &str) -> Result<Option<PersonaRecord>, RegistryError> {
    let slim = conn
        .query_row(
            &format!("{PERSONA_SELECT} WHERE id = ?1"),
            params![id],
            PersonaRowSlim::from_row,
        )
        .optional()
        .map_err(map_sqlite_err)?;
    match slim {
        Some(s) => ConstellationRegistryDb::hydrate_record(conn, s).map(Some),
        None => Ok(None),
    }
}

fn find_blocking(
    conn: &Connection,
    project: Option<&str>,
    kind: Option<RelationshipKind>,
) -> Result<Vec<PersonaRecord>, RegistryError> {
    // Build SQL dynamically. AND together project + kind filters.
    let mut sql = String::from(
        "SELECT DISTINCT a.id, a.name, a.persona_status, a.config_path, a.project_attachments
                 FROM agents a",
    );
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bound: Vec<String> = Vec::new();

    if let Some(k) = kind {
        sql.push_str(" JOIN persona_relationships r ON r.from_persona = a.id");
        where_clauses.push("r.kind = ?".to_string());
        bound.push(relationship_kind_to_str(k).to_string());
    }
    if let Some(p) = project {
        where_clauses.push(
            "EXISTS (SELECT 1 FROM json_each(a.project_attachments) j WHERE j.value = ?)"
                .to_string(),
        );
        bound.push(p.to_string());
    }
    if !where_clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY a.name");

    let mut stmt = conn.prepare(&sql).map_err(map_sqlite_err)?;
    let bound_refs: Vec<&dyn rusqlite::ToSql> =
        bound.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(bound_refs),
            PersonaRowSlim::from_row,
        )
        .map_err(map_sqlite_err)?;
    let slims: Vec<_> = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_err)?;

    slims
        .into_iter()
        .map(|s| ConstellationRegistryDb::hydrate_record(conn, s))
        .collect()
}

fn register_blocking(conn: &mut Connection, record: PersonaRecord) -> Result<(), RegistryError> {
    // Check duplicate first for a clean error.
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM agents WHERE id = ?1",
            params![record.id.as_str()],
            |_| Ok(true),
        )
        .optional()
        .map_err(map_sqlite_err)?
        .unwrap_or(false);
    if exists {
        return Err(RegistryError::DuplicatePersona(record.id));
    }

    let pa_json = serde_json::to_string(&record.project_attachments).map_err(|e| {
        tracing::warn!(target: "pattern_db::constellation", error = %e, "encode project_attachments");
        RegistryError::BackendUnavailable
    })?;
    let config_path_str = record
        .config_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    let tx = conn.transaction().map_err(map_sqlite_err)?;
    tx.execute(
        "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config,
                             enabled_tools, status, persona_status, config_path,
                             project_attachments, created_at, updated_at)
         VALUES (?1, ?2, '', '', '', '{}', '[]', 'active', ?3, ?4, ?5,
                 datetime('now'), datetime('now'))",
        params![
            record.id.as_str(),
            record.name,
            persona_status_to_str(record.status),
            config_path_str,
            pa_json,
        ],
    )
    .map_err(map_sqlite_err)?;

    // Carry through any relationships on the record (matches plan note: "Also
    // inserts any relationships carried on the record."). Use ON CONFLICT
    // DO NOTHING so duplicates are silently deduped.
    for edge in &record.relationships {
        let (from, to) = match edge.direction {
            EdgeDirection::Outgoing => (record.id.as_str(), edge.other.as_str()),
            EdgeDirection::Incoming => (edge.other.as_str(), record.id.as_str()),
        };
        tx.execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(from_persona, to_persona, kind) DO NOTHING",
            params![
                new_id().as_str(),
                from,
                to,
                relationship_kind_to_str(edge.kind)
            ],
        )
        .map_err(map_sqlite_err)?;
    }

    tx.commit().map_err(map_sqlite_err)?;
    Ok(())
}

fn add_relationship_blocking(
    conn: &mut Connection,
    edge: RelationshipSpec,
) -> Result<bool, RegistryError> {
    // Validate endpoints exist before insert (cleaner error than FK violation).
    for id in [edge.from.as_str(), edge.to.as_str()] {
        let exists: bool = conn
            .query_row("SELECT 1 FROM agents WHERE id = ?1", params![id], |_| {
                Ok(true)
            })
            .optional()
            .map_err(map_sqlite_err)?
            .unwrap_or(false);
        if !exists {
            return Err(RegistryError::PersonaNotFound(PersonaId::new(id)));
        }
    }

    // `ON CONFLICT DO NOTHING` returns 0 rows affected when the edge already
    // exists; 1 when a new row was inserted. We surface this to the caller so
    // event-emitting decorators can skip no-op insertions.
    let rows_affected = conn
        .execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(from_persona, to_persona, kind) DO NOTHING",
            params![
                new_id().as_str(),
                edge.from.as_str(),
                edge.to.as_str(),
                relationship_kind_to_str(edge.kind),
            ],
        )
        .map_err(map_sqlite_err)?;
    Ok(rows_affected > 0)
}

fn groups_blocking(
    conn: &Connection,
    scope: &RegistryScope,
) -> Result<Vec<PersonaGroup>, RegistryError> {
    let groups: Vec<(String, String, Option<String>)> = match scope {
        RegistryScope::All => {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, project_id FROM persona_groups
                     ORDER BY project_id, name",
                )
                .map_err(map_sqlite_err)?;
            stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let proj: Option<String> = row.get(2)?;
                Ok((id, name, proj))
            })
            .map_err(map_sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_err)?
        }
        RegistryScope::Project(p) => {
            let p_str = p.to_string_lossy().into_owned();
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, project_id FROM persona_groups
                     WHERE project_id = ?1
                     ORDER BY name",
                )
                .map_err(map_sqlite_err)?;
            stmt.query_map(params![p_str], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let proj: Option<String> = row.get(2)?;
                Ok((id, name, proj))
            })
            .map_err(map_sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_err)?
        }
    };

    // Batch-load members per group.
    let mut out = Vec::with_capacity(groups.len());
    let mut members_by_group: HashMap<String, Vec<PersonaId>> = HashMap::new();
    for (id, _, _) in &groups {
        members_by_group.insert(id.clone(), load_group_members(conn, id)?);
    }
    for (id, name, project_id) in groups {
        let members = members_by_group.remove(&id).unwrap_or_default();
        out.push(PersonaGroup::with_members(
            GroupId::new(id.as_str()),
            name,
            project_id,
            members,
        ));
    }
    Ok(out)
}

fn create_group_blocking(
    conn: &Connection,
    name: String,
    project_id: Option<String>,
) -> Result<PersonaGroup, RegistryError> {
    // Pre-check duplicate (matching the in-memory impl + plan AC).
    let collision: bool = conn
        .query_row(
            "SELECT 1 FROM persona_groups WHERE name = ?1 AND IFNULL(project_id, '') = IFNULL(?2, '')",
            params![name, project_id],
            |_| Ok(true),
        )
        .optional()
        .map_err(map_sqlite_err)?
        .unwrap_or(false);
    if collision {
        return Err(RegistryError::DuplicateGroup { name, project_id });
    }

    let id = new_id();
    conn.execute(
        "INSERT INTO persona_groups (id, name, project_id, created_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![id.as_str(), name, project_id],
    )
    .map_err(map_sqlite_err)?;

    Ok(PersonaGroup::new(
        GroupId::new(id.as_str()),
        name,
        project_id,
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn fresh_db() -> Arc<ConstellationDb> {
        Arc::new(ConstellationDb::open_in_memory().unwrap())
    }

    fn seed_persona(
        conn: &Connection,
        id: &str,
        name: &str,
        status: PersonaStatus,
        projects: &[&str],
    ) {
        let pa_json = serde_json::to_string(&projects.to_vec()).unwrap();
        conn.execute(
            "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config,
                                 enabled_tools, status, persona_status, config_path,
                                 project_attachments, created_at, updated_at)
             VALUES (?1, ?2, '', '', '', '{}', '[]', 'active', ?3, NULL, ?4,
                     datetime('now'), datetime('now'))",
            params![id, name, persona_status_to_str(status), pa_json],
        )
        .unwrap();
    }

    fn raw_insert_relationship(conn: &Connection, from: &str, to: &str, kind: RelationshipKind) {
        conn.execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            params![new_id().as_str(), from, to, relationship_kind_to_str(kind)],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn list_all_returns_all_seeded_personas() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "alice", "Alice", PersonaStatus::Active, &["/p1"]);
            seed_persona(&conn, "bob", "Bob", PersonaStatus::Draft, &["/p1", "/p2"]);
            seed_persona(&conn, "carol", "Carol", PersonaStatus::Inactive, &[]);
        }
        let reg = ConstellationRegistryDb::new(db);
        let all = reg.list(RegistryScope::All).await.unwrap();
        assert_eq!(all.len(), 3);
        let alice = all.iter().find(|r| r.id.as_str() == "alice").unwrap();
        assert_eq!(alice.status, PersonaStatus::Active);
        let bob = all.iter().find(|r| r.id.as_str() == "bob").unwrap();
        assert_eq!(bob.status, PersonaStatus::Draft);
        assert_eq!(bob.project_attachments.len(), 2);
    }

    #[tokio::test]
    async fn list_project_filters_via_json_each() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "a", "A", PersonaStatus::Active, &["/p1"]);
            seed_persona(&conn, "b", "B", PersonaStatus::Active, &["/p2"]);
            seed_persona(&conn, "c", "C", PersonaStatus::Active, &["/p1", "/p2"]);
        }
        let reg = ConstellationRegistryDb::new(db);
        let p1 = reg
            .list(RegistryScope::Project(PathBuf::from("/p1")))
            .await
            .unwrap();
        let ids: Vec<_> = p1.iter().map(|r| r.id.as_str().to_string()).collect();
        assert_eq!(p1.len(), 2);
        assert!(ids.contains(&"a".to_string()));
        assert!(ids.contains(&"c".to_string()));
    }

    #[tokio::test]
    async fn list_project_unknown_path_returns_empty_not_error() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "a", "A", PersonaStatus::Active, &["/p1"]);
        }
        let reg = ConstellationRegistryDb::new(db);
        let unknown = reg
            .list(RegistryScope::Project(PathBuf::from("/nowhere")))
            .await
            .unwrap();
        assert!(
            unknown.is_empty(),
            "unknown project must return empty vec, not error"
        );
    }

    #[tokio::test]
    async fn get_returns_some_then_none() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "alice", "Alice", PersonaStatus::Active, &[]);
        }
        let reg = ConstellationRegistryDb::new(db);
        let found = reg.get(&"alice".into()).await.unwrap().unwrap();
        assert_eq!(found.name, "Alice");
        let missing = reg.get(&"ghost".into()).await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn find_by_project_and_kind_filters_correctly() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "alice", "Alice", PersonaStatus::Active, &["/p1"]);
            seed_persona(&conn, "bob", "Bob", PersonaStatus::Active, &["/p1"]);
            seed_persona(&conn, "carol", "Carol", PersonaStatus::Active, &["/p2"]);
            raw_insert_relationship(&conn, "alice", "bob", RelationshipKind::SupervisorOf);
            raw_insert_relationship(&conn, "carol", "bob", RelationshipKind::PeerWith);
        }
        let reg = ConstellationRegistryDb::new(db);

        // project=/p1, kind=SupervisorOf → alice (only alice has an outgoing supervisor_of edge AND is in /p1).
        let combined = reg
            .find(
                Some(std::path::Path::new("/p1")),
                Some(RelationshipKind::SupervisorOf),
            )
            .await
            .unwrap();
        let ids: Vec<_> = combined.iter().map(|r| r.id.as_str().to_string()).collect();
        assert_eq!(ids, vec!["alice".to_string()]);

        // project=/p2 alone returns carol.
        let p2 = reg
            .find(Some(std::path::Path::new("/p2")), None)
            .await
            .unwrap();
        assert_eq!(p2.len(), 1);
        assert_eq!(p2[0].id.as_str(), "carol");
    }

    #[tokio::test]
    async fn register_inserts_record_and_relationships() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "bob", "Bob", PersonaStatus::Active, &[]);
        }
        let reg = ConstellationRegistryDb::new(db);

        let mut alice = PersonaRecord::new("alice", "Alice", PersonaStatus::Active);
        alice.project_attachments.push(PathBuf::from("/p1"));
        alice.relationships.push(RelationshipEdge {
            other: PersonaId::new("bob"),
            kind: RelationshipKind::SupervisorOf,
            direction: EdgeDirection::Outgoing,
        });
        reg.register(alice).await.unwrap();

        let loaded = reg.get(&"alice".into()).await.unwrap().unwrap();
        assert_eq!(loaded.name, "Alice");
        assert_eq!(loaded.project_attachments.len(), 1);
        assert_eq!(loaded.relationships.len(), 1);
        assert_eq!(loaded.relationships[0].other.as_str(), "bob");
        assert_eq!(loaded.relationships[0].direction, EdgeDirection::Outgoing);

        // Bob should now see an incoming edge from alice.
        let bob = reg.get(&"bob".into()).await.unwrap().unwrap();
        assert_eq!(bob.relationships.len(), 1);
        assert_eq!(bob.relationships[0].other.as_str(), "alice");
        assert_eq!(bob.relationships[0].direction, EdgeDirection::Incoming);
    }

    #[tokio::test]
    async fn register_duplicate_persona_errors() {
        let db = fresh_db();
        let reg = ConstellationRegistryDb::new(db);
        reg.register(PersonaRecord::new("alice", "Alice", PersonaStatus::Active))
            .await
            .unwrap();
        let err = reg
            .register(PersonaRecord::new("alice", "Alice2", PersonaStatus::Active))
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::DuplicatePersona(ref id) if id.as_str() == "alice"));
    }

    #[tokio::test]
    async fn set_status_updates_then_errors_on_missing() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "alice", "Alice", PersonaStatus::Draft, &[]);
        }
        let reg = ConstellationRegistryDb::new(db);
        reg.set_status(&"alice".into(), PersonaStatus::Active)
            .await
            .unwrap();
        let updated = reg.get(&"alice".into()).await.unwrap().unwrap();
        assert_eq!(updated.status, PersonaStatus::Active);

        let err = reg
            .set_status(&"ghost".into(), PersonaStatus::Active)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::PersonaNotFound(ref id) if id.as_str() == "ghost"));
    }

    #[tokio::test]
    async fn add_relationship_is_idempotent() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "alice", "Alice", PersonaStatus::Active, &[]);
            seed_persona(&conn, "bob", "Bob", PersonaStatus::Active, &[]);
        }
        let reg = ConstellationRegistryDb::new(db.clone());
        let spec = RelationshipSpec::new("alice", "bob", RelationshipKind::PeerWith);
        let first = reg.add_relationship(spec.clone()).await.unwrap();
        let second = reg.add_relationship(spec).await.unwrap();

        assert!(first, "first insert must return true (row was inserted)");
        assert!(
            !second,
            "second insert must return false (no-op; edge already existed)"
        );

        let count: i64 = db
            .get()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM persona_relationships
                 WHERE from_persona = 'alice' AND to_persona = 'bob' AND kind = 'peer_with'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "duplicate edge inserts must be deduped");
    }

    #[tokio::test]
    async fn add_relationship_missing_endpoint_errors() {
        let db = fresh_db();
        {
            let conn = db.get().unwrap();
            seed_persona(&conn, "alice", "Alice", PersonaStatus::Active, &[]);
        }
        let reg = ConstellationRegistryDb::new(db);
        let err = reg
            .add_relationship(RelationshipSpec::new(
                "alice",
                "ghost",
                RelationshipKind::PeerWith,
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::PersonaNotFound(ref id) if id.as_str() == "ghost"));
    }

    #[tokio::test]
    async fn create_group_then_duplicate_errors() {
        let db = fresh_db();
        let reg = ConstellationRegistryDb::new(db);
        let g = reg
            .create_group("support".into(), Some("proj-a".into()))
            .await
            .unwrap();
        assert_eq!(g.name, "support");

        let err = reg
            .create_group("support".into(), Some("proj-a".into()))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::DuplicateGroup { ref name, project_id: Some(ref p) }
                if name == "support" && p == "proj-a"
        ));

        // Same name, different project_id is fine.
        let _g2 = reg
            .create_group("support".into(), Some("proj-b".into()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn groups_filter_by_project_scope() {
        let db = fresh_db();
        let reg = ConstellationRegistryDb::new(db);
        reg.create_group("alpha".into(), Some("proj-a".into()))
            .await
            .unwrap();
        reg.create_group("beta".into(), Some("proj-b".into()))
            .await
            .unwrap();
        reg.create_group("global".into(), None).await.unwrap();

        let all = reg.groups(RegistryScope::All).await.unwrap();
        assert_eq!(all.len(), 3);

        let by_a = reg
            .groups(RegistryScope::Project(PathBuf::from("proj-a")))
            .await
            .unwrap();
        assert_eq!(by_a.len(), 1);
        assert_eq!(by_a[0].name, "alpha");
    }
}
