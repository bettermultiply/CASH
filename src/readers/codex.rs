use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::ir::{AgentKind, Event, EventKind, Trace, TraceMeta};
use crate::util::{parse_ts, sha256_hex};

/// Parse a Codex rollout JSONL file (e.g. ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl).
pub fn read(path: &Path) -> Result<Trace, String> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file_hash = sha256_hex(&raw);

    let mut meta = TraceMeta {
        source: AgentKind::Codex,
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
            .and_then(|x| x.as_str())
            .and_then(parse_ts);
        let entry_id = v
            .get("payload")
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("rec_{i:05}"));
        match t {
            "session_meta" => {
                let p = &v["payload"];
                meta.session_id = p
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                meta.cwd = p.get("cwd").and_then(Value::as_str).map(String::from);
                meta.started_at = p
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_ts)
                    .or(ts);
                if let Some(p) = p.get("model_provider").and_then(Value::as_str) {
                    meta.model = Some(p.to_string());
                }
            }
            "response_item" => {
                let p = &v["payload"];
                let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
                let native = codex_native(p);
                match ptype {
                    "message" => {
                        let text = collect_text(&p["content"]);
                        let role = p.get("role").and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() {
                            let ev = match role {
                                "user" | "developer" => EventKind::UserMessage { text },
                                _ => EventKind::AssistantMessage { text },
                            };
                            events.push(Event {
                                original_id: entry_id,
                                parent_original_id: None,
                                time: ts,
                                native,
                                kind: ev,
                            });
                        }
                    }
                    "function_call" | "custom_tool_call" => {
                        let id = p
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let name = p
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let arguments = if ptype == "custom_tool_call" {
                            serde_json::to_string(&json_arg("input", p.get("input")))
                                .unwrap_or_default()
                        } else {
                            p.get("arguments")
                                .map(|a| match a {
                                    Value::String(s) => s.clone(),
                                    other => serde_json::to_string(other).unwrap_or_default(),
                                })
                                .unwrap_or_default()
                        };
                        events.push(Event {
                            original_id: entry_id,
                            parent_original_id: None,
                            time: ts,
                            native,
                            kind: EventKind::ToolCall {
                                id,
                                name,
                                arguments,
                            },
                        });
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        let call_id = p
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let output = p
                            .get("output")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| collect_text(&p["output"]));
                        events.push(Event {
                            original_id: entry_id,
                            parent_original_id: None,
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
                    "reasoning" => {
                        let text = collect_text(&p["summary"]);
                        if !text.is_empty() {
                            events.push(Event {
                                original_id: entry_id,
                                parent_original_id: None,
                                time: ts,
                                native,
                                kind: EventKind::Reasoning { text },
                            });
                        }
                    }
                    // Unmodeled response_item: preserve verbatim so nothing is lost.
                    other => {
                        events.push(Event {
                            original_id: entry_id,
                            parent_original_id: None,
                            time: ts,
                            native: Some(v.clone()),
                            kind: EventKind::NativeRecord {
                                record_type: other.into(),
                            },
                        });
                    }
                }
            }
            "event_msg" => {
                let mtype = v
                    .get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str);
                if mtype == Some("task_complete") {
                    meta.ended_at = ts;
                }
            }
            // session_meta / turn_context / event_msg / world_state are infra
            // telemetry, not user content; they are not part of the event trace.
            _ => {}
        }
    }

    if meta.session_id.is_empty() {
        meta.session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());
    }
    finish_meta(&mut meta, &events);
    Ok(Trace { meta, events })
}

/// Native-only metadata of a Codex response_item: the turn linkage and any
/// per-item extras, kept so extraction loses nothing.
fn codex_native(payload: &Value) -> Option<Value> {
    payload
        .get("internal_chat_message_metadata_passthrough")
        .cloned()
        .map(|v| json!({ "internal": v }))
}

/// Recursively list rollout JSONL files under a sessions root.
pub fn list_sessions(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    if !root.exists() {
        return Err(format!("sessions dir not found: {}", root.display()));
    }
    walk(root, &mut out)?;
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
            && stem.starts_with("rollout-")
        {
            out.push((stem.to_string(), p));
        }
    }
    Ok(())
}

fn collect_text(v: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            if let Some(t) = item.get("text").and_then(Value::as_str) {
                parts.push(t.to_string());
            }
        }
    }
    parts.join("\n")
}

fn json_arg(key: &str, value: Option<&Value>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(key.into(), value.cloned().unwrap_or(Value::Null));
    Value::Object(map)
}

fn finish_meta(meta: &mut TraceMeta, events: &[Event]) {
    crate::util::finish_meta(meta, events);
}
