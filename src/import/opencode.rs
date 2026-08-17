use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

use crate::import::ImportResult;
use crate::ir::{AgentKind, Event, EventKind, Trace};

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

    let v2 = has_table(&tx, "session_message") && has_table(&tx, "event");

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
                if v2 {
                    tx.execute("DELETE FROM event WHERE aggregate_id = ?1", [id])
                        .map_err(|e| format!("delete old events: {e}"))?;
                    tx.execute("DELETE FROM event_sequence WHERE aggregate_id = ?1", [id])
                        .map_err(|e| format!("delete old event sequence: {e}"))?;
                    tx.execute("DELETE FROM session_message WHERE session_id = ?1", [id])
                        .map_err(|e| format!("delete old projected messages: {e}"))?;
                    tx.execute("DELETE FROM session_input WHERE session_id = ?1", [id])
                        .map_err(|e| format!("delete old session inputs: {e}"))?;
                }
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

    let target_agent = opencode_agent(trace);
    let target_model = opencode_model(&tx, trace, model_override);
    let (model_id, provider_id, model_variant) = parse_model(&target_model);
    let model_ref = model_ref_json(&model_id, &provider_id, model_variant.as_deref());
    let cwd = trace.meta.cwd.clone().unwrap_or_default();
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
            cwd,
            &title,
            &version,
            now,
            now,
            &target_agent,
            &target_model,
        ],
    )
    .map_err(|e| format!("insert session: {e}"))?;

    let mut anchor: Option<String> = None;
    let mut message_count = 0usize;
    let mut dropped_event_count = 0usize;
    let mut event_seq = current_event_seq(&tx, &session_id);

    if v2 {
        // `event` rows reference `event_sequence` via a foreign key, so the
        // aggregate row must exist before the first event is inserted.
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2)
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
            params![&session_id, event_seq],
        )
        .map_err(|e| format!("update event sequence: {e}"))?;
    }

    // Events are the single representation. Consecutive events sharing an
    // original_id (a message) are grouped into one message row with its parts.
    let mut i = 0usize;
    let mut pending_tool: Option<(String, String, String)> = None;
    let mut previous_message_id: Option<String> = None;
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
        let msg_id = opencode_unique_id(&tx, "message", "msg", IDDirection::Ascending)?;
        let event_time = now + message_count as i64;
        emit_group(
            &tx,
            &session_id,
            group,
            role,
            &msg_id,
            previous_message_id.as_deref(),
            &target_agent,
            &model_id,
            &provider_id,
            model_variant.as_deref(),
            &model_ref,
            &cwd,
            event_time,
            &mut event_seq,
            &mut pending_tool,
            v2,
        )?;

        previous_message_id = Some(msg_id.clone());
        anchor = Some(msg_id);
        message_count += 1;
        i = j;
    }
    let _ = dropped_event_count;

    if v2 {
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2)
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
            params![&session_id, event_seq],
        )
        .map_err(|e| format!("update event sequence: {e}"))?;
    }

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

/// Append events to an existing OpenCode session without replacing its history.
pub fn append_existing(
    trace: &Trace,
    db_path: &Path,
    session_id: &str,
) -> Result<ImportResult, String> {
    let mut conn =
        Connection::open(db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let exists = tx
        .query_row("SELECT 1 FROM session WHERE id = ?1", [session_id], |_| {
            Ok(())
        })
        .is_ok();
    if !exists {
        return Err(format!("OpenCode session is missing: {session_id}"));
    }

    let v2 = has_table(&tx, "session_message") && has_table(&tx, "event");
    let now = chrono::Utc::now().timestamp_millis();
    let cwd: String = tx
        .query_row(
            "SELECT directory FROM session WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap_or_default();
    let agent: String = tx
        .query_row(
            "SELECT agent FROM session WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "build".to_string());
    let model_json: String = tx
        .query_row(
            "SELECT model FROM session WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap_or_default();
    let (model_id, provider_id, model_variant) = parse_model(&model_json);
    let model_ref = model_ref_json(&model_id, &provider_id, model_variant.as_deref());

    let mut anchor: Option<String> = None;
    let mut message_count = 0usize;
    let mut dropped_event_count = 0usize;
    let mut event_seq = current_event_seq(&tx, session_id);

    if v2 {
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2)
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
            params![session_id, event_seq],
        )
        .map_err(|e| format!("update event sequence: {e}"))?;
    }
    let mut pending_tool: Option<(String, String, String)> = None;
    let mut previous_message_id: Option<String> = tx
        .query_row(
            "SELECT id FROM message WHERE session_id = ?1 ORDER BY time_created DESC, id DESC LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .ok();
    let mut i = 0usize;
    while i < trace.events.len() {
        let oid = trace.events[i].original_id.clone();
        let mut j = i;
        while j < trace.events.len() && trace.events[j].original_id == oid {
            j += 1;
        }
        let group = &trace.events[i..j];
        let materializable = group.iter().any(|event| {
            !matches!(
                event.kind,
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
            .any(|event| matches!(event.kind, EventKind::UserMessage { .. }))
        {
            "user"
        } else {
            "assistant"
        };
        let msg_id = opencode_unique_id(&tx, "message", "msg", IDDirection::Ascending)?;
        let event_time = now + message_count as i64;
        emit_group(
            &tx,
            session_id,
            group,
            role,
            &msg_id,
            previous_message_id.as_deref(),
            &agent,
            &model_id,
            &provider_id,
            model_variant.as_deref(),
            &model_ref,
            &cwd,
            event_time,
            &mut event_seq,
            &mut pending_tool,
            v2,
        )?;
        previous_message_id = Some(msg_id.clone());
        anchor = Some(msg_id);
        message_count += 1;
        i = j;
    }
    let _ = dropped_event_count;

    if v2 {
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2)
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
            params![session_id, event_seq],
        )
        .map_err(|e| format!("update event sequence: {e}"))?;
    }
    tx.execute(
        "UPDATE session SET time_updated = ?1 WHERE id = ?2",
        params![now + message_count as i64, session_id],
    )
    .map_err(|e| format!("update session: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))?;

    Ok(ImportResult {
        session_id: session_id.into(),
        file: db_path.to_string_lossy().into_owned(),
        anchor_message_id: anchor.unwrap_or_default(),
        message_count,
        dropped_event_count,
    })
}

/// Emit one message group: legacy `message`/`part` rows (read by the CLI TUI
/// via `/session/:id/message`) and, when the store has the v2 tables, the
/// durable `event` log plus the `session_message` projection (read by the
/// web app and v2 clients via `/api/session/:id/message`).
#[allow(clippy::too_many_arguments)]
fn emit_group(
    tx: &Connection,
    session_id: &str,
    group: &[Event],
    role: &str,
    msg_id: &str,
    previous_message_id: Option<&str>,
    agent: &str,
    model_id: &str,
    provider_id: &str,
    model_variant: Option<&str>,
    model_ref: &Value,
    cwd: &str,
    event_time: i64,
    event_seq: &mut i64,
    pending_tool: &mut Option<(String, String, String)>,
    v2: bool,
) -> Result<(), String> {
    let start_time = group
        .iter()
        .filter_map(|e| e.time)
        .min()
        .unwrap_or(event_time);
    let end_time = group
        .iter()
        .filter_map(|e| e.time)
        .max()
        .unwrap_or(start_time);
    let (tokens, cost) = usage_from_group(group);

    // ---- legacy message row (strict v1 Message schema) ----
    let parent = previous_message_id.unwrap_or(msg_id);
    let data = opencode_message_data(
        group,
        role,
        parent,
        agent,
        model_id,
        provider_id,
        cwd,
        start_time,
        end_time,
        &tokens,
        cost,
    );
    tx.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?3, ?4)",
        params![
            msg_id,
            session_id,
            event_time,
            serde_json::to_string(&data).unwrap_or_default(),
        ],
    )
    .map_err(|e| format!("insert message: {e}"))?;

    let mut tool_call: Option<(String, String, String)> = None;
    let mut tool_result: Option<(String, String, Option<i32>, Option<String>, i64)> = None;
    let mut content: Vec<Value> = Vec::new();
    let mut text_n = 0usize;
    let mut reasoning_n = 0usize;

    // v2 durable events: the message-defining event must precede any
    // content events so the event replay reducer can attach parts.
    if v2 {
        if role == "user" {
            let text = group
                .iter()
                .find_map(|e| match &e.kind {
                    EventKind::UserMessage { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            insert_event(
                tx,
                session_id,
                event_seq,
                "session.next.prompted.1",
                &json!({
                    "sessionID": session_id,
                    "timestamp": start_time,
                    "messageID": msg_id,
                    "prompt": {"text": text},
                    "delivery": "steer",
                }),
            )?;
        } else {
            insert_event(
                tx,
                session_id,
                event_seq,
                "session.next.step.started.1",
                &json!({
                    "sessionID": session_id,
                    "timestamp": start_time,
                    "assistantMessageID": msg_id,
                    "agent": agent,
                    "model": model_ref,
                }),
            )?;
        }
    }

    for ev in group {
        match &ev.kind {
            EventKind::UserMessage { text } => {
                insert_part(
                    tx,
                    session_id,
                    msg_id,
                    json!({"type": "text", "text": text}),
                    event_time,
                )?;
            }
            EventKind::AssistantMessage { text } => {
                insert_part(
                    tx,
                    session_id,
                    msg_id,
                    json!({"type": "text", "text": text}),
                    event_time,
                )?;
                if v2 {
                    let tid = format!("text-{text_n}");
                    text_n += 1;
                    insert_event(
                        tx,
                        session_id,
                        event_seq,
                        "session.next.text.started.1",
                        &json!({
                            "sessionID": session_id,
                            "timestamp": ev.time,
                            "assistantMessageID": msg_id,
                            "textID": tid,
                        }),
                    )?;
                    insert_event(
                        tx,
                        session_id,
                        event_seq,
                        "session.next.text.ended.1",
                        &json!({
                            "sessionID": session_id,
                            "timestamp": ev.time,
                            "assistantMessageID": msg_id,
                            "textID": tid,
                            "text": text,
                        }),
                    )?;
                    content.push(json!({"type": "text", "id": tid, "text": text}));
                }
            }
            EventKind::Reasoning { text } => {
                insert_part(
                    tx,
                    session_id,
                    msg_id,
                    json!({
                        "type": "reasoning",
                        "text": text,
                        "time": {"start": ev.time, "end": ev.time},
                    }),
                    event_time,
                )?;
                if v2 {
                    let rid = format!("reasoning-{reasoning_n}");
                    reasoning_n += 1;
                    insert_event(
                        tx,
                        session_id,
                        event_seq,
                        "session.next.reasoning.started.1",
                        &json!({
                            "sessionID": session_id,
                            "timestamp": ev.time,
                            "assistantMessageID": msg_id,
                            "reasoningID": rid,
                        }),
                    )?;
                    insert_event(
                        tx,
                        session_id,
                        event_seq,
                        "session.next.reasoning.ended.1",
                        &json!({
                            "sessionID": session_id,
                            "timestamp": ev.time,
                            "assistantMessageID": msg_id,
                            "reasoningID": rid,
                            "text": text,
                        }),
                    )?;
                    content.push(json!({"type": "reasoning", "id": rid, "text": text}));
                }
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
                    Some((call_id.clone(), output.clone(), *exit_code, error.clone(), ev.time.unwrap_or(event_time)));
            }
            EventKind::ModelChange { .. } => {}
            EventKind::NativeRecord { .. } => {}
        }
    }

    // OpenCode stores call+result in a single tool part. A call and its
    // result may come from different native records (e.g. Pi), so buffer
    // the call across groups.
    if tool_call.is_some() {
        *pending_tool = tool_call.clone();
    }
    if let Some((call_id, output, exit, error, result_time)) = tool_result {
        let (name, args) = match (tool_call, &*pending_tool) {
            (Some((_, name, args)), _) => (name, args.clone()),
            (None, Some((_, name, args))) => (name.clone(), args.clone()),
            _ => ("tool".to_string(), "{}".to_string()),
        };
        let args_val: Value = serde_json::from_str(&args)
            .ok()
            .filter(|v: &Value| v.is_object())
            .unwrap_or_else(|| json!({}));
        let error_text = error.clone().unwrap_or_default();

        // ---- legacy tool part (strict v1 ToolState schema) ----
        let state = if error.is_some() {
            json!({
                "status": "error",
                "input": args_val,
                "error": error_text,
                "metadata": {},
                "time": {"start": event_time, "end": event_time},
            })
        } else {
            json!({
                "status": "completed",
                "input": args_val,
                "output": output,
                "title": "",
                "metadata": {"exit": exit},
                "time": {"start": event_time, "end": event_time},
            })
        };
        insert_part(
            tx,
            session_id,
            msg_id,
            json!({
                "type": "tool",
                "callID": call_id,
                "tool": name,
                "state": state,
            }),
            event_time,
        )?;

        // ---- v2 durable tool events + projected content ----
        if v2 {
            insert_event(
                tx,
                session_id,
                event_seq,
                "session.next.tool.input.started.1",
                &json!({
                    "sessionID": session_id,
                    "timestamp": result_time,
                    "assistantMessageID": msg_id,
                    "callID": call_id,
                    "name": name,
                }),
            )?;
            insert_event(
                tx,
                session_id,
                event_seq,
                "session.next.tool.input.ended.1",
                &json!({
                    "sessionID": session_id,
                    "timestamp": result_time,
                    "assistantMessageID": msg_id,
                    "callID": call_id,
                    "text": args,
                }),
            )?;
            insert_event(
                tx,
                session_id,
                event_seq,
                "session.next.tool.called.1",
                &json!({
                    "sessionID": session_id,
                    "timestamp": result_time,
                    "assistantMessageID": msg_id,
                    "callID": call_id,
                    "tool": name,
                    "input": args_val,
                    "provider": {"executed": false},
                }),
            )?;
            if let Some(err) = error {
                insert_event(
                    tx,
                    session_id,
                    event_seq,
                    "session.next.tool.failed.1",
                    &json!({
                        "sessionID": session_id,
                        "timestamp": result_time,
                        "assistantMessageID": msg_id,
                        "callID": call_id,
                        "error": {"type": "unknown", "message": err},
                        "provider": {"executed": false},
                    }),
                )?;
                content.push(json!({
                    "type": "tool",
                    "id": call_id,
                    "name": name,
                    "state": {
                        "status": "error",
                        "input": args_val,
                        "content": [],
                        "structured": {},
                        "error": {"type": "unknown", "message": err},
                    },
                    "time": {"created": result_time, "ran": result_time, "completed": result_time},
                }));
            } else {
                insert_event(
                    tx,
                    session_id,
                    event_seq,
                    "session.next.tool.success.1",
                    &json!({
                        "sessionID": session_id,
                        "timestamp": result_time,
                        "assistantMessageID": msg_id,
                        "callID": call_id,
                        "structured": {},
                        "content": [],
                        "result": output,
                        "provider": {"executed": false},
                    }),
                )?;
                content.push(json!({
                    "type": "tool",
                    "id": call_id,
                    "name": name,
                    "state": {
                        "status": "completed",
                        "input": args_val,
                        "content": [],
                        "structured": {},
                        "result": output,
                    },
                    "time": {"created": result_time, "ran": result_time, "completed": result_time},
                }));
            }
        }
        *pending_tool = None;
    }

    // ---- v2 durable events + session_message projection ----
    if v2 {
        let v2_data = if role == "user" {
            let text = group
                .iter()
                .find_map(|e| match &e.kind {
                    EventKind::UserMessage { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            json!({"text": text, "time": {"created": start_time}})
        } else {
            let cost_v = if cost.is_finite() { json!(cost) } else { json!(0) };
            insert_event(
                tx,
                session_id,
                event_seq,
                "session.next.step.ended.2",
                &json!({
                    "sessionID": session_id,
                    "timestamp": end_time,
                    "assistantMessageID": msg_id,
                    "finish": "stop",
                    "cost": cost_v,
                    "tokens": tokens,
                }),
            )?;
            json!({
                "agent": agent,
                "model": model_ref_json(model_id, provider_id, model_variant),
                "content": content,
                "finish": "stop",
                "cost": cost_v,
                "tokens": tokens,
                "time": {"created": start_time, "completed": end_time},
            })
        };
        let seq = *event_seq;
        tx.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                msg_id,
                session_id,
                role,
                seq,
                start_time,
                end_time,
                serde_json::to_string(&v2_data).unwrap_or_default(),
            ],
        )
        .map_err(|e| format!("insert projected message: {e}"))?;
    }

    Ok(())
}

/// v1 `Message` data (strict schema: `time`, `agent`, `model`, `parentID`,
/// `path`, `cost`, `tokens` are required by OpenCode >= 1.18).
fn opencode_message_data(
    _group: &[Event],
    role: &str,
    parent_id: &str,
    agent: &str,
    model_id: &str,
    provider_id: &str,
    cwd: &str,
    start: i64,
    end: i64,
    tokens: &Value,
    cost: f64,
) -> Value {
    let mut data = json!({
        "role": role,
        "time": {"created": start},
        "agent": agent,
    });
    if role == "user" {
        data["model"] = json!({"providerID": provider_id, "modelID": model_id});
    } else {
        data["time"]["completed"] = json!(end);
        data["parentID"] = json!(parent_id);
        data["modelID"] = json!(model_id);
        data["providerID"] = json!(provider_id);
        data["mode"] = json!(agent);
        data["path"] = json!({"cwd": cwd, "root": cwd});
        data["cost"] = json!(cost);
        data["tokens"] = tokens.clone();
        data["finish"] = json!("stop");
    }
    data
}

fn usage_from_group(group: &[Event]) -> (Value, f64) {
    let mut input = 0i64;
    let mut output = 0i64;
    let mut reasoning = 0i64;
    let mut cache_read = 0i64;
    let mut cache_write = 0i64;
    let mut cost = 0.0;
    for ev in group {
        let Some(native) = &ev.native else {
            continue;
        };
        let Some(usage) = native.get("usage") else {
            continue;
        };
        let num = |k: &str| -> i64 {
            usage
                .get(k)
                .and_then(Value::as_i64)
                .or_else(|| usage.get(k).and_then(Value::as_f64).map(|f| f as i64))
                .unwrap_or(0)
        };
        input = num("input");
        output = num("output");
        reasoning = num("reasoning");
        cache_read = num("cacheRead");
        cache_write = num("cacheWrite");
        if let Some(c) = usage.pointer("/cost/total").and_then(Value::as_f64) {
            cost = c;
        }
    }
    let tokens = json!({
        "input": input,
        "output": output,
        "reasoning": reasoning,
        "cache": {"read": cache_read, "write": cache_write},
    });
    (tokens, cost)
}

fn model_ref_json(model_id: &str, provider_id: &str, variant: Option<&str>) -> Value {
    match variant {
        Some(v) => json!({"id": model_id, "providerID": provider_id, "variant": v}),
        None => json!({"id": model_id, "providerID": provider_id}),
    }
}

fn parse_model(raw: &str) -> (String, String, Option<String>) {
    let v: Value = serde_json::from_str(raw).unwrap_or_default();
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("gpt-5.6-sol")
        .to_string();
    let provider = v
        .get("providerID")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("openai")
        .to_string();
    let variant = v.get("variant").and_then(Value::as_str).map(String::from);
    (id, provider, variant)
}

fn current_event_seq(tx: &Connection, session_id: &str) -> i64 {
    tx.query_row(
        "SELECT seq FROM event_sequence WHERE aggregate_id = ?1",
        [session_id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn insert_event(
    tx: &Connection,
    session_id: &str,
    seq: &mut i64,
    type_name: &str,
    data: &Value,
) -> Result<(), String> {
    *seq += 1;
    let eid = opencode_unique_id(tx, "event", "evt", IDDirection::Ascending)?;
    tx.execute(
        "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            &eid,
            session_id,
            *seq,
            type_name,
            serde_json::to_string(data).unwrap_or_default(),
        ],
    )
    .map_err(|e| format!("insert event: {e}"))?;
    Ok(())
}

fn opencode_agent(trace: &Trace) -> String {
    trace
        .events
        .iter()
        .filter_map(|event| event.native.as_ref())
        .filter_map(|native| native.get("agent").and_then(Value::as_str))
        .find(|agent| !agent.is_empty() && *agent != "cash")
        .unwrap_or("build")
        .to_string()
}

fn opencode_model(conn: &Connection, trace: &Trace, model_override: Option<&str>) -> String {
    let latest: Option<Value> = conn
        .query_row(
            "SELECT model FROM session
             WHERE model IS NOT NULL AND model NOT LIKE '%\"providerID\":\"cash\"%'
             ORDER BY time_updated DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .filter(valid_opencode_model);
    let source = source_model(trace).filter(valid_opencode_model);
    let mut model = if trace.meta.source == AgentKind::OpenCode {
        source.or(latest)
    } else {
        latest.or(source)
    }
    .unwrap_or_else(|| json!({"id": "gpt-5.6-sol", "providerID": "openai", "variant": "default"}));
    if let Some(model_override) = model_override {
        model["id"] = json!(model_override);
    }
    serde_json::to_string(&model).unwrap_or_default()
}

fn source_model(trace: &Trace) -> Option<Value> {
    if let Some(raw) = trace.meta.model.as_deref()
        && let Ok(model) = serde_json::from_str::<Value>(raw)
        && valid_opencode_model(&model)
    {
        return Some(model);
    }
    for event in trace.events.iter().rev() {
        if let EventKind::ModelChange { provider, model } = &event.kind
            && let Some(model) = model.as_deref()
        {
            let candidate = json!({
                "id": model,
                "providerID": provider.as_deref().unwrap_or("openai"),
                "variant": "default"
            });
            if valid_opencode_model(&candidate) {
                return Some(candidate);
            }
        }
        let Some(native) = event.native.as_ref() else {
            continue;
        };
        let model = native
            .get("modelID")
            .or_else(|| native.get("model"))
            .and_then(Value::as_str);
        let provider = native
            .get("providerID")
            .or_else(|| native.get("provider"))
            .and_then(Value::as_str);
        if let (Some(model), Some(provider)) = (model, provider) {
            let candidate = json!({"id": model, "providerID": provider, "variant": "default"});
            if valid_opencode_model(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn valid_opencode_model(model: &Value) -> bool {
    let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
    let provider = model
        .get("providerID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    !id.is_empty() && id != "cash" && !provider.is_empty() && provider != "cash"
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
            "SELECT id FROM project WHERE worktree = ?1 ORDER BY time_updated DESC, id DESC LIMIT 1",
            [cwd],
            |r| r.get(0),
        )
        .ok();
    if let Some(id) = found {
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
            "event" => conn
                .query_row("SELECT 1 FROM event WHERE id = ?1", [&id], |_| Ok(()))
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
