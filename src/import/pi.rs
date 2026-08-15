use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::import::ImportResult;
use crate::ir::{EventKind, Trace};

/// Write a trace into Pi Agent's native JSONL session layout.
pub fn import(trace: &Trace, sessions_root: &Path) -> Result<ImportResult, String> {
    import_existing(trace, sessions_root, None, None, None, false)
}

/// Write a trace into Pi Agent storage, reusing an existing target binding when
/// present. The existing file is replaced atomically; a target that continued
/// after the recorded anchor requires `force` to overwrite.
pub fn import_existing(
    trace: &Trace,
    sessions_root: &Path,
    existing_file: Option<&Path>,
    existing_session_id: Option<&str>,
    existing_anchor: Option<&str>,
    force: bool,
) -> Result<ImportResult, String> {
    let now = chrono::Utc::now().timestamp_millis();
    let session_id = existing_session_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let cwd = trace.meta.cwd.clone().unwrap_or_else(|| "/".into());
    let dir = sessions_root.join(pi_session_dir(&cwd));
    let file = existing_file
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.join(format!("{}_{}.jsonl", file_timestamp(now), session_id)));

    if let Some(anchor) = existing_anchor
        && file.exists() && !force && has_records_after_anchor(&file, anchor)? {
            return Err(format!(
                "target Pi session continued after anchor; refusing to overwrite {} (use --force to replace it)",
                file.display()
            ));
        }
    if existing_file.is_some() && !file.exists() && !force {
        return Err(format!(
            "target Pi session file is missing: {} (use --force to recreate it)",
            file.display()
        ));
    }

    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let temporary = file.with_extension(format!("jsonl.tmp-{}", Uuid::new_v4().simple()));
    let mut writer = std::fs::File::create(&temporary)
        .map_err(|e| format!("create {}: {e}", temporary.display()))?;

    let session_line = json!({
        "type": "session",
        "version": 3,
        "id": session_id,
        "timestamp": rfc3339_ms(now),
        "cwd": cwd,
    });
    write_jsonl(&mut writer, &session_line)?;

    let mut parent_id: Option<String> = None;
    let mut anchor = String::new();
    let mut message_count = 0usize;
    let mut used_ids = HashSet::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut current_provider = "cash".to_string();
    let mut current_model = trace.meta.model.clone().unwrap_or_else(|| "cash".into());

    for ev in &trace.events {
        let id = short_id(&mut used_ids);
        let ts = ev.time.unwrap_or(now);
        let line = match &ev.kind {
            EventKind::ModelChange { provider, model } => json!({
                "type": "model_change",
                "id": id,
                "parentId": parent_id,
                "timestamp": rfc3339_ms(ts),
                "provider": provider.clone().unwrap_or_default(),
                "modelId": model.clone().unwrap_or_default(),
            }),
            EventKind::UserMessage { text } => pi_message(
                &id,
                parent_id.as_deref(),
                ts,
                json!({
                    "role": "user",
                    "content": [{"type": "text", "text": text}],
                    "timestamp": ts,
                }),
            ),
            EventKind::AssistantMessage { text } => pi_message(
                &id,
                parent_id.as_deref(),
                ts,
                assistant_message(
                    &current_provider,
                    &current_model,
                    "stop",
                    ts,
                    json!([{"type": "text", "text": text}]),
                ),
            ),
            EventKind::Reasoning { text } => pi_message(
                &id,
                parent_id.as_deref(),
                ts,
                assistant_message(
                    &current_provider,
                    &current_model,
                    "stop",
                    ts,
                    json!([{"type": "thinking", "thinking": text, "thinkingSignature": "cash"}]),
                ),
            ),
            EventKind::ToolCall {
                id: call_id,
                name,
                arguments,
            } => {
                tool_names.insert(call_id.clone(), name.clone());
                let args = serde_json::from_str::<Value>(arguments)
                    .unwrap_or(Value::String(arguments.clone()));
                pi_message(
                    &id,
                    parent_id.as_deref(),
                    ts,
                    assistant_message(
                        &current_provider,
                        &current_model,
                        "toolUse",
                        ts,
                        json!([{"type": "toolCall", "id": call_id, "name": name, "arguments": args}]),
                    ),
                )
            }
            EventKind::ToolResult {
                call_id, output, ..
            } => {
                let tool_name = tool_names
                    .get(call_id)
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
                pi_message(
                    &id,
                    parent_id.as_deref(),
                    ts,
                    json!({
                        "role": "toolResult",
                        "toolCallId": call_id,
                        "toolName": tool_name,
                        "content": [{"type": "text", "text": output}],
                        "timestamp": ts,
                    }),
                )
            }
        };
        write_jsonl(&mut writer, &line)?;
        parent_id = Some(id.clone());
        anchor = id;
        message_count += 1;

        if let EventKind::ModelChange { provider, model } = &ev.kind {
            if let Some(provider) = provider {
                current_provider = provider.clone();
            }
            if let Some(model) = model {
                current_model = model.clone();
            }
        }
    }

    writer
        .sync_all()
        .map_err(|e| format!("flush {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, &file).map_err(|e| format!("replace {}: {e}", file.display()))?;

    Ok(ImportResult {
        session_id,
        file: file.to_string_lossy().into_owned(),
        anchor_message_id: anchor,
        message_count,
        dropped_event_count: 0,
    })
}

pub fn has_records_after_anchor(path: &Path, anchor: &str) -> Result<bool, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut found = false;
    for line in raw.lines() {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if found && value.get("type").and_then(Value::as_str) != Some("session") {
            return Ok(true);
        }
        if value.get("id").and_then(Value::as_str) == Some(anchor) {
            found = true;
        }
    }
    Ok(!found)
}

fn pi_message(id: &str, parent_id: Option<&str>, ts: i64, message: Value) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": parent_id,
        "timestamp": rfc3339_ms(ts),
        "message": message,
    })
}

fn assistant_message(
    provider: &str,
    model: &str,
    stop_reason: &str,
    ts: i64,
    content: Value,
) -> Value {
    json!({
        "role": "assistant",
        "content": content,
        "api": "cash",
        "provider": provider,
        "model": model,
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "total": 0
            }
        },
        "stopReason": stop_reason,
        "timestamp": ts,
    })
}

fn write_jsonl(writer: &mut std::fs::File, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value).map_err(|e| e.to_string())?;
    writer.write_all(b"\n").map_err(|e| e.to_string())
}

fn pi_session_dir(cwd: &str) -> String {
    let trimmed = cwd.trim_matches('/');
    if trimmed.is_empty() {
        "--root--".into()
    } else {
        format!("--{}--", trimmed.replace('/', "-"))
    }
}

fn file_timestamp(ms: i64) -> String {
    rfc3339_ms(ms).replace(':', "-")
}

fn rfc3339_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn short_id(used: &mut HashSet<String>) -> String {
    for _ in 0..100 {
        let id = Uuid::new_v4().simple().to_string()[..8].to_string();
        if used.insert(id.clone()) {
            return id;
        }
    }
    let id = Uuid::new_v4().to_string();
    used.insert(id.clone());
    id
}
