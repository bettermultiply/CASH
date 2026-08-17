use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;

use crate::ir::{AgentKind, Event, EventKind, Trace, TraceMeta};
use crate::util::sha256_hex;

type SessionRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);
pub type ListRow = (String, String, Option<String>, Option<i64>);

/// Parse a session from the OpenCode SQLite store (default ~/.local/share/opencode/opencode.db).
pub fn read(db_path: &Path, session_id: &str) -> Result<Trace, String> {
    let conn = Connection::open(db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?;

    let sess: Option<SessionRow> = conn
        .query_row(
            "SELECT id, directory, title, model, time_created FROM session WHERE id = ?1",
            [session_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .ok();

    let (sess_id, directory, title, model, started_at) = match sess {
        Some(s) => s,
        None => {
            return Err(format!(
                "session {session_id} not found in {}",
                db_path.display()
            ));
        }
    };

    let mut raw = String::new();
    for row in conn
        .prepare("SELECT data FROM message WHERE session_id = ?1 ORDER BY time_created, id")
        .map_err(|e| e.to_string())?
        .query_map([session_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
    {
        raw.push_str(&row.map_err(|e| e.to_string())?);
        raw.push('\n');
    }
    let file_hash = sha256_hex(&raw);

    let mut events: Vec<Event> = Vec::new();
    let mut prev_msg_id: Option<String> = None;

    for msg in conn
        .prepare("SELECT id, data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created, id")
        .map_err(|e| e.to_string())?
        .query_map([session_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })
        .map_err(|e| e.to_string())?
    {
        let (msg_id, msg_data, _msg_time) = msg.map_err(|e| e.to_string())?;
        let mv: Value = serde_json::from_str(&msg_data).map_err(|e| format!("bad message json: {e}"))?;
        let role = mv.get("role").and_then(Value::as_str).unwrap_or("");
        let parent = prev_msg_id.clone();

        let part_rows: Vec<(String, String)> = conn
            .prepare("SELECT id, data FROM part WHERE message_id = ?1 ORDER BY time_created, id")
            .map_err(|e| e.to_string())?
            .query_map([&msg_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;

        let mut parts: Vec<(String, i64, Value)> = Vec::new();
        for (pid, pdata) in part_rows {
            let parsed: Value =
                serde_json::from_str(&pdata).map_err(|e| format!("bad part json: {e}"))?;
            let ptime = parsed
                .get("time")
                .and_then(Value::as_object)
                .and_then(|t| t.get("end"))
                .and_then(Value::as_i64)
                .unwrap_or(_msg_time);
            parts.push((pid, ptime, parsed));
        }

        for (_pid, part_time, pv) in parts {
            let ptype = pv.get("type").and_then(Value::as_str).unwrap_or("");
            let time = Some(part_time);
            // All parts of a message share the message id as original_id.
            match ptype {
                "text" => {
                    let text = pv.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                    if !text.is_empty() {
                        let kind = match role {
                            "user" => EventKind::UserMessage { text },
                            _ => EventKind::AssistantMessage { text },
                        };
                        events.push(Event {
                            original_id: msg_id.clone(),
                            parent_original_id: parent.clone(),
                            time,
                            native: Some(mv.clone()),
                            kind,
                        });
                    }
                }
                "reasoning" => {
                    let text = pv.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                    if !text.is_empty() {
                        events.push(Event {
                            original_id: msg_id.clone(),
                            parent_original_id: parent.clone(),
                            time,
                            native: Some(mv.clone()),
                            kind: EventKind::Reasoning { text },
                        });
                    }
                }
                "tool" => {
                    let name = pv.get("tool").and_then(Value::as_str).unwrap_or_default().to_string();
                    let call_id = pv.get("callID").and_then(Value::as_str).unwrap_or_default().to_string();
                    let state = &pv["state"];
                    let input = state.get("input").cloned().unwrap_or(Value::Null);
                    let output = state
                        .get("output")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let exit = state
                        .get("metadata")
                        .and_then(|m| m.get("exit"))
                        .and_then(Value::as_i64)
                        .map(|x| x as i32);
                    let status = state.get("status").and_then(Value::as_str).unwrap_or("");
                    let arguments = serde_json::to_string(&input).unwrap_or_default();
                    let is_error = status == "error" && output.is_empty();

                    events.push(Event {
                        original_id: msg_id.clone(),
                        parent_original_id: parent.clone(),
                        time,
                        native: Some(mv.clone()),
                        kind: EventKind::ToolCall {
                            id: call_id.clone(),
                            name,
                            arguments,
                        },
                    });
                    events.push(Event {
                        original_id: msg_id.clone(),
                        parent_original_id: parent.clone(),
                        time,
                        native: Some(mv.clone()),
                        kind: EventKind::ToolResult {
                            call_id,
                            output,
                            exit_code: exit,
                            error: is_error.then(|| "tool reported error".to_string()),
                        },
                    });
                }
                // step-start / step-finish are UI markers; not part of the trace.
                _ => {}
            }
        }
        prev_msg_id = Some(msg_id);
    }

    let meta = TraceMeta {
        source: AgentKind::OpenCode,
        session_id: sess_id,
        file: format!("{}({session_id})", db_path.display()),
        cwd: directory,
        title,
        model,
        started_at,
        ended_at: None,
        source_file_sha256: file_hash,
        events_sha256: String::new(),
        event_count: 0,
    };
    let mut meta = meta;
    crate::util::finish_meta(&mut meta, &events);
    Ok(Trace { meta, events })
}

/// List available sessions from the OpenCode SQLite store.
pub fn list_session_summaries(db_path: &Path) -> Result<Vec<super::SessionSummary>, String> {
    let conn = Connection::open(db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, directory, title, time_updated FROM session ORDER BY time_updated DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(super::SessionSummary {
                session_id: row.get(0)?,
                cwd: row.get(1)?,
                title: row.get(2)?,
                time: row.get(3)?,
                time_kind: super::SessionTimeKind::Updated,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut summaries = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    super::sort_session_summaries(&mut summaries);
    Ok(summaries)
}

/// Compatibility view used by callers that still consume the old tuple shape.
pub fn list_sessions(db_path: &Path) -> Result<Vec<ListRow>, String> {
    Ok(list_session_summaries(db_path)?
        .into_iter()
        .map(|summary| {
            (
                summary.session_id,
                summary.cwd.unwrap_or_default(),
                summary.title,
                summary.time,
            )
        })
        .collect())
}
