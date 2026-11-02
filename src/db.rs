use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde_json::{Map, Value};

use crate::context::retrieval::Chunk;
use crate::types::*;

/// Same schema (and file name) as the previous build, so existing history keeps working.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE sessions(id TEXT PRIMARY KEY, started_at INTEGER NOT NULL, ended_at INTEGER, status TEXT NOT NULL,
  profile_id TEXT, mode TEXT, title TEXT);
CREATE TABLE transcript_segments(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, source TEXT NOT NULL,
  start_ms INTEGER, end_ms INTEGER, text TEXT NOT NULL, lang TEXT);
CREATE INDEX ix_seg_session ON transcript_segments(session_id, start_ms);
CREATE TABLE questions(id TEXT NOT NULL, revision INTEGER NOT NULL, session_id TEXT NOT NULL, source TEXT, text TEXT,
  kind TEXT, status TEXT, trigger TEXT, start_ms INTEGER, end_ms INTEGER, supersedes TEXT, created_at INTEGER,
  PRIMARY KEY(id, revision));
CREATE INDEX ix_q_session ON questions(session_id);
CREATE TABLE answers(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, question_id TEXT, revision INTEGER, kind TEXT,
  provider TEXT, model TEXT, preset TEXT, text TEXT, status TEXT, first_token_ms INTEGER, latency_ms INTEGER,
  input_tokens INTEGER, output_tokens INTEGER, cost_usd REAL, created_at INTEGER);
CREATE INDEX ix_a_session ON answers(session_id);
CREATE INDEX ix_a_question ON answers(question_id);
CREATE TABLE profiles(id TEXT PRIMARY KEY, name TEXT NOT NULL, preset TEXT NOT NULL, style TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '');
CREATE TABLE knowledge_documents(id TEXT PRIMARY KEY, profile_id TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL, created_at INTEGER);
CREATE TABLE knowledge_chunks(id TEXT PRIMARY KEY, document_id TEXT NOT NULL, ord INTEGER, section TEXT, text TEXT NOT NULL,
  trust TEXT NOT NULL DEFAULT 'user_provided');
CREATE INDEX ix_chunk_doc ON knowledge_chunks(document_id);
CREATE TABLE usage_events(id TEXT PRIMARY KEY, session_id TEXT, provider TEXT, model TEXT, at INTEGER,
  input_tokens INTEGER, output_tokens INTEGER, cost_usd REAL);
CREATE INDEX ix_usage_at ON usage_events(at);
CREATE TABLE app_diagnostics(id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, kind TEXT, data TEXT);
"#, "ALTER TABLE sessions ADD COLUMN summary TEXT;"];

pub type Row = Map<String, Value>;

pub struct HistoryDetail {
    pub session: Option<Row>,
    pub segments: Vec<Row>,
    pub questions: Vec<Row>,
    pub answers: Vec<Row>,
}

pub struct Db {
    conn: Mutex<Connection>,
}

static DIAG: OnceLock<Arc<Db>> = OnceLock::new();

/// Diagnostics log (redacted) from anywhere; silently dropped before the DB is open.
pub fn diag(kind: &str, data: Value) {
    if let Some(db) = DIAG.get() {
        db.log_diag(kind, &crate::redact::redact_json(&data));
    }
}

fn value_of(r: &rusqlite::Row, i: usize) -> Value {
    use rusqlite::types::ValueRef;
    match r.get_ref(i) {
        Ok(ValueRef::Integer(n)) => Value::from(n),
        Ok(ValueRef::Real(f)) => Value::from(f),
        Ok(ValueRef::Text(t)) => Value::from(String::from_utf8_lossy(t).into_owned()),
        _ => Value::Null,
    }
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Arc<Db>> {
        let conn = Connection::open(path)?;
        let db = Arc::new(Self::init(conn)?);
        let _ = DIAG.set(db.clone());
        Ok(db)
    }

    #[cfg(test)]
    pub fn memory() -> Arc<Db> {
        Arc::new(Self::init(Connection::open_in_memory().unwrap()).unwrap())
    }

    fn init(conn: Connection) -> rusqlite::Result<Db> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL)")?;
        let mut v: i64 = conn.query_row("SELECT COALESCE(MAX(version),0) FROM schema_version", [], |r| r.get(0))?;
        while (v as usize) < MIGRATIONS.len() {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(MIGRATIONS[v as usize])?;
            tx.execute("INSERT INTO schema_version(version) VALUES (?)", [v + 1])?;
            tx.commit()?;
            v += 1;
        }
        Ok(Db { conn: Mutex::new(conn) })
    }

    fn rows(&self, sql: &str, args: &[&dyn ToSql]) -> Vec<Row> {
        let c = self.conn.lock().unwrap();
        let Ok(mut st) = c.prepare(sql) else { return Vec::new() };
        let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
        st.query_map(args, |r| Ok(names.iter().enumerate().map(|(i, n)| (n.clone(), value_of(r, i))).collect::<Row>()))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    fn exec(&self, sql: &str, args: &[&dyn ToSql]) {
        let c = self.conn.lock().unwrap();
        let _ = c.execute(sql, args);
    }

    /// I mark sessions left 'active' by a crash as 'interrupted'; their transcript is already on disk (WAL).
    pub fn recover_interrupted(&self) -> usize {
        let c = self.conn.lock().unwrap();
        c.execute("UPDATE sessions SET status='interrupted', ended_at=COALESCE(ended_at, ?) WHERE status='active'", [now_ms()]).unwrap_or(0)
    }

    pub fn start_session(&self, id: &str, profile_id: &str, mode: &str, title: &str) {
        self.exec(
            "INSERT INTO sessions(id, started_at, status, profile_id, mode, title) VALUES (?,?,'active',?,?,?)",
            &[&id, &now_ms(), &profile_id, &mode, &title],
        );
    }

    pub fn end_session(&self, id: &str) {
        self.exec("UPDATE sessions SET status='ended', ended_at=? WHERE id=?", &[&now_ms(), &id]);
    }

    pub fn add_segment(&self, s: &TranscriptSegment) {
        self.exec(
            "INSERT OR REPLACE INTO transcript_segments(id, session_id, source, start_ms, end_ms, text, lang) VALUES (?,?,?,?,?,?,?)",
            &[&s.segment_id, &s.session_id, &s.source.as_str(), &s.start_ms, &s.end_ms, &s.text, &s.lang],
        );
    }

    pub fn upsert_question(&self, q: &QuestionEvent) {
        self.exec(
            "INSERT OR REPLACE INTO questions(id, revision, session_id, source, text, kind, status, trigger, start_ms, end_ms, supersedes, created_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            &[
                &q.question_id,
                &q.revision,
                &q.session_id,
                &q.source.as_str(),
                &q.text,
                &q.kind.as_str(),
                &q.status.as_str(),
                &q.trigger.as_str(),
                &q.start_ms,
                &q.end_ms,
                &q.supersedes,
                &now_ms(),
            ],
        );
    }

    pub fn upsert_answer(&self, a: &AnswerEvent) {
        let (inp, out) = a.usage.map(|u| (Some(u.input_tokens as i64), Some(u.output_tokens as i64))).unwrap_or((None, None));
        self.exec(
            "INSERT OR REPLACE INTO answers(id, session_id, question_id, revision, kind, provider, model, preset, text, status,
               first_token_ms, latency_ms, input_tokens, output_tokens, cost_usd, created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            &[
                &a.answer_id,
                &a.session_id,
                &a.question_id,
                &a.revision,
                &a.kind.as_str(),
                &a.provider,
                &a.model,
                &a.prompt_preset,
                &a.text,
                &a.status.as_str(),
                &a.first_token_ms.map(|v| v as i64),
                &a.latency_ms.map(|v| v as i64),
                &inp,
                &out,
                &a.usage_cost_usd,
                &now_ms(),
            ],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_usage(&self, id: &str, session_id: Option<&str>, provider: &str, model: &str, input: u64, output: u64, cost: Option<f64>) {
        self.exec(
            "INSERT INTO usage_events(id, session_id, provider, model, at, input_tokens, output_tokens, cost_usd) VALUES (?,?,?,?,?,?,?,?)",
            &[&id, &session_id, &provider, &model, &now_ms(), &(input as i64), &(output as i64), &cost],
        );
    }

    pub fn spent_since(&self, ts: i64) -> f64 {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT COALESCE(SUM(cost_usd),0) FROM usage_events WHERE at >= ?", [ts], |r| r.get(0)).unwrap_or(0.0)
    }

    pub fn list_sessions(&self, query: &str, limit: i64) -> Vec<SessionSummary> {
        let q = query.trim();
        let rows = if q.is_empty() {
            self.rows(
                "SELECT s.*, (SELECT COUNT(DISTINCT id) FROM questions WHERE session_id=s.id) AS qn FROM sessions s ORDER BY started_at DESC LIMIT ?",
                &[&limit],
            )
        } else {
            let like = format!("%{q}%");
            self.rows(
                "SELECT s.*, (SELECT COUNT(DISTINCT id) FROM questions WHERE session_id=s.id) AS qn FROM sessions s
                 WHERE s.title LIKE ? OR s.summary LIKE ? OR EXISTS (SELECT 1 FROM transcript_segments t WHERE t.session_id=s.id AND t.text LIKE ?)
                 ORDER BY started_at DESC LIMIT ?",
                &[&like, &like, &like, &limit],
            )
        };
        rows.iter()
            .map(|r| SessionSummary {
                id: s(r, "id"),
                started_at: i(r, "started_at"),
                ended_at: r.get("ended_at").and_then(Value::as_i64),
                status: s(r, "status"),
                title: s(r, "title"),
                questions: i(r, "qn"),
            })
            .collect()
    }

    pub fn set_summary(&self, id: &str, text: &str) {
        self.exec("UPDATE sessions SET summary=? WHERE id=?", &[&text, &id]);
    }

    pub fn session_detail(&self, id: &str) -> HistoryDetail {
        HistoryDetail {
            session: self.rows("SELECT * FROM sessions WHERE id=?", &[&id]).into_iter().next(),
            segments: self.rows("SELECT * FROM transcript_segments WHERE session_id=? ORDER BY start_ms", &[&id]),
            questions: self.rows("SELECT * FROM questions WHERE session_id=? ORDER BY created_at, revision", &[&id]),
            answers: self.rows("SELECT * FROM answers WHERE session_id=? AND status IN ('done','error','cancelled') ORDER BY created_at", &[&id]),
        }
    }

    /// The most recent finished session that has any transcript — the one "Continue previous session" picks up.
    pub fn last_resumable(&self) -> Option<ResumableSession> {
        let r = self
            .rows(
                "SELECT s.*, (SELECT COUNT(*) FROM transcript_segments t WHERE t.session_id=s.id) AS segs
                 FROM sessions s WHERE s.status!='active' ORDER BY COALESCE(s.ended_at, s.started_at) DESC LIMIT 1",
                &[],
            )
            .into_iter()
            .next()?;
        if i(&r, "segs") == 0 {
            return None;
        }
        Some(ResumableSession {
            id: s(&r, "id"),
            title: s(&r, "title"),
            started_at: i(&r, "started_at"),
            ended_at: r.get("ended_at").and_then(Value::as_i64),
            mode: SessionMode::parse(&s(&r, "mode")),
            interrupted: s(&r, "status") == "interrupted",
            segments: i(&r, "segs"),
        })
    }

    pub fn session_segments(&self, id: &str) -> Vec<TranscriptSegment> {
        self.rows("SELECT * FROM transcript_segments WHERE session_id=? ORDER BY start_ms", &[&id])
            .iter()
            .map(|r| TranscriptSegment {
                session_id: s(r, "session_id"),
                segment_id: s(r, "id"),
                source: Source::parse(&s(r, "source")),
                start_ms: i(r, "start_ms"),
                end_ms: i(r, "end_ms"),
                text: s(r, "text"),
                is_final: true,
                lang: r.get("lang").and_then(Value::as_str).map(String::from),
            })
            .collect()
    }

    pub fn delete_session(&self, id: &str) {
        let c = self.conn.lock().unwrap();
        if let Ok(tx) = c.unchecked_transaction() {
            for t in ["transcript_segments", "questions", "answers", "usage_events"] {
                let _ = tx.execute(&format!("DELETE FROM {t} WHERE session_id=?"), [id]);
            }
            let _ = tx.execute("DELETE FROM sessions WHERE id=?", [id]);
            let _ = tx.commit();
        }
        // secure_delete zeroes freed pages in the main file, but the WAL still holds the old page images until checkpointed.
        let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
    }

    pub fn delete_all_history(&self) {
        let c = self.conn.lock().unwrap();
        if let Ok(tx) = c.unchecked_transaction() {
            for t in ["transcript_segments", "questions", "answers", "usage_events", "sessions", "app_diagnostics"] {
                let _ = tx.execute(&format!("DELETE FROM {t}"), []);
            }
            let _ = tx.commit();
        }
        let _ = c.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);");
    }

    pub fn list_profiles(&self) -> Vec<Profile> {
        self.rows("SELECT * FROM profiles ORDER BY name", &[])
            .iter()
            .map(|r| Profile {
                id: s(r, "id"),
                name: s(r, "name"),
                preset: ProfilePreset::parse(&s(r, "preset")),
                style: AnswerStyle::parse(&s(r, "style")),
                notes: s(r, "notes"),
            })
            .collect()
    }

    pub fn upsert_profile(&self, p: &Profile) {
        self.exec(
            "INSERT OR REPLACE INTO profiles(id, name, preset, style, notes) VALUES (?,?,?,?,?)",
            &[&p.id, &p.name, &p.preset.as_str(), &p.style.as_str(), &p.notes],
        );
    }

    pub fn delete_profile(&self, id: &str) {
        let docs = self.list_docs(id);
        let c = self.conn.lock().unwrap();
        if let Ok(tx) = c.unchecked_transaction() {
            for d in docs {
                let _ = tx.execute("DELETE FROM knowledge_chunks WHERE document_id=?", [&d.id]);
                let _ = tx.execute("DELETE FROM knowledge_documents WHERE id=?", [&d.id]);
            }
            let _ = tx.execute("DELETE FROM profiles WHERE id=?", [id]);
            let _ = tx.commit();
        };
    }

    pub fn add_document(&self, id: &str, profile_id: &str, name: &str, kind: DocKind, chunks: &[(String, String)]) {
        let c = self.conn.lock().unwrap();
        if let Ok(tx) = c.unchecked_transaction() {
            let _ = tx.execute(
                "INSERT INTO knowledge_documents(id, profile_id, name, kind, created_at) VALUES (?,?,?,?,?)",
                params![id, profile_id, name, kind.as_str(), now_ms()],
            );
            for (n, (section, text)) in chunks.iter().enumerate() {
                let _ = tx.execute(
                    "INSERT INTO knowledge_chunks(id, document_id, ord, section, text, trust) VALUES (?,?,?,?,?,'user_provided')",
                    params![new_id(), id, n as i64, section, text],
                );
            }
            let _ = tx.commit();
        };
    }

    pub fn list_docs(&self, profile_id: &str) -> Vec<KnowledgeDoc> {
        self.rows(
            "SELECT d.*, (SELECT COUNT(*) FROM knowledge_chunks c WHERE c.document_id=d.id) AS n FROM knowledge_documents d WHERE profile_id=? ORDER BY created_at",
            &[&profile_id],
        )
        .iter()
        .map(|r| KnowledgeDoc {
            id: s(r, "id"),
            profile_id: s(r, "profile_id"),
            name: s(r, "name"),
            kind: DocKind::parse(&s(r, "kind")),
            chunks: i(r, "n"),
            created_at: i(r, "created_at"),
        })
        .collect()
    }

    pub fn chunks_for_profile(&self, profile_id: &str) -> Vec<Chunk> {
        self.rows(
            "SELECT c.*, d.name AS doc_name, d.kind AS doc_kind FROM knowledge_chunks c JOIN knowledge_documents d ON d.id=c.document_id
             WHERE d.profile_id=? ORDER BY d.created_at, c.ord",
            &[&profile_id],
        )
        .iter()
        .map(|r| Chunk {
            id: s(r, "id"),
            document_id: s(r, "document_id"),
            doc_name: s(r, "doc_name"),
            kind: DocKind::parse(&s(r, "doc_kind")),
            section: s(r, "section"),
            text: s(r, "text"),
        })
        .collect()
    }

    pub fn delete_doc(&self, id: &str) {
        let c = self.conn.lock().unwrap();
        if let Ok(tx) = c.unchecked_transaction() {
            let _ = tx.execute("DELETE FROM knowledge_chunks WHERE document_id=?", [id]);
            let _ = tx.execute("DELETE FROM knowledge_documents WHERE id=?", [id]);
            let _ = tx.commit();
        };
    }

    pub fn log_diag(&self, kind: &str, data: &Value) {
        let c = self.conn.lock().unwrap();
        let _ = c.execute("INSERT INTO app_diagnostics(at, kind, data) VALUES (?,?,?)", params![now_ms(), kind, data.to_string()]);
        let _ = c.execute("DELETE FROM app_diagnostics WHERE id <= (SELECT MAX(id) - 2000 FROM app_diagnostics)", []);
    }

    pub fn recent_diag(&self, limit: i64) -> Vec<(i64, String, String)> {
        self.rows("SELECT at, kind, data FROM app_diagnostics ORDER BY id DESC LIMIT ?", &[&limit])
            .iter()
            .map(|r| (i(r, "at"), s(r, "kind"), s(r, "data")))
            .collect()
    }

    pub fn quick_check(&self) -> String {
        let c = self.conn.lock().unwrap();
        c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).optional().ok().flatten().unwrap_or_else(|| "?".into())
    }
}

pub fn s(r: &Row, k: &str) -> String {
    match r.get(k) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

pub fn i(r: &Row, k: &str) -> i64 {
    r.get(k).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0)
}
