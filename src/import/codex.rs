use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::import::ImportResult;
use crate::ir::{EventKind, Trace};

/// Write a trace into Codex's native rollout JSONL layout.
pub fn import(trace: &Trace, sessions_root: &Path) -> Result<ImportResult, String> {
    import_existing(trace, sessions_root, None, None, None, false, None)
}

/// Write a trace into Codex storage, reusing an existing target binding when
/// present. The existing file is replaced atomically; a target that continued
/// after the recorded anchor requires `force` to overwrite.
pub fn import_existing(
    trace: &Trace,
    sessions_root: &Path,
    existing_file: Option<&Path>,
    existing_session_id: Option<&str>,
    existing_anchor: Option<&str>,
    force: bool,
    _model_override: Option<&str>,
) -> Result<ImportResult, String> {
    let now = chrono::Utc::now();
    let session_id = existing_session_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let cwd = trace.meta.cwd.clone().unwrap_or_else(|| "/".into());

    let day = now.format("%Y/%m/%d").to_string();
    let dir = sessions_root.join(&day);
    let file = existing_file.map(Path::to_path_buf).unwrap_or_else(|| {
        let ts = now.format("%Y-%m-%dT%H-%M-%S").to_string();
        dir.join(format!("rollout-{ts}-{session_id}.jsonl"))
    });

    if let Some(anchor) = existing_anchor
        && file.exists()
        && !force
        && has_records_after_anchor(&file, anchor)?
    {
        return Err(format!(
            "target Codex session continued after anchor; refusing to overwrite {} (use --force to replace it)",
            file.display()
        ));
    }
    if existing_file.is_some() && !file.exists() && !force {
        return Err(format!(
            "target Codex session file is missing: {} (use --force to recreate it)",
            file.display()
        ));
    }

    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let temporary = file.with_extension(format!("jsonl.tmp-{}", Uuid::new_v4().simple()));
    let mut writer = std::fs::File::create(&temporary)
        .map_err(|e| format!("create {}: {e}", temporary.display()))?;

    // Events are the single representation; original_id is reused as the native
    // item id, and turn linkage preserved via the `native` bag.
    let (anchor, message_count) = {
        let header = session_meta(&session_id, &cwd, &now);
        write_jsonl(&mut writer, &header)?;

        let mut anchor = String::new();
        let mut message_count = 0usize;

        for ev in &trace.events {
            let ts = rfc3339_ms(ev.time.unwrap_or_else(|| now.timestamp_millis()));
            // Restore the native turn linkage captured during extraction.
            let internal = ev
                .native
                .as_ref()
                .and_then(|n| n.get("internal"))
                .cloned()
                .map(|v| json!({ "internal_chat_message_metadata_passthrough": v }));
            let line = match &ev.kind {
                EventKind::UserMessage { text } => {
                    let mut payload = json!({
                        "type": "message",
                        "id": ev.original_id,
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}],
                    });
                    merge_internal(&mut payload, &internal);
                    response_item(&ts, payload)
                }
                EventKind::AssistantMessage { text } => {
                    let mut payload = json!({
                        "type": "message",
                        "id": ev.original_id,
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    });
                    merge_internal(&mut payload, &internal);
                    response_item(&ts, payload)
                }
                EventKind::Reasoning { text } => {
                    let mut payload = json!({
                        "type": "reasoning",
                        "id": ev.original_id,
                        "summary": [{"type": "summary_text", "text": text}],
                    });
                    merge_internal(&mut payload, &internal);
                    response_item(&ts, payload)
                }
                EventKind::ToolCall {
                    id: call_id,
                    name,
                    arguments,
                } => {
                    let mut payload = json!({
                        "type": "function_call",
                        "id": ev.original_id,
                        "name": name,
                        "arguments": arguments,
                        "call_id": call_id,
                    });
                    merge_internal(&mut payload, &internal);
                    response_item(&ts, payload)
                }
                EventKind::ToolResult {
                    call_id, output, ..
                } => {
                    let mut payload = json!({
                        "type": "function_call_output",
                        "id": ev.original_id,
                        "call_id": call_id,
                        "output": [{"type": "input_text", "text": output}],
                    });
                    merge_internal(&mut payload, &internal);
                    response_item(&ts, payload)
                }
                // Preserve native records with no cross-agent semantic verbatim.
                EventKind::NativeRecord { .. } => {
                    if let Some(native) = &ev.native {
                        write_jsonl(&mut writer, native)?;
                        message_count += 1;
                        if let Some(id) = native
                            .get("payload")
                            .and_then(|p| p.get("id"))
                            .and_then(Value::as_str)
                        {
                            anchor = id.to_string();
                        }
                        continue;
                    }
                    continue;
                }
                // Codex has no model-change event; with the default-model policy
                // we intentionally drop the source model on import.
                EventKind::ModelChange { .. } => continue,
            };
            write_jsonl(&mut writer, &line)?;
            if let Some(id) = line
                .get("payload")
                .and_then(|p| p.get("id"))
                .and_then(Value::as_str)
            {
                anchor = id.to_string();
            }
            message_count += 1;
            let _ = ts;
        }

        (anchor, message_count)
    };

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

fn session_meta(session_id: &str, cwd: &str, now: &chrono::DateTime<chrono::Utc>) -> Value {
    json!({
        "timestamp": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "cwd": cwd,
            "originator": "cash",
            "cli_version": "0.147.0",
            "source": "cli",
        }
    })
}

fn response_item(ts: &str, payload: Value) -> Value {
    json!({
        "timestamp": ts,
        "type": "response_item",
        "payload": payload,
    })
}

fn merge_internal(payload: &mut Value, internal: &Option<Value>) {
    if let Some(internal) = internal
        && let Some(obj) = payload.as_object_mut()
        && let Some(v) = internal.get("internal_chat_message_metadata_passthrough")
    {
        obj.insert(
            "internal_chat_message_metadata_passthrough".into(),
            v.clone(),
        );
    }
}

fn rfc3339_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn write_jsonl(writer: &mut std::fs::File, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value).map_err(|e| e.to_string())?;
    writer.write_all(b"\n").map_err(|e| e.to_string())
}

pub fn has_records_after_anchor(path: &Path, anchor: &str) -> Result<bool, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut found = false;
    for line in raw.lines() {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("parse {}: {e}", path.display()))?;
        let is_record = value.get("type").and_then(Value::as_str) != Some("session_meta");
        if found && is_record {
            return Ok(true);
        }
        let id = value
            .get("payload")
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str);
        if id == Some(anchor) {
            found = true;
        }
    }
    Ok(!found)
}
