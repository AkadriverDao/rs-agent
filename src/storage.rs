use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::types::*;

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub model: String,
    pub parent_id: Option<String>,
    pub message_count: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub created_at: String,
    pub updated_at: String,
}

pub struct Storage {
    conn: Mutex<Connection>,
}

impl Storage {
    pub fn new() -> Result<Self> {
        let path = Self::default_path()?;
        Self::open(&path)
    }

    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open SQLite database")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("Failed to set pragmas")?;
        let storage = Self {
            conn: Mutex::new(conn),
        };
        storage.initialize()?;
        Ok(storage)
    }

    fn default_path() -> Result<String> {
        let home = std::env::var("HOME").context("HOME not set")?;
        let dir = PathBuf::from(&home)
            .join(".local")
            .join("share")
            .join("agent-engine");
        std::fs::create_dir_all(&dir).context("Failed to create data directory")?;
        Ok(dir
            .join("agent-engine.db")
            .to_string_lossy()
            .to_string())
    }

    fn initialize(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        if version < 1 {
            conn.execute_batch("DROP TABLE IF EXISTS message_parts; DROP TABLE IF EXISTS tool_results; DROP TABLE IF EXISTS permission_rules;")?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT '',
                system_prompt TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                parent_id TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS message_parts (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                part_type TEXT NOT NULL,
                position INTEGER NOT NULL DEFAULT 0,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS tool_results (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                tool_call_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'success',
                result_value TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS permission_rules (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                tool_pattern TEXT NOT NULL,
                effect TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
            CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
            CREATE INDEX IF NOT EXISTS idx_message_parts_message_id ON message_parts(message_id);
            CREATE INDEX IF NOT EXISTS idx_tool_results_message_id ON tool_results(message_id);
            CREATE INDEX IF NOT EXISTS idx_tool_results_tool_name ON tool_results(tool_name);",
        )
        .context("Failed to initialize schema")?;

        if version < 1 {
            conn.pragma_update(None, "user_version", 1)?;
        }

        // migration: drop old data column if it exists (v0 → v1)
        if Self::has_column(&conn, "messages", "data") {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS messages_v1 (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
                );
                INSERT OR IGNORE INTO messages_v1 (id, session_id, role, created_at)
                    SELECT id, session_id, role, created_at FROM messages;
                DROP TABLE messages;
                ALTER TABLE messages_v1 RENAME TO messages;",
            )?;
        }
        Ok(())
    }

    fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
        let sql = format!("PRAGMA table_info({})", table);
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |row| {
                let name: String = row.get(1)?;
                Ok(name)
            }) {
                return rows.filter_map(|r| r.ok()).any(|n| n == column);
            }
        }
        false
    }

    // ── Session CRUD ──

    pub fn create_session(
        &self,
        title: &str,
        system_prompt: &str,
        model: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, title, system_prompt, model, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, title, system_prompt, model, now, now],
        )?;
        Ok(id)
    }

    pub fn create_child_session(
        &self,
        parent_id: &str,
        title: &str,
        system_prompt: &str,
        model: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, title, system_prompt, model, parent_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, title, system_prompt, model, parent_id, now, now],
        )?;
        Ok(id)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.model, s.parent_id, s.input_tokens, s.output_tokens,
                    s.created_at, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) as msg_count
             FROM sessions s
             ORDER BY s.updated_at DESC",
        )?;
        let sessions = stmt
            .query_map([], |row| {
                Ok(SessionInfo {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    model: row.get(2)?,
                    parent_id: row.get(3)?,
                    input_tokens: row.get::<_, i64>(4)? as u64,
                    output_tokens: row.get::<_, i64>(5)? as u64,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    message_count: row.get(8)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(sessions)
    }

    pub fn update_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    pub fn update_session_usage(
        &self,
        session_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET input_tokens = input_tokens + ?1, output_tokens = output_tokens + ?2, updated_at = ?3 WHERE id = ?4",
            rusqlite::params![input_tokens as i64, output_tokens as i64, chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM message_parts WHERE message_id IN (SELECT id FROM messages WHERE session_id = ?1)",
            rusqlite::params![session_id],
        )?;
        conn.execute(
            "DELETE FROM tool_results WHERE message_id IN (SELECT id FROM messages WHERE session_id = ?1)",
            rusqlite::params![session_id],
        )?;
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        conn.execute(
            "DELETE FROM permission_rules WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    // ── Message CRUD ──

    pub fn save_message(&self, session_id: &str, message: &Message) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = message.id().to_string();
        let role = message.role_str();
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO messages (id, session_id, role, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, session_id, role, now],
        )?;

        match message {
            Message::System { content, .. } => {
                self.insert_part(&conn, &id, "system", 0, &serde_json::json!({"content": content}), &now)?;
            }
            Message::User { content, .. } => {
                for (i, part) in content.iter().enumerate() {
                    let (part_type, part_data) = match part {
                        ContentPart::Text { text } => ("text", serde_json::json!({"text": text})),
                        ContentPart::Reasoning { text } => ("reasoning", serde_json::json!({"text": text})),
                        ContentPart::File { uri, mime } => ("file", serde_json::json!({"uri": uri, "mime": mime})),
                    };
                    self.insert_part(&conn, &id, part_type, i as i64, &part_data, &now)?;
                }
            }
            Message::Assistant {
                content, tool_calls, ..
            } => {
                for (i, part) in content.iter().enumerate() {
                    let (part_type, part_data) = match part {
                        ContentPart::Text { text } => ("text", serde_json::json!({"text": text})),
                        ContentPart::Reasoning { text } => ("reasoning", serde_json::json!({"text": text})),
                        ContentPart::File { uri, mime } => ("file", serde_json::json!({"uri": uri, "mime": mime})),
                    };
                    self.insert_part(&conn, &id, part_type, i as i64, &part_data, &now)?;
                }
                for (i, tc) in tool_calls.iter().enumerate() {
                    let offset = content.len() + i;
                    self.insert_part(
                        &conn,
                        &id,
                        "tool_call",
                        offset as i64,
                        &serde_json::json!({"id": tc.id, "name": tc.name, "input": tc.input}),
                        &now,
                    )?;
                }
            }
            Message::Tool {
                tool_call_id,
                tool_name,
                result,
                ..
            } => {
                let status = match result {
                    ToolResultValue::Error { .. } => "error",
                    _ => "success",
                };
                let result_json = serde_json::to_string(result)?;
                conn.execute(
                    "INSERT INTO tool_results (id, message_id, tool_call_id, tool_name, status, result_value, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        uuid::Uuid::new_v4().to_string(),
                        id,
                        tool_call_id,
                        tool_name,
                        status,
                        result_json,
                        now
                    ],
                )?;
                self.insert_part(
                    &conn,
                    &id,
                    "tool_result",
                    0,
                    &serde_json::json!({"tool_call_id": tool_call_id, "tool_name": tool_name, "result": result}),
                    &now,
                )?;
            }
        }

        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, session_id],
        )?;
        Ok(())
    }

    pub fn save_messages(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        for msg in messages {
            self.save_message(session_id, msg)?;
        }
        Ok(())
    }

    fn insert_part(
        &self,
        conn: &Connection,
        message_id: &str,
        part_type: &str,
        position: i64,
        data: &serde_json::Value,
        now: &str,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO message_parts (id, message_id, part_type, position, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                message_id,
                part_type,
                position,
                data.to_string(),
                now
            ],
        )?;
        Ok(())
    }

    pub fn load_session_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();

        let mut msg_stmt = conn.prepare(
            "SELECT id, role FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let mut part_stmt = conn.prepare(
            "SELECT part_type, position, data FROM message_parts WHERE message_id = ?1 ORDER BY position ASC",
        )?;
        let mut tool_stmt = conn.prepare(
            "SELECT tool_call_id, tool_name, status, result_value FROM tool_results WHERE message_id = ?1 ORDER BY created_at ASC",
        )?;

        let messages: Vec<Message> = msg_stmt
            .query_map(rusqlite::params![session_id], |row| {
                let id: String = row.get(0)?;
                let role: String = row.get(1)?;
                Ok((id, role))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(id, role)| {
                let parts: Vec<(String, i64, String)> = part_stmt
                    .query_map(rusqlite::params![&id], |row| {
                        let t: String = row.get(0)?;
                        let p: i64 = row.get(1)?;
                        let d: String = row.get(2)?;
                        Ok((t, p, d))
                    })
                    .ok()?
                    .filter_map(|r| r.ok())
                    .collect();

                let tool_results: Vec<(String, String, String, String)> = tool_stmt
                    .query_map(rusqlite::params![&id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })
                    .ok()?
                    .filter_map(|r| r.ok())
                    .collect();

                Self::reconstruct_message(&id, &role, &parts, &tool_results)
            })
            .collect();

        Ok(messages)
    }

    fn reconstruct_message(
        id: &str,
        role: &str,
        parts: &[(String, i64, String)],
        tool_results: &[(String, String, String, String)],
    ) -> Option<Message> {
        match role {
            "system" => {
                let content = parts.first()
                    .and_then(|(_, _, d)| serde_json::from_str::<serde_json::Value>(d).ok())
                    .and_then(|v| v["content"].as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                Some(Message::System {
                    id: id.to_string(),
                    content,
                })
            }
            "user" => {
                let content: Vec<ContentPart> = parts
                    .iter()
                    .filter_map(|(t, _, d)| {
                        let v: serde_json::Value = serde_json::from_str(d).ok()?;
                        match t.as_str() {
                            "text" => Some(ContentPart::Text { text: v["text"].as_str()?.to_string() }),
                            "reasoning" => Some(ContentPart::Reasoning { text: v["text"].as_str()?.to_string() }),
                            "file" => Some(ContentPart::File {
                                uri: v["uri"].as_str()?.to_string(),
                                mime: v["mime"].as_str()?.to_string(),
                            }),
                            _ => None,
                        }
                    })
                    .collect();
                Some(Message::User {
                    id: id.to_string(),
                    content,
                })
            }
            "assistant" => {
                let mut content = Vec::new();
                let mut tool_calls = Vec::new();
                for (t, _, d) in parts {
                    let v: serde_json::Value = serde_json::from_str(d).ok()?;
                    match t.as_str() {
                        "text" => content.push(ContentPart::Text { text: v["text"].as_str()?.to_string() }),
                        "reasoning" => content.push(ContentPart::Reasoning { text: v["text"].as_str()?.to_string() }),
                        "file" => content.push(ContentPart::File {
                            uri: v["uri"].as_str()?.to_string(),
                            mime: v["mime"].as_str()?.to_string(),
                        }),
                        "tool_call" => {
                            if let Some(tc) = ToolCall::from_json_value(&v) {
                                tool_calls.push(tc);
                            }
                        }
                        _ => {}
                    }
                }
                Some(Message::Assistant {
                    id: id.to_string(),
                    content,
                    tool_calls,
                })
            }
            "tool" => {
                if let Some((_, _, _, result_str)) = tool_results.first() {
                    let result: ToolResultValue = serde_json::from_str(result_str).ok()?;
                    Some(Message::Tool {
                        id: id.to_string(),
                        tool_call_id: tool_results.first()?.0.clone(),
                        tool_name: tool_results.first()?.1.clone(),
                        result,
                    })
                } else {
                    // fallback: reconstruct from part data
                    let (_, _, d) = parts.first()?;
                    let v: serde_json::Value = serde_json::from_str(d).ok()?;
                    let result: ToolResultValue = serde_json::from_value(v["result"].clone()).ok()?;
                    Some(Message::Tool {
                        id: id.to_string(),
                        tool_call_id: v["tool_call_id"].as_str()?.to_string(),
                        tool_name: v["tool_name"].as_str()?.to_string(),
                        result,
                    })
                }
            }
            _ => None,
        }
    }

    // ── Permission Rules ──

    pub fn save_permission_rule(
        &self,
        session_id: &str,
        tool_pattern: &str,
        effect: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO permission_rules (id, session_id, tool_pattern, effect, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, session_id, tool_pattern, effect, now],
        )?;
        Ok(id)
    }

    pub fn load_permission_rules(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT tool_pattern, effect FROM permission_rules WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rules = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rules)
    }

    // ── Tool Usage Stats ──

    pub fn get_tool_usage_stats(&self, session_id: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT tool_name, COUNT(*) as cnt FROM tool_results
             WHERE message_id IN (SELECT id FROM messages WHERE session_id = ?1)
             GROUP BY tool_name ORDER BY cnt DESC",
        )?;
        let stats = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(stats)
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::new().expect("Failed to initialize storage")
    }
}
