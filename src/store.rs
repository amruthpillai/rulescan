use crate::{
    model::*,
    paths::{self, Locations},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

pub struct Store {
    pub conn: Connection,
}
impl Store {
    pub fn open(locations: &Locations) -> Result<Self> {
        let _lock = locations.lock()?;
        paths::private_file(&locations.db(), false)?;
        let conn = Connection::open(locations.db())?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;",
        )?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version <= 1,
            "database is newer than this binary; do not downgrade"
        );
        if version == 0 {
            conn.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE artifacts(id TEXT PRIMARY KEY, root_id TEXT NOT NULL, data TEXT NOT NULL);
                CREATE TABLE approvals(artifact_id TEXT PRIMARY KEY, digest TEXT NOT NULL, snapshot BLOB, approved_at INTEGER NOT NULL, source TEXT NOT NULL);
                CREATE TABLE plans(id TEXT PRIMARY KEY, data TEXT NOT NULL);
                CREATE TABLE quarantine(id TEXT PRIMARY KEY, data TEXT NOT NULL);
                CREATE TABLE scans(id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, coverage TEXT NOT NULL);
                CREATE TABLE events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, kind TEXT NOT NULL, artifact_id TEXT, digest TEXT);
                CREATE TABLE settings(key TEXT PRIMARY KEY, data TEXT NOT NULL);
                PRAGMA user_version=1; COMMIT;")?;
        }
        Ok(Self { conn })
    }
    pub fn artifacts(&self) -> Result<Vec<Artifact>> {
        self.list("SELECT data FROM artifacts ORDER BY rowid")
    }
    pub fn artifact(&self, id: &str) -> Result<Artifact> {
        self.get("SELECT data FROM artifacts WHERE id=?1", id)
    }
    pub fn save_artifact(&self, a: &Artifact) -> Result<()> {
        self.conn.execute("INSERT INTO artifacts VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data", params![a.artifact_id, a.root_id, serde_json::to_string(a)?])?;
        Ok(())
    }
    pub fn approval(&self, id: &str) -> Result<Option<(String, Option<Vec<u8>>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT digest,snapshot FROM approvals WHERE artifact_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }
    pub fn approve(&self, artifact: &Artifact, bytes: Option<&[u8]>, source: &str) -> Result<()> {
        let digest = artifact
            .approval_digest
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no complete approval snapshot"))?;
        self.conn.execute("INSERT INTO approvals VALUES(?1,?2,?3,?4,?5) ON CONFLICT(artifact_id) DO UPDATE SET digest=excluded.digest,snapshot=excluded.snapshot,approved_at=excluded.approved_at,source=excluded.source", params![artifact.artifact_id, digest, bytes, now() as i64, source])?;
        // Keep at most 64 MiB of current baseline text, evicting only diff bytes.
        while self.conn.query_row(
            "SELECT COALESCE(sum(length(snapshot)),0) FROM approvals",
            [],
            |r| r.get::<_, i64>(0),
        )? > 64 * 1024 * 1024
        {
            self.conn.execute("UPDATE approvals SET snapshot=NULL WHERE artifact_id=(SELECT artifact_id FROM approvals WHERE snapshot IS NOT NULL ORDER BY approved_at LIMIT 1)", [])?;
        }
        Ok(())
    }
    pub fn plan(&self, id: &str) -> Result<ActionPlan> {
        self.get("SELECT data FROM plans WHERE id=?1", id)
    }
    pub fn save_plan(&self, plan: &ActionPlan) -> Result<()> {
        self.conn.execute(
            "INSERT INTO plans VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![plan.plan_id, serde_json::to_string(plan)?],
        )?;
        Ok(())
    }
    pub fn quarantine(&self, id: &str) -> Result<QuarantineRecord> {
        self.get("SELECT data FROM quarantine WHERE id=?1", id)
    }
    pub fn quarantines(&self) -> Result<Vec<QuarantineRecord>> {
        self.list("SELECT data FROM quarantine ORDER BY rowid")
    }
    pub fn save_quarantine(&self, q: &QuarantineRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO quarantine VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![q.quarantine_id, serde_json::to_string(q)?],
        )?;
        Ok(())
    }
    pub fn event(&self, kind: &str, artifact_id: Option<&str>, digest: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events(created_at,kind,artifact_id,digest) VALUES(?1,?2,?3,?4)",
            params![now() as i64, kind, artifact_id, digest],
        )?;
        self.conn.execute("DELETE FROM events WHERE sequence < (SELECT COALESCE(max(sequence),0)-10000 FROM events)", [])?;
        Ok(())
    }
    pub fn save_coverage(&self, coverage: &Coverage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scans(created_at,coverage) VALUES(?1,?2)",
            params![now() as i64, serde_json::to_string(coverage)?],
        )?;
        self.conn.execute(
            "DELETE FROM scans WHERE id < (SELECT COALESCE(max(id),0)-100 FROM scans)",
            [],
        )?;
        Ok(())
    }
    pub fn last_coverage(&self) -> Result<Option<Coverage>> {
        let data: Option<String> = self
            .conn
            .query_row(
                "SELECT coverage FROM scans ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }
    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let data: Option<String> = self
            .conn
            .query_row("SELECT data FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        data.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }
    pub fn save_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET data=excluded.data",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    fn get<T: DeserializeOwned>(&self, sql: &str, id: &str) -> Result<T> {
        let data: String = self
            .conn
            .query_row(sql, [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| InvocationError(format!("unknown id: {id}")))?;
        Ok(serde_json::from_str(&data)?)
    }
    fn list<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
}
