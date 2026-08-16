use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

use crate::import::ImportResult;
use crate::ir::{EventKind, Trace};

/// Inject `trace` into an OpenCode SQLite store as a fresh synthetic session.
/// Returns the created session id and the anchor (last injected message id).
pub fn import(trace: &Trace, db_path: &Path) -> Result<ImportResult, String> {
    import_existing(trace, db_path, None, None, false, None)
}

pub fn import_existing(
    trace: &Trace,
    db_path: &Path,
    existing_session_id: Option<&str>,
    existing_anchor: Option<&str>,
    force: bool,
    model_override: Option<&str>,
) -> Result<ImportResult, String> {
    let mut conn =
        Connection::open(db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    let now = chrono::Utc::now().timestamp_millis();
    let project_id = resolve_project(&tx, trace.meta.cwd.as_deref().unwrap_or("/"), now)?;
    let session_id = match existing_session_id {
        Some(id) => {
            let exists = tx
                .query_row("SELECT 1 FROM session WHERE id = ?1", [id], |_| Ok(()))
                .is_ok();
            if !exists && !force {
                return Err(format!(
                    "target OpenCode session is missing: {id} (use --force to recreate it)"
                ));
            }
            if exists {
                if let Some(anchor) = existing_anchor
                    && !force
                    && opencode_has_messages_after_anchor(&tx, id, anchor)?
                {
                    return Err(format!(
                        "target OpenCode session continued after anchor; refusing to overwrite {id} (use --force to replace it)"
                    ));
                }
                tx.execute("DELETE FROM part WHERE session_id = ?1", [id])
                    .map_err(|e| format!("delete old parts: {e}"))?;
                tx.execute("DELETE FROM message WHERE session_id = ?1", [id])
                    .map_err(|e| format!("delete old messages: {e}"))?;
            }
            id.to_string()
        }
        None => opencode_unique_id(&tx, "session", "ses", IDDirection::Descending)?,
    };
    let slug = format!("imported-{}", &session_id[4..10]);
    let version: String = tx
        .query_row(
            "SELECT version FROM session ORDER BY time_updated DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "1.0.0".to_string());

    let title = trace
        .meta
        .title
        .clone()
        .or_else(|| {
            Some(format!(
                "imported from {} · {}",
                trace.meta.source, trace.meta.session_id
            ))
        })
        .unwrap_or_default();

    tx.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated, agent, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
           project_id = excluded.project_id,
           slug = excluded.slug,
           directory = excluded.directory,
           title = excluded.title,
           version = excluded.version,
           time_updated = excluded.time_updated,
           agent = excluded.agent,
           model = excluded.model",
        params![
            &session_id,
            &project_id,
            &slug,
            trace.meta.cwd.clone().unwrap_or_default(),
            &title,
            &version,
            now,
            now,
            "cash",
            serde_json::to_string(&json!({
                "id": model_override
                    .map(str::to_owned)
                    .or_else(|| trace.meta.model.clone())
                    .unwrap_or_else(|| "cash".into()),
                "providerID": "cash",
                "variant": "default"
            })).unwrap_or_default(),
        ],
    )
    .map_err(|e| format!("insert session: {e}"))?;

    let mut anchor: Option<String> = None;
    let mut message_count = 0usize;
    let mut dropped_event_count = 0usize;

    // Events are the single representation. Consecutive events sharing an
    // original_id (a message) are grouped into one message row with its parts.
    let mut i = 0usize;
    let mut pending_tool: Option<(String, String, String)> = None;
    while i < trace.events.len() {
        let oid = trace.events[i].original_id.clone();
        let mut j = i;
        while j < trace.events.len() && trace.events[j].original_id == oid {
            j += 1;
        }
        let group = &trace.events[i..j];

        // A group consisting only of model_change (no OpenCode representation)
        // or native passthrough records is skipped entirely.
        let materializable = group.iter().any(|e| {
            !matches!(
                e.kind,
                EventKind::ModelChange { .. } | EventKind::NativeRecord { .. }
            )
        });
        if !materializable {
            dropped_event_count += group.len();
            i = j;
            continue;
        }

        let role = if group
            .iter()
            .any(|e| matches!(e.kind, EventKind::UserMessage { .. }))
        {
            "user"
        } else {
            "assistant"
        };
        let data = group
            .iter()
            .find_map(|e| e.native.clone())
            .filter(|n| n.get("role").is_some())
            .unwrap_or_else(|| json!({ "role": role }));
        let msg_id = opencode_unique_id(&tx, "message", "msg", IDDirection::Ascending)?;
        let event_time = now + message_count as i64;
        tx.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?3, ?4)",
            params![
                &msg_id,
                &session_id,
                event_time,
                serde_json::to_string(&data).unwrap_or_default(),
            ],
        )
        .map_err(|e| format!("insert message: {e}"))?;

        let mut tool_call: Option<(String, String, String)> = None;
        let mut tool_result: Option<(String, String, Option<i32>, Option<String>)> = None;
        for ev in group {
            match &ev.kind {
                EventKind::UserMessage { text } | EventKind::AssistantMessage { text } => {
                    insert_part(
                        &tx,
                        &session_id,
                        &msg_id,
                        json!({"type": "text", "text": text}),
                        event_time,
                    )?;
                }
                EventKind::Reasoning { text } => {
                    insert_part(
                        &tx,
                        &session_id,
                        &msg_id,
                        json!({"type": "reasoning", "text": text}),
                        event_time,
                    )?;
                }
                EventKind::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => {
                    tool_call = Some((id.clone(), name.clone(), arguments.clone()));
                }
                EventKind::ToolResult {
                    call_id,
                    output,
                    exit_code,
                    error,
                } => {
                    tool_result =
                        Some((call_id.clone(), output.clone(), *exit_code, error.clone()));
                }
                EventKind::ModelChange { .. } => {
                    dropped_event_count += 1;
                }
                EventKind::NativeRecord { .. } => {}
            }
        }
        // OpenCode stores call+result in a single tool part. A call and its
        // result may come from different native records (e.g. Pi), so buffer
        // the call across groups.
        if tool_call.is_some() {
            pending_tool = tool_call.clone();
        }
        if let Some((call_id, output, exit, error)) = tool_result {
            let (name, args) = match (tool_call, &pending_tool) {
                (Some((_, name, args)), _) => (name, args.clone()),
                (None, Some((_, name, args))) => (name.clone(), args.clone()),
                _ => ("tool".to_string(), "{}".to_string()),
            };
            let args_val: Value = serde_json::from_str(&args).unwrap_or(Value::String(args));
            let part = json!({
                "type": "tool",
                "tool": name,
                "callID": call_id,
                "state": {
                    "status": if error.is_some() { "error" } else { "completed" },
                    "input": args_val,
                    "output": output,
                    "metadata": {
                        "exit": exit,
                        "truncated": false
                    },
                    "time": { "start": event_time, "end": event_time }
                }
            });
            insert_part(&tx, &session_id, &msg_id, part, event_time)?;
            pending_tool = None;
        }

        anchor = Some(msg_id);
        message_count += 1;
        i = j;
    }
    let _ = dropped_event_count;

    tx.commit().map_err(|e| format!("commit: {e}"))?;

    let anchor_message_id = anchor.unwrap_or_default();
    Ok(ImportResult {
        session_id,
        file: db_path.to_string_lossy().into_owned(),
        anchor_message_id,
        message_count,
        dropped_event_count,
    })
}

fn opencode_has_messages_after_anchor(
    conn: &Connection,
    session_id: &str,
    anchor: &str,
) -> Result<bool, String> {
    let anchor_order: Option<(i64, String)> = conn
        .query_row(
            "SELECT time_created, id FROM message WHERE session_id = ?1 AND id = ?2",
            params![session_id, anchor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((created, id)) = anchor_order else {
        return Ok(true);
    };
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message
             WHERE session_id = ?1 AND (time_created > ?2 OR (time_created = ?2 AND id > ?3))",
            params![session_id, created, id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count > 0)
}

fn resolve_project(tx: &Connection, cwd: &str, now: i64) -> Result<String, String> {
    let found: Option<String> = tx
        .query_row(
            "SELECT id FROM project WHERE worktree = ?1 LIMIT 1",
            [cwd],
            |r| r.get(0),
        )
        .ok();
    if let Some(id) = found {
        return Ok(id);
    }
    let global: Option<String> = tx
        .query_row(
            "SELECT id FROM project WHERE id = 'global' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if let Some(id) = global {
        return Ok(id);
    }
    let id = Uuid::new_v4().simple().to_string();
    tx.execute(
        "INSERT INTO project (id, worktree, name, time_created, time_updated, sandboxes)
         VALUES (?1, ?2, ?3, ?4, ?5, '[]')",
        params![
            &id,
            cwd,
            cwd.rsplit('/').next().unwrap_or("project"),
            now,
            now
        ],
    )
    .map_err(|e| format!("insert project: {e}"))?;
    Ok(id)
}

fn insert_part(
    tx: &Connection,
    session_id: &str,
    message_id: &str,
    data: Value,
    now: i64,
) -> Result<(), String> {
    let pid = opencode_unique_id(tx, "part", "prt", IDDirection::Ascending)?;
    tx.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        params![&pid, message_id, session_id, now, serde_json::to_string(&data).unwrap_or_default()],
    )
    .map_err(|e| format!("insert part: {e}"))?;
    Ok(())
}

fn opencode_unique_id(
    conn: &Connection,
    table: &str,
    prefix: &str,
    direction: IDDirection,
) -> Result<String, String> {
    for _ in 0..100 {
        let id = opencode_id(prefix, direction);
        let exists = match table {
            "session" => conn
                .query_row("SELECT 1 FROM session WHERE id = ?1", [&id], |_| Ok(()))
                .is_ok(),
            "message" => conn
                .query_row("SELECT 1 FROM message WHERE id = ?1", [&id], |_| Ok(()))
                .is_ok(),
            "part" => conn
                .query_row("SELECT 1 FROM part WHERE id = ?1", [&id], |_| Ok(()))
                .is_ok(),
            _ => return Err(format!("unsupported id table: {table}")),
        };
        if !exists {
            return Ok(id);
        }
    }
    Err(format!(
        "could not generate unique {prefix} id after 100 attempts"
    ))
}

#[derive(Clone, Copy)]
enum IDDirection {
    Ascending,
    Descending,
}

static LAST_TIMESTAMP: AtomicU64 = AtomicU64::new(0);
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn opencode_id(prefix: &str, direction: IDDirection) -> String {
    let timestamp = chrono::Utc::now().timestamp_millis() as u64;
    let last = LAST_TIMESTAMP.swap(timestamp, Ordering::SeqCst);
    let counter = if last == timestamp {
        COUNTER.fetch_add(1, Ordering::SeqCst) + 1
    } else {
        COUNTER.store(1, Ordering::SeqCst);
        1
    };

    let mut encoded = timestamp.saturating_mul(0x1000).saturating_add(counter);
    if matches!(direction, IDDirection::Descending) {
        encoded = !encoded;
    }

    let mut time_hex = String::with_capacity(12);
    for i in 0..6 {
        let byte = ((encoded >> (40 - 8 * i)) & 0xff) as u8;
        time_hex.push_str(&format!("{byte:02x}"));
    }
    format!("{prefix}_{time_hex}{}", random_base62(14))
}

fn random_base62(len: usize) -> String {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes).expect("OS randomness unavailable");
    bytes
        .iter()
        .map(|b| CHARS[(*b as usize) % CHARS.len()] as char)
        .collect()
}
