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

    // Events are the single representation. Response-item IDs are encoded into
    // Codex's restricted identifier alphabet, while turn linkage is preserved
    // via the `native` bag.
    let (anchor, message_count) = {
        let header = session_meta(&session_id, &cwd, &now, sessions_root);
        write_jsonl(&mut writer, &header)?;

        let mut anchor = String::new();
        let mut message_count = 0usize;
        // Sibling events derived from one native record share `original_id`.
        // Every response item needs a unique API-safe ID, including when the
        // source itself used characters Codex does not accept.
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for ev in &trace.events {
            let ts = rfc3339_ms(ev.time.unwrap_or_else(|| now.timestamp_millis()));
            // Restore the native turn linkage captured during extraction.
            let internal = ev
                .native
                .as_ref()
                .and_then(|n| n.get("internal"))
                .cloned()
                .map(|v| json!({ "internal_chat_message_metadata_passthrough": v }));
            let occurrence = {
                let n = seen.entry(ev.original_id.clone()).or_insert(0);
                *n += 1;
                *n
            };
            let native_id = imported_response_item_id(&ev.original_id, occurrence);
            match &ev.kind {
                EventKind::UserMessage { text } => {
                    let mut payload = json!({
                        "type": "message",
                        "id": native_id,
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}],
                    });
                    merge_internal(&mut payload, &internal);
                    write_jsonl(&mut writer, &response_item(&ts, payload))?;
                    // Codex's resume picker and UI transcript discover user turns
                    // from `event_msg user_message` records, not response items.
                    write_jsonl(
                        &mut writer,
                        &event_msg(
                            &ts,
                            json!({
                                "type": "user_message",
                                "message": text,
                                "images": [],
                                "local_images": [],
                                "audio": [],
                                "local_audio": [],
                                "text_elements": [],
                            }),
                        ),
                    )?;
                    anchor = native_id;
                    message_count += 1;
                }
                EventKind::AssistantMessage { text } => {
                    let mut payload = json!({
                        "type": "message",
                        "id": native_id,
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    });
                    merge_internal(&mut payload, &internal);
                    write_jsonl(&mut writer, &response_item(&ts, payload))?;
                    write_jsonl(
                        &mut writer,
                        &event_msg(
                            &ts,
                            json!({
                                "type": "agent_message",
                                "message": text,
                                "memory_citation": null,
                            }),
                        ),
                    )?;
                    anchor = native_id;
                    message_count += 1;
                }
                EventKind::Reasoning { text } => {
                    let mut payload = json!({
                        "type": "reasoning",
                        "id": native_id,
                        "summary": [{"type": "summary_text", "text": text}],
                    });
                    merge_internal(&mut payload, &internal);
                    write_jsonl(&mut writer, &response_item(&ts, payload))?;
                    anchor = native_id;
                    message_count += 1;
                }
                EventKind::ToolCall {
                    id: call_id,
                    name,
                    arguments,
                } => {
                    let mut payload = json!({
                        "type": "function_call",
                        "id": native_id,
                        "name": name,
                        "arguments": arguments,
                        "call_id": imported_call_id(call_id),
                    });
                    merge_internal(&mut payload, &internal);
                    write_jsonl(&mut writer, &response_item(&ts, payload))?;
                    anchor = native_id;
                    message_count += 1;
                }
                EventKind::ToolResult {
                    call_id, output, ..
                } => {
                    let mut payload = json!({
                        "type": "function_call_output",
                        "id": native_id,
                        "call_id": imported_call_id(call_id),
                        "output": [{"type": "input_text", "text": output}],
                    });
                    merge_internal(&mut payload, &internal);
                    write_jsonl(&mut writer, &response_item(&ts, payload))?;
                    anchor = native_id;
                    message_count += 1;
                }
                // Preserve native records with no cross-agent semantic verbatim.
                EventKind::NativeRecord { .. } => {
                    if let Some(native) = &ev.native {
                        let mut native = native.clone();
                        normalize_native_response_item(&mut native, &native_id);
                        write_jsonl(&mut writer, &native)?;
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

const IMPORTED_ITEM_PREFIX: &str = "cash_item_";
const IMPORTED_ITEM_SUFFIX: &str = "_n_";
const IMPORTED_CALL_PREFIX: &str = "call_cash_";

fn imported_response_item_id(original_id: &str, occurrence: usize) -> String {
    format!(
        "{IMPORTED_ITEM_PREFIX}{}{IMPORTED_ITEM_SUFFIX}{occurrence}",
        hex_encode(original_id)
    )
}

fn imported_call_id(original_id: &str) -> String {
    format!("{IMPORTED_CALL_PREFIX}{}", hex_encode(original_id))
}

fn hex_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn normalize_native_response_item(record: &mut Value, item_id: &str) {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return;
    }
    let Some(payload) = record.get_mut("payload").and_then(Value::as_object_mut) else {
        return;
    };
    payload.insert("id".into(), Value::String(item_id.into()));
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(call_id) = call_id {
        payload.insert("call_id".into(), Value::String(imported_call_id(&call_id)));
    }
}

fn session_meta(
    session_id: &str,
    cwd: &str,
    now: &chrono::DateTime<chrono::Utc>,
    sessions_root: &Path,
) -> Value {
    json!({
        "timestamp": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "type": "session_meta",
        "payload": {
            // Codex's deserializer fills session_id from `id` when missing, but
            // real rollouts always carry both.
            "session_id": session_id,
            "id": session_id,
            "timestamp": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "cwd": cwd,
            "originator": "cash",
            "cli_version": "0.147.0",
            "source": "cli",
            // Codex persists the session's model provider in the rollout and
            // restores it on resume; an empty/missing value makes `codex resume`
            // fail with `Model provider `` not found`. Use the target's default
            // provider (its config.toml), since Codex has no model-change event.
            "model_provider": default_model_provider(sessions_root),
            // We write the legacy layout (response_item + event_msg pairs); make
            // that explicit so future default changes cannot re-interpret it.
            "history_mode": "legacy",
        }
    })
}

/// The model provider Codex would use for a new session: `model_provider` from
/// `<codex home>/config.toml` (i.e. the parent of the sessions root), falling
/// back to Codex's built-in default.
fn default_model_provider(sessions_root: &Path) -> String {
    if let Some(config_path) = sessions_root.parent().map(|p| p.join("config.toml"))
        && let Ok(raw) = std::fs::read_to_string(&config_path)
        && let Ok(value) = raw.parse::<toml::Value>()
        && let Some(provider) = value.get("model_provider").and_then(toml::Value::as_str)
        && !provider.is_empty()
    {
        return provider.to_string();
    }
    "openai".into()
}

fn response_item(ts: &str, payload: Value) -> Value {
    json!({
        "timestamp": ts,
        "type": "response_item",
        "payload": payload,
    })
}

/// UI event-log record. The resume picker's preview and the transcript are
/// built from `event_msg` records; response items alone are not discoverable.
fn event_msg(ts: &str, payload: Value) -> Value {
    json!({
        "timestamp": ts,
        "type": "event_msg",
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
        let t = value.get("type").and_then(Value::as_str);
        // session_meta is the header; event_msg records are UI-log duplicates
        // without a payload id and are never anchors, so they do not count as
        // content appended after the anchor.
        if t == Some("session_meta") || t == Some("event_msg") {
            continue;
        }
        if found {
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
