//! Transactional row-mutation telemetry for supported mutable memory metadata.
//! This is an operational counter, not a semantic replay journal.
use crate::db::{Backend, Database};
use anyhow::Result;

pub async fn migrate(db: &Database) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_mutations(memory_id BIGINT PRIMARY KEY REFERENCES memory_meta(memory_id) ON DELETE CASCADE, observed_since BIGINT NOT NULL, mutation_count BIGINT NOT NULL DEFAULT 0)").execute(&db.pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS generated_context_files(project TEXT PRIMARY KEY, content_hash TEXT NOT NULL, dirty BIGINT NOT NULL DEFAULT 0)").execute(&db.pool).await?;
    sqlx::query("INSERT INTO memory_mutations(memory_id,observed_since) SELECT memory_id,$1 FROM memory_meta WHERE 1=1 ON CONFLICT(memory_id) DO NOTHING").bind(chrono::Utc::now().timestamp()).execute(&db.pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS context_revision(singleton BIGINT PRIMARY KEY,revision BIGINT NOT NULL)").execute(&db.pool).await?;
    sqlx::query("INSERT INTO context_revision(singleton,revision) VALUES(1,0) ON CONFLICT(singleton) DO NOTHING").execute(&db.pool).await?;
    // These tables cover supported mutable memory state. Memories themselves are
    // immutable through the application (replacement gets a new ID). CCR bytes
    // and session observations are not semantic memory mutations.
    for table in [
        "memory_meta",
        "memory_influence_policy",
        "memory_evidence_roots",
        "memory_entities",
        "memory_edges",
        "memory_chunks",
        "code_anchors",
        "contradiction_members",
    ] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let reference = if event == "DELETE" { "OLD" } else { "NEW" };
            let trigger = format!("access_mutation_{table}_{}", event.to_ascii_lowercase());
            let now = match db.backend {
                Backend::Sqlite => "CAST(strftime('%s','now') AS BIGINT)",
                Backend::Postgres => "CAST(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) AS BIGINT)",
            };
            let body=format!("INSERT INTO memory_mutations(memory_id,observed_since,mutation_count) SELECT memory_id,{now},1 FROM memory_meta WHERE memory_id={reference}.memory_id ON CONFLICT(memory_id) DO UPDATE SET mutation_count=memory_mutations.mutation_count+1; UPDATE generated_context_files SET dirty=dirty+1; UPDATE context_revision SET revision=revision+1 WHERE singleton=1;");
            let body = if table == "memory_meta" && event == "INSERT" {
                format!("INSERT INTO memory_access(memory_id,observed_since) VALUES(NEW.memory_id,{now}) ON CONFLICT(memory_id) DO NOTHING; {body}")
            } else {
                body
            };
            match db.backend {
                Backend::Sqlite => {
                    sqlx::query(&format!("CREATE TRIGGER IF NOT EXISTS {trigger} AFTER {event} ON {table} BEGIN {body} END")).execute(&db.pool).await?;
                }
                Backend::Postgres => {
                    sqlx::query(&format!("CREATE OR REPLACE FUNCTION {trigger}_fn() RETURNS TRIGGER LANGUAGE plpgsql AS $$ BEGIN {body} RETURN {reference}; END $$")).execute(&db.pool).await?;
                    sqlx::query(&format!("CREATE OR REPLACE TRIGGER {trigger} AFTER {event} ON {table} FOR EACH ROW EXECUTE FUNCTION {trigger}_fn()")).execute(&db.pool).await?;
                }
            }
        }
    }
    // Updating a contradiction changes eligibility of every member.
    let body="UPDATE memory_mutations SET mutation_count=mutation_count+1 WHERE memory_id IN (SELECT memory_id FROM contradiction_members WHERE contradiction_set_id=NEW.id); UPDATE generated_context_files SET dirty=dirty+1; UPDATE context_revision SET revision=revision+1 WHERE singleton=1;";
    match db.backend {
        Backend::Sqlite => {
            sqlx::query(&format!("CREATE TRIGGER IF NOT EXISTS access_contradiction_update AFTER UPDATE ON contradiction_sets BEGIN {body} END")).execute(&db.pool).await?;
        }
        Backend::Postgres => {
            sqlx::query(&format!("CREATE OR REPLACE FUNCTION access_contradiction_update_fn() RETURNS TRIGGER LANGUAGE plpgsql AS $$ BEGIN {body} RETURN NEW; END $$")).execute(&db.pool).await?;
            sqlx::query("CREATE OR REPLACE TRIGGER access_contradiction_update AFTER UPDATE ON contradiction_sets FOR EACH ROW EXECUTE FUNCTION access_contradiction_update_fn()").execute(&db.pool).await?;
        }
    }
    Ok(())
}
