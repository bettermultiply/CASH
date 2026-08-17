use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ir::{AgentKind, Event, EventKind, Trace, TraceMeta};
use crate::util::sha256_hex;

/// Parse a Pi Agent session JSONL file (e.g. ~/.pi/agent/sessions/--<dir>--/<ts>_<uuid>.jsonl).
pub fn read(path: &Path) -> Result<Trace, String> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file_hash = sha256_hex(&raw);

    let mut meta = TraceMeta {
        source: AgentKind::Pi,
        session_id: String::new(),
        file: path.to_string_lossy().into_owned(),
        cwd: None,
        title: None,
        model: None,
        started_at: None,
        ended_at: None,
        source_file_sha256: file_hash.clone(),
        events_sha256: String::new(),
        event_count: 0,
    };

    let mut events: Vec<Event> = Vec::new();

    for (i, line) in raw.lines().enumerate() {
        let v: Value = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: bad json: {e}", path.display(), i + 1))?;
        let t = v.get("type").and_then(Value::as_str).unwrap_or("");
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::util::parse_ts);
        let entry_id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let parent_id = v.get("parentId").and_then(Value::as_str).map(String::from);
        match t {
            "session" => {
                meta.session_id = v
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                meta.cwd = v.get("cwd").and_then(Value::as_str).map(String::from);
                meta.started_at = ts;
            }
            "model_change" => {
                let provider = v.get("provider").and_then(Value::as_str).map(String::from);
                let model = v.get("modelId").and_then(Value::as_str).map(String::from);
                meta.model = model.clone().or(provider.clone());
                events.push(Event {
                    original_id: entry_id,
                    parent_original_id: parent_id,
                    time: ts,
                    native: None,
                    kind: EventKind::ModelChange { provider, model },
                });
            }
            "message" => {
                let m = &v["message"];
                let role = m.get("role").and_then(Value::as_str).unwrap_or("");
                let content = m
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // Native-only metadata (usage, model, responseId, ...) minus the
                // content array, which is captured by the typed events below.
                let native = message_native(m);
                match role {
                    "user" => {
                        let text = join_text(&content);
                        if !text.is_empty() {
                            events.push(Event {
                                original_id: entry_id.clone(),
                                parent_original_id: parent_id.clone(),
                                time: ts.or(m.get("timestamp").and_then(Value::as_i64)),
                                native: native.clone(),
                                kind: EventKind::UserMessage { text },
                            });
                        }
                    }
                    "assistant" => {
                        let mut modeled = false;
                        for item in &content {
                            let time = ts;
                            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                                "text" => {
                                    let text = item
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    if !text.is_empty() {
                                        modeled = true;
                                        events.push(Event {
                                            original_id: entry_id.clone(),
                                            parent_original_id: parent_id.clone(),
                                            time,
                                            native: native.clone(),
                                            kind: EventKind::AssistantMessage {
                                                text: text.to_string(),
                                            },
                                        });
                                    }
                                }
                                "thinking" => {
                                    let text = item
                                        .get("thinking")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    if !text.is_empty() {
                                        modeled = true;
                                        events.push(Event {
                                            original_id: entry_id.clone(),
                                            parent_original_id: parent_id.clone(),
                                            time,
                                            native: native.clone(),
                                            kind: EventKind::Reasoning {
                                                text: text.to_string(),
                                            },
                                        });
                                    }
                                }
                                "toolCall" => {
                                    let id = item
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    let name = item
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    let arguments = item
                                        .get("arguments")
                                        .map(|a| serde_json::to_string(a).unwrap_or_default())
                                        .unwrap_or_default();
                                    modeled = true;
                                    events.push(Event {
                                        original_id: entry_id.clone(),
                                        parent_original_id: parent_id.clone(),
                                        time,
                                        native: native.clone(),
                                        kind: EventKind::ToolCall {
                                            id,
                                            name,
                                            arguments,
                                        },
                                    });
                                }
                                // Unmodeled content block: keep the whole message
                                // verbatim so nothing is lost.
                                _ => {
                                    events.push(Event {
                                        original_id: entry_id.clone(),
                                        parent_original_id: parent_id.clone(),
                                        time,
                                        native: Some(v.clone()),
                                        kind: EventKind::NativeRecord {
                                            record_type: "content_block".into(),
                                        },
                                    });
                                    modeled = true;
                                }
                            }
                        }
                        // Empty assistant message (e.g. an error stub): keep the
                        // record so the session shape is preserved.
                        if !modeled {
                            events.push(Event {
                                original_id: entry_id.clone(),
                                parent_original_id: parent_id.clone(),
                                time: ts,
                                native: Some(v.clone()),
                                kind: EventKind::NativeRecord {
                                    record_type: "message".into(),
                                },
                            });
                        }
                    }
                    "toolResult" => {
                        let call_id = m
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let output = join_text(&content);
                        events.push(Event {
                            original_id: entry_id.clone(),
                            parent_original_id: parent_id.clone(),
                            time: ts,
                            native,
                            kind: EventKind::ToolResult {
                                call_id,
                                output,
                                exit_code: None,
                                error: None,
                            },
                        });
                    }
                    _ => {
                        // user-adjacent roles or unknown message shapes: keep verbatim.
                        events.push(Event {
                            original_id: entry_id.clone(),
                            parent_original_id: parent_id.clone(),
                            time: ts,
                            native: Some(v.clone()),
                            kind: EventKind::NativeRecord {
                                record_type: "message".into(),
                            },
                        });
                    }
                }
            }
            // Entries with no cross-agent semantic (labels, compaction,
            // thinking_level_change, custom entries, ...): preserve verbatim.
            other => {
                events.push(Event {
                    original_id: entry_id,
                    parent_original_id: parent_id,
                    time: ts,
                    native: Some(v.clone()),
                    kind: EventKind::NativeRecord {
                        record_type: other.into(),
                    },
                });
            }
        }
    }

    // Pi numbers tool calls sequentially (`toolCallId` on the result message),
    // but the assistant's toolCall content block carries an unrelated internal
    // id. Pair them in transcript order so a call and its result share one id;
    // downstream targets (Codex in particular) pair by that id.
    let calls: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.kind, EventKind::ToolCall { .. }))
        .map(|(i, _)| i)
        .collect();
    let results: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.kind, EventKind::ToolResult { .. }))
        .map(|(i, _)| i)
        .collect();
    for (call_idx, res_idx) in calls.iter().zip(&results) {
        let call_id = match &events[*res_idx].kind {
            EventKind::ToolResult { call_id, .. } => call_id.clone(),
            _ => unreachable!(),
        };
        if let EventKind::ToolCall { id, .. } = &mut events[*call_idx].kind {
            *id = call_id;
        }
    }

    if meta.session_id.is_empty() {
        meta.session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());
    }
    crate::util::finish_meta(&mut meta, &events);
    Ok(Trace { meta, events })
}

/// Native-only metadata of a Pi message: everything except the content array,
/// so usage/model/responseId/... survive extraction without duplicating text.
fn message_native(message: &Value) -> Option<Value> {
    let mut map = message.as_object()?.clone();
    map.remove("content");
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

/// List Pi sessions with the small amount of metadata needed by the CLI.
///
/// Only the prefix through the first user message is parsed, which keeps the
/// listing cheap even when session transcripts are large.
pub fn list_session_summaries(root: &Path) -> Result<Vec<super::SessionSummary>, String> {
    let mut summaries = list_sessions(root)?
        .into_iter()
        .map(|(selector, path)| summarize_session(selector, path))
        .collect::<Result<Vec<_>, _>>()?;
    super::sort_session_summaries(&mut summaries);
    Ok(summaries)
}

fn summarize_session(selector: String, path: PathBuf) -> Result<super::SessionSummary, String> {
    let mut session_id = None;
    let mut started_at = None;
    let mut cwd = None;
    let mut title = None;

    super::scan_jsonl_until(&path, |value| {
        match value.get("type").and_then(Value::as_str) {
            Some("session") => {
                session_id = value.get("id").and_then(Value::as_str).map(String::from);
                started_at = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(crate::util::parse_ts);
                cwd = value.get("cwd").and_then(Value::as_str).map(String::from);
            }
            Some("message")
                if title.is_none()
                    && value
                        .get("message")
                        .and_then(|message| message.get("role"))
                        .and_then(Value::as_str)
                        == Some("user") =>
            {
                let content = value["message"]
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let text = join_text(&content);
                if !text.trim().is_empty() {
                    title = Some(text);
                }
            }
            _ => {}
        }
        session_id.is_some() && title.is_some()
    })?;

    Ok(super::SessionSummary {
        session_id: session_id.unwrap_or(selector),
        time: started_at,
        time_kind: super::SessionTimeKind::Started,
        cwd,
        title,
    })
}

pub(super) fn native_session_id(path: &Path) -> Result<Option<String>, String> {
    let mut session_id = None;
    super::scan_jsonl_until(path, |value| {
        if value.get("type").and_then(Value::as_str) != Some("session") {
            return false;
        }
        session_id = value.get("id").and_then(Value::as_str).map(String::from);
        true
    })?;
    Ok(session_id)
}

/// Recursively list session JSONL files under a Pi sessions root.
pub fn list_sessions(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    if !root.exists() {
        return Err(format!("sessions dir not found: {}", root.display()));
    }
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read dir {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl")
            && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
        {
            out.push((stem.to_string(), p));
        }
    }
    Ok(())
}

fn join_text(content: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text")
            && let Some(t) = item.get("text").and_then(Value::as_str)
        {
            parts.push(t.to_string());
        }
    }
    parts.join("\n")
}
