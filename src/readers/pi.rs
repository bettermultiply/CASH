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
                    time: ts,
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
                match role {
                    "user" => {
                        let text = join_text(&content);
                        if !text.is_empty() {
                            events.push(Event {
                                time: ts.or(m.get("timestamp").and_then(Value::as_i64)),
                                kind: EventKind::UserMessage { text },
                            });
                        }
                    }
                    "assistant" => {
                        for item in &content {
                            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                                "text" => {
                                    let text = item
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    if !text.is_empty() {
                                        events.push(Event {
                                            time: ts,
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
                                        events.push(Event {
                                            time: ts,
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
                                    events.push(Event {
                                        time: ts,
                                        kind: EventKind::ToolCall {
                                            id,
                                            name,
                                            arguments,
                                        },
                                    });
                                }
                                _ => {}
                            }
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
                            time: ts,
                            kind: EventKind::ToolResult {
                                call_id,
                                output,
                                exit_code: None,
                                error: None,
                            },
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
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
