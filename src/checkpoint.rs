//! Project checkpoints capture authoritative relational state under one transaction.
//! Incremental storage uses canonical row-set differences, avoiding incomplete write hooks.
use crate::db::{Backend, BrainSnapshot, Database};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{AnyConnection, Row};
use std::collections::{BTreeMap, BTreeSet};

pub type Record = BTreeMap<String, Value>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub fields: Vec<(String, String)>,
    pub rows: Vec<Record>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub project: String,
    pub tables: BTreeMap<String, Table>,
    /// Exact source bytes, independent of physical codec/dictionary layout.
    pub sources: BTreeMap<String, String>,
    /// Historical evidence retained on export, never replayed over live audit history.
    pub audit_evidence: Vec<Record>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    removed: BTreeMap<String, Vec<Record>>,
    added: BTreeMap<String, Vec<Record>>,
    sources: BTreeMap<String, String>,
    removed_sources: Vec<String>,
    audit_evidence: Vec<Record>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub project: String,
    pub state_hash: String,
    pub parent: Option<String>,
    pub parent_hash: Option<String>,
    pub depth: usize,
    pub full: Option<State>,
    pub delta: Option<Delta>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn state_hash(state: &State) -> Result<String> {
    Ok(digest(&serde_json::to_vec(state)?))
}
fn id_col(db: &Database) -> &'static str {
    if matches!(db.backend, Backend::Sqlite) {
        "rowid"
    } else {
        "id"
    }
}
const TABLES: &[&str] = &[
    "sessions",
    "observations",
    "memories",
    "memory_meta",
    "memory_evidence_roots",
    "memory_influence_policy",
    "memory_entities",
    "memory_edges",
    "memory_chunks",
    "code_anchors",
    "reflection_proposals",
    "contradiction_sets",
    "contradiction_members",
];

fn filter(table: &str, id: &str) -> String {
    let memories = format!("SELECT {id} FROM memories WHERE project=$1");
    match table {
        "memory_meta" | "memory_evidence_roots" | "memory_influence_policy" | "memory_entities" => {
            format!("memory_id IN ({memories})")
        }
        "contradiction_members" => {
            "contradiction_set_id IN (SELECT id FROM contradiction_sets WHERE project=$1)".into()
        }
        _ => "project=$1".into(),
    }
}

async fn transaction(db: &Database) -> Result<sqlx::Transaction<'_, sqlx::Any>> {
    let mut tx = crate::db::begin_write(db).await?;
    match db.backend {
        Backend::Sqlite => {}
        Backend::Postgres => {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
                .execute(&mut *tx)
                .await?;
            // Serializes checkpoint publication; row diff parents follow committed state.
            sqlx::query("SELECT pg_advisory_xact_lock(734924113)")
                .execute(&mut *tx)
                .await?;
        }
    }
    Ok(tx)
}

async fn capture_table(
    conn: &mut AnyConnection,
    db: &Database,
    table: &str,
    project: &str,
) -> Result<Table> {
    let fields: Vec<(String,String)> = match db.backend {
        Backend::Sqlite => sqlx::query(&format!("PRAGMA table_info({table})")).fetch_all(&mut *conn).await?.into_iter()
            .map(|r| (r.get("name"),r.get::<String,_>("type"))).collect(),
        Backend::Postgres => sqlx::query("SELECT CAST(column_name AS TEXT) AS column_name,CAST(data_type AS TEXT) AS data_type FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY ordinal_position")
            .bind(table).fetch_all(&mut *conn).await?.into_iter().map(|r| (r.get("column_name"),r.get("data_type"))).collect(),
    };
    let mut fields = fields;
    if table == "memories" && matches!(db.backend, Backend::Sqlite) {
        fields.insert(0, ("rowid".into(), "BIGINT".into()));
    }
    ensure!(!fields.is_empty(), "missing checkpoint table {table}");
    let expression = fields
        .iter()
        .map(|(name, _)| format!("'{name}',{name}"))
        .collect::<Vec<_>>()
        .join(",");
    let predicate = if table == "memory_ledger" {
        format!(
            "memory_id IN (SELECT {} FROM memories WHERE project=$1)",
            id_col(db)
        )
    } else {
        filter(table, id_col(db))
    };
    let sql = if matches!(db.backend, Backend::Sqlite) {
        format!("SELECT json_object({expression}) AS record FROM {table} WHERE {predicate}")
    } else {
        format!("SELECT CAST(row_to_json(t) AS TEXT) AS record FROM (SELECT * FROM {table} WHERE {predicate}) t")
    };
    let mut rows: Vec<Record> = sqlx::query(&sql)
        .bind(project)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|r| serde_json::from_str(&r.get::<String, _>("record")))
        .collect::<std::result::Result<_, _>>()?;
    // Normalize SQLite FTS names/types to a portable relational schema.
    if table == "memories" {
        for row in &mut rows {
            row.remove("search_vector");
            if let Some(id) = row.remove("rowid") {
                row.insert("id".into(), id);
            }
        }
        fields = vec![
            ("id".into(), "BIGINT".into()),
            ("project".into(), "TEXT".into()),
            ("session_id".into(), "TEXT".into()),
            ("summary".into(), "TEXT".into()),
            ("tags".into(), "TEXT".into()),
            ("created_at".into(), "BIGINT".into()),
        ];
    }
    for (_, kind) in &mut fields {
        *kind = normalized_type(kind)?.into();
    }
    rows.sort_by_cached_key(|r| serde_json::to_string(r).expect("record serialization"));
    Ok(Table { fields, rows })
}
fn normalized_type(kind: &str) -> Result<&'static str> {
    match kind.to_ascii_lowercase().as_str() {
        "integer" | "bigint" | "bigserial" => Ok("BIGINT"),
        "real" | "double precision" => Ok("DOUBLE PRECISION"),
        "text" | "" => Ok("TEXT"),
        _ => anyhow::bail!("unsupported checkpoint column type {kind}"),
    }
}

async fn capture(
    conn: &mut AnyConnection,
    db: &Database,
    project: &str,
    include_sources: bool,
) -> Result<State> {
    let mut tables = BTreeMap::new();
    for table in TABLES {
        tables.insert(
            (*table).into(),
            capture_table(conn, db, table, project).await?,
        );
    }
    // Cross-project contradiction membership cannot be safely partially replaced.
    let cross: i64 = sqlx::query(&format!("SELECT COUNT(*) AS n FROM contradiction_members cm JOIN contradiction_sets cs ON cs.id=cm.contradiction_set_id WHERE (cm.memory_id IN (SELECT {} FROM memories WHERE project=$1) AND (cs.project IS NULL OR cs.project<>$1)) OR (cs.project=$1 AND cm.memory_id NOT IN (SELECT {} FROM memories WHERE project=$1))",id_col(db),id_col(db)))
        .bind(project).fetch_one(&mut *conn).await?.get("n");
    ensure!(cross == 0,"project has cross-project contradiction sets; checkpoint requires an explicit shared-scope export");
    let mut hashes = BTreeSet::new();
    for (table, field) in [
        ("memory_meta", "session_blob"),
        ("observations", "output_blob"),
        ("memory_chunks", "source_hash"),
    ] {
        for row in &tables[table].rows {
            if let Some(hash) = row.get(field).and_then(Value::as_str) {
                hashes.insert(hash.to_string());
            }
        }
    }
    let mut sources = BTreeMap::new();
    use base64::Engine;
    for hash in hashes.into_iter().filter(|_| include_sources) {
        let bytes = crate::ccr::load_blob(db, &hash).await?;
        sources.insert(
            hash,
            base64::engine::general_purpose::STANDARD.encode(bytes),
        );
    }
    let mut audit_evidence = capture_table(conn, db, "memory_ledger", project)
        .await?
        .rows;
    for row in &mut audit_evidence {
        row.insert("_evidence_table".into(), serde_json::json!("memory_ledger"));
    }
    for saved in sqlx::query("SELECT data FROM checkpoint_audit_evidence WHERE project=$1")
        .bind(project)
        .fetch_all(&mut *conn)
        .await?
    {
        audit_evidence.push(serde_json::from_str(&saved.get::<String, _>("data"))?);
    }
    for row in sqlx::query("SELECT id,snapshot_id,project,state_hash,created_at FROM checkpoint_restore_events WHERE project=$1 ORDER BY id").bind(project).fetch_all(&mut *conn).await? {
        audit_evidence.push(serde_json::from_value(serde_json::json!({"_evidence_table":"checkpoint_restore_events","id":row.get::<String,_>("id"),"snapshot_id":row.get::<String,_>("snapshot_id"),"project":row.get::<String,_>("project"),"state_hash":row.get::<String,_>("state_hash"),"created_at":row.get::<i64,_>("created_at")}))?);
    }
    audit_evidence.sort_by_cached_key(|r| serde_json::to_string(r).unwrap());
    audit_evidence.dedup();
    Ok(State {
        project: project.into(),
        tables,
        sources,
        audit_evidence,
    })
}

fn difference(base: &State, next: &State) -> Result<Delta> {
    ensure!(
        base.project == next.project && base.tables.keys().eq(next.tables.keys()),
        "checkpoint schemas differ"
    );
    let mut removed = BTreeMap::new();
    let mut added = BTreeMap::new();
    for (name, table) in &next.tables {
        ensure!(
            table.fields == base.tables[name].fields,
            "checkpoint fields differ"
        );
        let mut old = BTreeMap::<String, usize>::new();
        let mut new = BTreeMap::<String, usize>::new();
        for row in &base.tables[name].rows {
            *old.entry(serde_json::to_string(row)?).or_default() += 1;
        }
        for row in &table.rows {
            *new.entry(serde_json::to_string(row)?).or_default() += 1;
        }
        let mut deletes = Vec::new();
        let mut inserts = Vec::new();
        for (row, count) in &old {
            for _ in 0..count.saturating_sub(*new.get(row).unwrap_or(&0)) {
                deletes.push(serde_json::from_str(row)?);
            }
        }
        for (row, count) in &new {
            for _ in 0..count.saturating_sub(*old.get(row).unwrap_or(&0)) {
                inserts.push(serde_json::from_str(row)?);
            }
        }
        removed.insert(name.clone(), deletes);
        added.insert(name.clone(), inserts);
    }
    Ok(Delta {
        removed,
        added,
        sources: next
            .sources
            .iter()
            .filter(|(h, _)| !base.sources.contains_key(*h))
            .map(|(h, b)| (h.clone(), b.clone()))
            .collect(),
        removed_sources: base
            .sources
            .keys()
            .filter(|h| !next.sources.contains_key(*h))
            .cloned()
            .collect(),
        audit_evidence: next.audit_evidence.clone(),
    })
}
fn apply_delta(mut base: State, delta: &Delta) -> Result<State> {
    ensure!(
        base.tables.keys().eq(delta.added.keys()) && base.tables.keys().eq(delta.removed.keys()),
        "delta table set mismatch"
    );
    for (name, table) in &mut base.tables {
        for row in &delta.removed[name] {
            let index = table
                .rows
                .iter()
                .position(|r| r == row)
                .ok_or_else(|| anyhow::anyhow!("delta removes absent record"))?;
            table.rows.remove(index);
        }
        for row in &delta.added[name] {
            table.rows.push(row.clone());
        }
        table
            .rows
            .sort_by_cached_key(|r| serde_json::to_string(r).unwrap());
    }
    for hash in &delta.removed_sources {
        ensure!(
            base.sources.remove(hash).is_some(),
            "delta removes absent source"
        );
    }
    base.sources.extend(delta.sources.clone());
    base.audit_evidence = delta.audit_evidence.clone();
    Ok(base)
}

pub async fn load(db: &Database, hash: &str) -> Result<State> {
    let mut next = hash.to_owned();
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..=8 {
        ensure!(seen.insert(next.clone()), "cyclic checkpoint chain");
        let bytes = crate::ccr::load_blob(db, &next).await?;
        let e: Envelope = serde_json::from_slice(&bytes)?;
        ensure!(
            e.version == 5 && e.depth <= 8 && (e.full.is_some() ^ e.delta.is_some()),
            "invalid checkpoint envelope"
        );
        let parent = e.parent.clone();
        chain.push(e);
        if let Some(parent) = parent {
            next = parent;
        } else {
            break;
        }
    }
    let full = chain
        .pop()
        .ok_or_else(|| anyhow::anyhow!("empty checkpoint chain"))?;
    ensure!(
        full.depth == 0 && full.parent.is_none() && full.parent_hash.is_none(),
        "checkpoint chain has no base"
    );
    let mut state = full
        .full
        .ok_or_else(|| anyhow::anyhow!("checkpoint base missing"))?;
    ensure!(
        state_hash(&state)? == full.state_hash,
        "checkpoint state hash mismatch"
    );
    let mut depth = 0;
    while let Some(e) = chain.pop() {
        depth += 1;
        ensure!(
            e.project == state.project
                && e.depth == depth
                && e.parent_hash.as_deref() == Some(&state_hash(&state)?),
            "checkpoint parent mismatch"
        );
        state = apply_delta(
            state,
            &e.delta.ok_or_else(|| anyhow::anyhow!("missing delta"))?,
        )?;
        ensure!(
            state_hash(&state)? == e.state_hash,
            "delta result hash mismatch"
        );
    }
    validate(&state)?;
    Ok(state)
}
fn validate(state: &State) -> Result<()> {
    ensure!(
        state.tables.len() == TABLES.len() && TABLES.iter().all(|t| state.tables.contains_key(*t)),
        "checkpoint table inventory mismatch"
    );
    for table in state.tables.values() {
        for (name, kind) in &table.fields {
            ensure!(
                name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid checkpoint column"
            );
            normalized_type(kind)?;
        }
        for row in &table.rows {
            ensure!(
                row.len() == table.fields.len()
                    && table.fields.iter().all(|(f, _)| row.contains_key(f)),
                "record schema mismatch"
            );
        }
    }
    for table in [
        "sessions",
        "observations",
        "memories",
        "memory_edges",
        "memory_chunks",
        "code_anchors",
        "reflection_proposals",
        "contradiction_sets",
    ] {
        for row in &state.tables[table].rows {
            ensure!(
                row.get("project").and_then(Value::as_str) == Some(&state.project),
                "cross-project checkpoint record"
            );
        }
    }
    let ids: BTreeSet<i64> = state.tables["memories"]
        .rows
        .iter()
        .map(|r| {
            r.get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("invalid memory ID"))
        })
        .collect::<Result<_>>()?;
    ensure!(
        ids.len() == state.tables["memories"].rows.len(),
        "duplicate memory identity"
    );
    for name in [
        "memory_meta",
        "memory_evidence_roots",
        "memory_influence_policy",
        "memory_entities",
        "memory_edges",
        "memory_chunks",
        "code_anchors",
        "contradiction_members",
    ] {
        for row in &state.tables[name].rows {
            ensure!(
                row.get("memory_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| ids.contains(&id)),
                "foreign memory in checkpoint table {name}"
            );
        }
    }
    use base64::Engine;
    for (hash, encoded) in &state.sources {
        ensure!(
            digest(&base64::engine::general_purpose::STANDARD.decode(encoded)?) == *hash,
            "checkpoint source mismatch"
        );
    }
    for (table, field) in [
        ("memory_meta", "session_blob"),
        ("observations", "output_blob"),
        ("memory_chunks", "source_hash"),
    ] {
        for row in &state.tables[table].rows {
            if let Some(hash) = row.get(field).and_then(Value::as_str) {
                ensure!(
                    state.sources.contains_key(hash),
                    "checkpoint source closure incomplete"
                );
            }
        }
    }
    Ok(())
}

async fn persist_blob(conn: &mut AnyConnection, bytes: &[u8]) -> Result<String> {
    let hash = digest(bytes);
    let compressed = zstd::bulk::compress(bytes, 19)?;
    sqlx::query("INSERT INTO blobs(hash,content_type,codec,orig_len,comp_len,data,refcount,created_at,dict_hash) VALUES($1,'json','zstd',$2,$3,$4,1,$5,NULL) ON CONFLICT(hash) DO UPDATE SET refcount=blobs.refcount+1")
        .bind(&hash).bind(bytes.len() as i64).bind(compressed.len() as i64).bind(compressed).bind(chrono::Utc::now().timestamp()).execute(&mut *conn).await?;
    Ok(hash)
}

pub async fn create(
    db: &Database,
    label: Option<&str>,
    project: &str,
    incremental: bool,
) -> Result<BrainSnapshot> {
    let mut tx = transaction(db).await?;
    let state = capture(&mut tx, db, project, true).await?;
    validate(&state)?;
    let state_hash = state_hash(&state)?;
    let mut envelope = Envelope {
        version: 5,
        project: project.into(),
        state_hash,
        parent: None,
        parent_hash: None,
        depth: 0,
        full: Some(state.clone()),
        delta: None,
    };
    if incremental {
        let previous = sqlx::query("SELECT blob_hash FROM checkpoint_heads WHERE project=$1")
            .bind(project)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(previous) = previous {
            let hash: String = previous.get("blob_hash");
            let bytes = crate::ccr::load_blob(db, &hash).await?;
            if let Ok(parent) = serde_json::from_slice::<Envelope>(&bytes) {
                let base = load(db, &hash).await?;
                if parent.state_hash == envelope.state_hash {
                    envelope = parent;
                } else if parent.depth < 8 {
                    let delta = difference(&base, &state)?;
                    let candidate = Envelope {
                        version: 5,
                        project: project.into(),
                        state_hash: envelope.state_hash.clone(),
                        parent: Some(hash),
                        parent_hash: Some(parent.state_hash),
                        depth: parent.depth + 1,
                        full: None,
                        delta: Some(delta),
                    };
                    if serde_json::to_vec(&candidate)?.len() * 2
                        < serde_json::to_vec(&envelope)?.len()
                    {
                        envelope = candidate;
                    }
                }
            }
        }
    }
    let hash = persist_blob(&mut tx, &serde_json::to_vec(&envelope)?).await?;
    if let Some(parent) = &envelope.parent {
        sqlx::query("INSERT INTO checkpoint_dependencies(child_hash,parent_hash) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(&hash).bind(parent).execute(&mut *tx).await?;
    }
    let snap = BrainSnapshot {
        id: format!("snap-{}", uuid::Uuid::new_v4()),
        label: label.map(str::to_owned),
        project: Some(project.into()),
        memory_count: state.tables["memories"].rows.len() as i64,
        edge_count: state.tables["memory_edges"].rows.len() as i64,
        blob_hash: hash,
        created_at: chrono::Utc::now().timestamp(),
    };
    sqlx::query("INSERT INTO brain_snapshots(id,label,project,memory_count,edge_count,blob_hash,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(&snap.id).bind(&snap.label).bind(project).bind(snap.memory_count).bind(snap.edge_count).bind(&snap.blob_hash).bind(snap.created_at).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO checkpoint_heads(project,blob_hash) VALUES($1,$2) ON CONFLICT(project) DO UPDATE SET blob_hash=excluded.blob_hash")
        .bind(project).bind(&snap.blob_hash).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(snap)
}

async fn insert_record(
    conn: &mut AnyConnection,
    db: &Database,
    name: &str,
    table: &Table,
    row: &Record,
) -> Result<()> {
    let columns = table
        .fields
        .iter()
        .map(|(f, _)| {
            if name == "memories" && f == "id" {
                id_col(db).to_owned()
            } else {
                f.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let values = table
        .fields
        .iter()
        .enumerate()
        .map(|(i, (_, kind))| format!("CAST(${} AS {kind})", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("INSERT INTO {name}({columns}) VALUES({values})");
    let mut query = sqlx::query(&sql);
    for (field, _) in &table.fields {
        let value = &row[field];
        let text = match value {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => anyhow::bail!("non-scalar checkpoint value"),
        };
        query = query.bind(text);
    }
    query.execute(&mut *conn).await?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct RestoreCounts {
    pub memories: usize,
    pub edges: usize,
    pub policies: usize,
    pub contradictions: usize,
}

/// Validation and replacement share the transaction. Current policy/revocation wins.
pub async fn restore(
    db: &Database,
    state: &State,
    snapshot_id: &str,
    dry_run: bool,
) -> Result<RestoreCounts> {
    validate(state)?;
    let mut tx = transaction(db).await?;
    if matches!(db.backend, Backend::Postgres) {
        sqlx::query(&format!(
            "LOCK TABLE {} IN SHARE ROW EXCLUSIVE MODE",
            TABLES.join(",")
        ))
        .execute(&mut *tx)
        .await?;
    }
    let current = capture(&mut tx, db, &state.project, false).await?;
    for name in TABLES {
        ensure!(
            current.tables[*name].fields == state.tables[*name].fields,
            "checkpoint schema differs for {name}"
        );
    }
    let mut replacement = state.clone();
    // Never roll authorization backwards when restoring content.
    for row in &mut replacement.tables.get_mut("memory_meta").unwrap().rows {
        if let Some(live) = current.tables["memory_meta"]
            .rows
            .iter()
            .find(|r| r["memory_id"] == row["memory_id"])
        {
            for field in ["namespace", "source_type", "writer_identity"] {
                ensure!(live[field]==row[field],"current {field} differs from historical provenance; use a separate recovery database");
            }
            for field in ["residency", "retention_policy_id"] {
                ensure!(
                    live[field].is_null() || row[field].is_null() || live[field] == row[field],
                    "incompatible current and historical {field}"
                );
                if !live[field].is_null() {
                    row.insert(field.into(), live[field].clone());
                }
            }
            if live["scope"] == "project" {
                row.insert("scope".into(), serde_json::json!("project"));
            }
            let tiers = ["untrusted", "low", "medium", "high"];
            let rank = |v: &Value| {
                tiers
                    .iter()
                    .position(|t| v.as_str() == Some(t))
                    .unwrap_or(0)
            };
            if rank(&live["trust_tier"]) < rank(&row["trust_tier"]) {
                row.insert("trust_tier".into(), live["trust_tier"].clone());
            }
            let old = row["classification"].as_str().unwrap_or("");
            let new = live["classification"].as_str().unwrap_or("");
            let classes = ["public", "internal", "confidential", "restricted"];
            if old != new {
                match (
                    classes.iter().position(|c| *c == old),
                    classes.iter().position(|c| *c == new),
                ) {
                    (Some(a), Some(b)) if b > a => {
                        row.insert("classification".into(), live["classification"].clone());
                    }
                    (Some(_), Some(_)) => {}
                    _ => anyhow::bail!(
                        "incompatible sensitive-data classifications; review required"
                    ),
                }
            }
            if matches!(
                live["consent_state"].as_str(),
                Some("denied" | "withdrawn" | "required")
            ) {
                row.insert("consent_state".into(), live["consent_state"].clone());
            }
            let expiration = [row["expires_at"].as_i64(), live["expires_at"].as_i64()]
                .into_iter()
                .flatten()
                .min();
            row.insert("expires_at".into(), serde_json::json!(expiration));
            if live["legal_hold"].as_i64() == Some(1) {
                row.insert("legal_hold".into(), serde_json::json!(1));
            }
            if !live["tombstoned_at"].is_null() {
                row.insert("tombstoned_at".into(), live["tombstoned_at"].clone());
                row.insert("tombstone_reason".into(), live["tombstone_reason"].clone());
            }
        } else {
            let historical: i64 =
                sqlx::query("SELECT COUNT(*) AS n FROM memory_ledger WHERE memory_id=$1")
                    .bind(
                        row["memory_id"]
                            .as_i64()
                            .ok_or_else(|| anyhow::anyhow!("invalid memory identity"))?,
                    )
                    .fetch_one(&mut *tx)
                    .await?
                    .get("n");
            let local_snapshot: i64 =
                sqlx::query("SELECT COUNT(*) AS n FROM brain_snapshots WHERE id=$1")
                    .bind(snapshot_id)
                    .fetch_one(&mut *tx)
                    .await?
                    .get("n");
            if historical > 0 || local_snapshot > 0 {
                row.insert(
                    "tombstoned_at".into(),
                    serde_json::json!(chrono::Utc::now().timestamp()),
                );
                row.insert(
                    "tombstone_reason".into(),
                    Value::String(
                        "Historical memory restored after removal; review required".into(),
                    ),
                );
            }
        }
    }
    let memories = replacement.tables["memories"].rows.clone();
    for row in &mut replacement.tables.get_mut("memory_meta").unwrap().rows {
        let memory: crate::db::Memory = serde_json::from_value(serde_json::to_value(
            memories
                .iter()
                .find(|m| m["id"] == row["memory_id"])
                .ok_or_else(|| anyhow::anyhow!("missing governed memory"))?,
        )?)?;
        let mut governance_json = serde_json::to_value(&*row)?;
        governance_json["legal_hold"] = serde_json::json!(row["legal_hold"].as_i64() == Some(1));
        let governance: crate::governance::MemoryGovernance =
            serde_json::from_value(governance_json)?;
        let hash = crate::governance::memory_record_hash(
            &memory.project,
            &memory.session_id,
            &memory.summary,
            memory.tags.as_deref(),
            row["scope"].as_str().unwrap_or("project"),
            row["kind"].as_str().unwrap_or("session"),
            &governance,
        );
        row.insert("record_hash".into(), serde_json::json!(hash));
    }
    for policy in &current.tables["memory_influence_policy"].rows {
        if replacement.tables["memories"]
            .rows
            .iter()
            .any(|r| r["id"] == policy["memory_id"])
        {
            let rows = &mut replacement
                .tables
                .get_mut("memory_influence_policy")
                .unwrap()
                .rows;
            if let Some(index) = rows
                .iter()
                .position(|r| r["memory_id"] == policy["memory_id"])
            {
                rows[index] = policy.clone();
            } else {
                rows.push(policy.clone());
            }
        }
    }
    // Detect identity collisions before SQLite FTS can overwrite another project's rowid.
    for name in TABLES {
        for row in &replacement.tables[*name].rows {
            if let Some(id) = row.get("id") {
                let column = if *name == "memories" {
                    id_col(db)
                } else {
                    "id"
                };
                let sql=format!("SELECT COUNT(*) AS n FROM {name} WHERE CAST({column} AS TEXT)=$1 AND project<>$2");
                let value = id
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string());
                let count: i64 = sqlx::query(&sql)
                    .bind(value)
                    .bind(&state.project)
                    .fetch_one(&mut *tx)
                    .await?
                    .get("n");
                ensure!(
                    count == 0,
                    "checkpoint identity collision in {name}; use a separate database"
                );
            }
        }
    }
    if dry_run {
        tx.rollback().await?;
        return Ok(RestoreCounts::default());
    }
    let predicate = format!(
        "owner_type='memory' AND owner_id IN (SELECT {} FROM memories WHERE project=$1)",
        id_col(db)
    );
    sqlx::query(&format!("DELETE FROM embeddings WHERE {predicate}"))
        .bind(&state.project)
        .execute(&mut *tx)
        .await?;
    let vector_table = if matches!(db.backend, Backend::Sqlite) {
        "vec_memories"
    } else {
        "memory_embeddings"
    };
    let exists: i64 = if matches!(db.backend, Backend::Sqlite) {
        sqlx::query("SELECT COUNT(*) AS n FROM sqlite_master WHERE name=$1")
            .bind(vector_table)
            .fetch_one(&mut *tx)
            .await?
            .get("n")
    } else {
        sqlx::query("SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema=current_schema() AND table_name=$1").bind(vector_table).fetch_one(&mut *tx).await?.get("n")
    };
    if exists > 0 {
        sqlx::query(&format!("DELETE FROM {vector_table} WHERE memory_id IN (SELECT {} FROM memories WHERE project=$1)",id_col(db))).bind(&state.project).execute(&mut *tx).await?;
    }
    if matches!(db.backend, Backend::Sqlite) {
        sqlx::query("UPDATE memory_identity_highwater SET maximum=MAX(maximum,COALESCE((SELECT MAX(rowid) FROM memories),0),COALESCE((SELECT MAX(memory_id) FROM memory_meta),0)) WHERE singleton=1").execute(&mut *tx).await?;
    }
    // Delete children before their relational parents. IDs stay stable for handles and history.
    for name in TABLES.iter().rev() {
        sqlx::query(&format!(
            "DELETE FROM {name} WHERE {}",
            filter(name, id_col(db))
        ))
        .bind(&state.project)
        .execute(&mut *tx)
        .await?;
    }
    use base64::Engine;
    for (hash, encoded) in &state.sources {
        let original = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        let compressed = zstd::bulk::compress(&original, 19)?;
        sqlx::query("INSERT INTO blobs(hash,content_type,codec,orig_len,comp_len,data,refcount,created_at,dict_hash) VALUES($1,'binary','zstd',$2,$3,$4,0,$5,NULL) ON CONFLICT(hash) DO UPDATE SET codec='zstd',orig_len=excluded.orig_len,comp_len=excluded.comp_len,data=excluded.data,dict_hash=NULL")
            .bind(hash).bind(original.len() as i64).bind(compressed.len() as i64).bind(compressed).bind(chrono::Utc::now().timestamp()).execute(&mut *tx).await?;
    }
    for hash in state.sources.keys() {
        sqlx::query("DELETE FROM ccr_object_chunks WHERE object_hash=$1")
            .bind(hash)
            .execute(&mut *tx)
            .await?;
    }
    for name in TABLES {
        let table = &replacement.tables[*name];
        for row in &table.rows {
            insert_record(&mut tx, db, name, table, row).await?;
        }
        if matches!(db.backend, Backend::Postgres)
            && table.fields.iter().any(|(f, k)| f == "id" && k == "BIGINT")
        {
            // Reserve future IDs above restored stable identities.
            sqlx::query(&format!("SELECT setval(pg_get_serial_sequence('{name}','id'),GREATEST(COALESCE((SELECT MAX(id) FROM {name}),1),nextval(pg_get_serial_sequence('{name}','id')),1),true)"))
                .execute(&mut *tx).await?;
        }
    }
    if matches!(db.backend, Backend::Postgres) {
        sqlx::query("UPDATE memories SET search_vector=to_tsvector('english',summary || ' ' || COALESCE(tags,'')) WHERE project=$1").bind(&state.project).execute(&mut *tx).await?;
    }
    for record in &state.audit_evidence {
        let data = serde_json::to_string(record)?;
        sqlx::query("INSERT INTO checkpoint_audit_evidence(project,evidence_hash,data) VALUES($1,$2,$3) ON CONFLICT(project,evidence_hash) DO NOTHING").bind(&state.project).bind(digest(data.as_bytes())).bind(data).execute(&mut *tx).await?;
    }
    sqlx::query(&format!("DELETE FROM embeddings WHERE {predicate}"))
        .bind(&state.project)
        .execute(&mut *tx)
        .await?;
    if exists > 0 {
        sqlx::query(&format!("DELETE FROM {vector_table} WHERE memory_id IN (SELECT {} FROM memories WHERE project=$1)",id_col(db))).bind(&state.project).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO checkpoint_restore_events(id,snapshot_id,project,state_hash,created_at) VALUES($1,$2,$3,$4,$5)")
        .bind(uuid::Uuid::new_v4().to_string()).bind(snapshot_id).bind(&state.project).bind(state_hash(&replacement)?).bind(chrono::Utc::now().timestamp()).execute(&mut *tx).await?;
    tx.commit().await?;
    // Invalidate generated context after the durable state transition. If removal fails,
    // report it rather than claiming a stale file is safe. Embeddings are rebuilt on demand.
    let path = std::path::Path::new(&state.project).join("IRONMEM.md");
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => anyhow::bail!("checkpoint restored, but stale context removal failed: {e}"),
    }
    Ok(RestoreCounts {
        memories: replacement.tables["memories"].rows.len(),
        edges: replacement.tables["memory_edges"].rows.len(),
        policies: replacement.tables["memory_influence_policy"].rows.len(),
        contradictions: replacement.tables["contradiction_sets"].rows.len(),
    })
}

pub async fn export(db: &Database, snapshot_id: &str, path: &std::path::Path) -> Result<()> {
    let snap = crate::db::brain_snapshot(db, snapshot_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("snapshot not found"))?;
    let state = load(db, &snap.blob_hash).await?;
    let e = Envelope {
        version: 5,
        project: state.project.clone(),
        state_hash: state_hash(&state)?,
        parent: None,
        parent_hash: None,
        depth: 0,
        full: Some(state),
        delta: None,
    };
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec(&e)?)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    Ok(())
}

pub async fn import(db: &Database, path: &std::path::Path, dry_run: bool) -> Result<usize> {
    let e: Envelope = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        e.version == 5 && e.depth == 0 && e.parent.is_none() && e.delta.is_none(),
        "export must be a full independent checkpoint"
    );
    let state = e
        .full
        .ok_or_else(|| anyhow::anyhow!("missing exported state"))?;
    ensure!(
        state.project == e.project && state_hash(&state)? == e.state_hash,
        "export integrity mismatch"
    );
    Ok(restore(db, &state, "import", dry_run).await?.memories)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> Result<(tempfile::TempDir, Database, String, i64)> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        std::fs::create_dir(&project)?;
        let db = Database::new(dir.path().join("state.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let project = project.to_str().unwrap().to_string();
        let session = crate::db::create_session(&db, &project).await?;
        let id = crate::db::insert_memory(&db, &project, &session, "fact ✓", Some("tag")).await?;
        let blob = crate::ccr::store_blob(&db, b"exact original \x00\xff", None).await?;
        crate::db::set_memory_session_blob(&db, id, &blob.hash).await?;
        Ok((dir, db, project, id))
    }
    #[tokio::test]
    #[ignore = "requires an isolated IRONMEM_TEST_POSTGRES_URL database"]
    async fn postgres_foundations() -> Result<()> {
        let url = std::env::var("IRONMEM_TEST_POSTGRES_URL")?;
        let db = Database::new(&url).await?;
        db.migrate().await?;
        let dir = tempfile::tempdir()?;
        let project = dir.path().to_str().unwrap();
        let session = crate::db::create_session(&db, project).await?;
        let id =
            crate::db::insert_memory(&db, project, &session, "Postgres portable ✓", None).await?;
        let bytes = crate::density::corpus(86).remove(2);
        let blob = crate::ccr::chunked::store(&db, &bytes, None).await?;
        crate::db::set_memory_session_blob(&db, id, &blob.hash).await?;
        let snap = create(&db, None, project, false).await?;
        let full = load(&db, &snap.blob_hash).await?;
        let delta = create(&db, None, project, true).await?;
        assert_eq!(load(&db, &delta.blob_hash).await?, full);
        restore(&db, &full, &snap.id, false).await?;
        crate::db::decref_blob(&db, &blob.hash).await?;
        crate::db::gc_blobs(&db).await?;
        assert_eq!(crate::ccr::load_blob(&db, &blob.hash).await?, bytes);
        let path = dir.path().join("export.json");
        export(&db, &snap.id, &path).await?;
        assert!(import(&db, &path, true).await.is_ok());
        db.pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn full_delta_export_restore_preserve_source_and_identity() -> Result<()> {
        let (dir, db, project, id) = fixture().await?;
        let full = create(&db, None, &project, false).await?;
        let before = load(&db, &full.blob_hash).await?;
        sqlx::query("UPDATE memories SET summary='updated' WHERE rowid=$1")
            .bind(id)
            .execute(&db.pool)
            .await?;
        let delta = create(&db, None, &project, true).await?;
        let after = load(&db, &delta.blob_hash).await?;
        assert_ne!(state_hash(&before)?, state_hash(&after)?);
        restore(&db, &before, &full.id, false).await?;
        assert_eq!(
            crate::db::memories_by_ids_in_namespace(&db, &[id], "local").await?[0].summary,
            "fact ✓"
        );
        assert_eq!(
            crate::expansion::retrieve_original(&db, None, Some(id), None, None)
                .await?
                .bytes,
            b"exact original \x00\xff".len()
        );
        let path = dir.path().join("backup.json");
        export(&db, &delta.id, &path).await?;
        let fresh = Database::new(dir.path().join("fresh.db").to_str().unwrap()).await?;
        fresh.migrate().await?;
        import(&fresh, &path, false).await?;
        crate::db::gc_blobs(&fresh).await?;
        assert_eq!(
            crate::db::memories_by_ids_in_namespace(&fresh, &[id], "local").await?[0].summary,
            "updated"
        );
        assert!(
            crate::expansion::retrieve_original(&fresh, None, Some(id), None, None)
                .await
                .is_ok()
        );
        db.pool.close().await;
        fresh.pool.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn restore_never_reuses_removed_memory_handles() -> Result<()> {
        let (_dir, db, project, _) = fixture().await?;
        let snap = create(&db, None, &project, false).await?;
        let session = crate::db::create_session(&db, &project).await?;
        let removed =
            crate::db::insert_memory(&db, &project, &session, "newer memory", None).await?;
        restore(&db, &load(&db, &snap.blob_hash).await?, &snap.id, false).await?;
        let fresh = crate::db::insert_memory(&db, &project, &session, "later memory", None).await?;
        assert!(fresh > removed, "old handles must not alias new memories");
        db.pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn damaged_source_recovery_and_dependency_pruning() -> Result<()> {
        let (_dir, db, project, id) = fixture().await?;
        // Force enough unchanged state for a real delta, then test chain integrity.
        for n in 0..12 {
            let session = crate::db::create_session(&db, &project).await?;
            crate::db::insert_memory(
                &db,
                &project,
                &session,
                &format!("memory {n} {}", "retained context ".repeat(100)),
                None,
            )
            .await?;
        }
        let full = create(&db, None, &project, false).await?;
        let no_change = create(&db, None, &project, true).await?;
        assert_eq!(
            full.blob_hash, no_change.blob_hash,
            "no-change snapshot must reuse payload"
        );
        sqlx::query("UPDATE memories SET summary='new' WHERE rowid=$1")
            .bind(id)
            .execute(&db.pool)
            .await?;
        let delta = create(&db, None, &project, true).await?;
        let envelope: Envelope =
            serde_json::from_slice(&crate::ccr::load_blob(&db, &delta.blob_hash).await?)?;
        assert!(envelope.delta.is_some());
        delete(&db, &full.id).await?;
        delete(&db, &no_change.id).await?;
        crate::db::gc_blobs(&db).await?;
        let state = load(&db, &delta.blob_hash).await?;
        let source = crate::db::get_memory_session_blob(&db, id).await?.unwrap();
        sqlx::query("UPDATE blobs SET data=$1 WHERE hash=$2")
            .bind(b"damaged".to_vec())
            .bind(&source)
            .execute(&db.pool)
            .await?;
        restore(&db, &state, &delta.id, false).await?;
        assert_eq!(
            crate::ccr::load_blob(&db, &source).await?,
            b"exact original \x00\xff"
        );
        delete(&db, &delta.id).await?;
        for _ in 0..3 {
            crate::db::gc_blobs(&db).await?;
        }
        assert!(crate::db::get_blob(&db, &full.blob_hash).await?.is_none());
        db.pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn failed_replacement_rolls_back_and_policy_never_rolls_back() -> Result<()> {
        let (_dir, db, project, id) = fixture().await?;
        let snap = create(&db, None, &project, false).await?;
        let state = load(&db, &snap.blob_hash).await?;
        sqlx::query("UPDATE memory_meta SET tombstoned_at=123,tombstone_reason='revoked' WHERE memory_id=$1").bind(id).execute(&db.pool).await?;
        sqlx::query("CREATE TRIGGER fail_checkpoint BEFORE INSERT ON memory_meta BEGIN SELECT RAISE(ABORT,'injected fault'); END").execute(&db.pool).await?;
        assert!(restore(&db, &state, &snap.id, false).await.is_err());
        assert_eq!(
            crate::db::memories_by_ids_in_namespace(&db, &[id], "local")
                .await?
                .len(),
            0
        ); // tombstoned remains hidden
        sqlx::query("DROP TRIGGER fail_checkpoint")
            .execute(&db.pool)
            .await?;
        restore(&db, &state, &snap.id, false).await?;
        let tombstone: i64 =
            sqlx::query("SELECT tombstoned_at FROM memory_meta WHERE memory_id=$1")
                .bind(id)
                .fetch_one(&db.pool)
                .await?
                .get("tombstoned_at");
        assert_eq!(tombstone, 123);
        db.pool.close().await;
        Ok(())
    }
}

/// Remove a snapshot root; descendant dependencies continue pinning its blob.
pub async fn delete(db: &Database, snapshot_id: &str) -> Result<()> {
    let mut tx = transaction(db).await?;
    let row = sqlx::query("DELETE FROM brain_snapshots WHERE id=$1 RETURNING blob_hash,project")
        .bind(snapshot_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("snapshot not found"))?;
    let hash: String = row.get("blob_hash");
    let project: Option<String> = row.try_get("project")?;
    sqlx::query("UPDATE blobs SET refcount=refcount-1 WHERE hash=$1 AND refcount>0")
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    if let Some(project) = project {
        sqlx::query("DELETE FROM checkpoint_heads WHERE project=$1 AND blob_hash=$2")
            .bind(&project)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO checkpoint_heads(project,blob_hash) SELECT project,blob_hash FROM brain_snapshots WHERE project=$1 ORDER BY created_at DESC,id DESC LIMIT 1 ON CONFLICT(project) DO NOTHING").bind(project).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// External index coordination requires an offline rebuild; do not serve stale mirrors.
pub fn ensure_native_restore(cfg: &crate::config::Config, dry_run: bool) -> Result<()> {
    ensure!(dry_run || (cfg.storage.vector_backend=="native" && cfg.storage.graph_backend=="native"),"restore with external indexes requires an isolated native database and an external index rebuild before serving it");
    Ok(())
}
