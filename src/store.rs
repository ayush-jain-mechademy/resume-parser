//! Persistent resume store (SQLite): a file-hash cache so unchanged resumes are
//! never re-processed, an audit trail of every extracted record, and the
//! identity key used to dedupe the same candidate across files. Human
//! corrections from the review app are written back here.

use crate::schema::CandidateRecord;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("opening store db")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS candidates (
                hash     TEXT PRIMARY KEY,
                identity TEXT NOT NULL,
                verified INTEGER NOT NULL DEFAULT 0,
                json     TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_identity ON candidates(identity);",
        )?;
        Ok(Store { conn })
    }

    /// Identity used for dedupe: email, else phone, else the file path.
    pub fn identity(rec: &CandidateRecord) -> String {
        if !rec.email.is_empty() {
            rec.email.to_ascii_lowercase()
        } else if !rec.whatsapp.is_empty() {
            rec.whatsapp.clone()
        } else {
            rec.source_file.clone()
        }
    }

    pub fn contains_hash(&self, hash: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM candidates WHERE hash = ?1",
            params![hash],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn get(&self, hash: &str) -> Result<Option<CandidateRecord>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM candidates WHERE hash = ?1",
                params![hash],
                |r| r.get(0),
            )
            .optional()?;
        match json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    pub fn upsert(&self, rec: &CandidateRecord) -> Result<()> {
        // Idempotency + safety: a fresh (unverified) re-extraction must never
        // clobber a human-verified record for the same file. The human wins.
        if !rec.human_verified {
            let existing_verified: Option<i64> = self
                .conn
                .query_row(
                    "SELECT verified FROM candidates WHERE hash = ?1",
                    params![rec.file_hash],
                    |r| r.get(0),
                )
                .optional()?;
            if existing_verified == Some(1) {
                return Ok(());
            }
        }
        let json = serde_json::to_string(rec)?;
        self.conn.execute(
            "INSERT INTO candidates (hash, identity, verified, json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(hash) DO UPDATE SET
                identity = excluded.identity,
                verified = excluded.verified,
                json     = excluded.json",
            params![rec.file_hash, Self::identity(rec), rec.human_verified as i64, json],
        )?;
        Ok(())
    }

    /// All stored records, newest-writes-last ordering not guaranteed.
    pub fn all(&self) -> Result<Vec<CandidateRecord>> {
        let mut stmt = self.conn.prepare("SELECT json FROM candidates")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }
}
