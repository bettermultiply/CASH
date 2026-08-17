use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::import::ImportResult;
use crate::ir::{EventKind, Trace};

/// Write a trace into Pi Agent's native JSONL session layout.
pub fn import(trace: &Trace, sessions_root: &Path) -> Result<ImportResult, String> {
    import_existing(trace, sessions_root, None, None, None, false, None)
}

/// Write a trace into Pi Agent storage, reusing an existing target binding when
/// present. The existing file is replaced atomically; a target that continued
/// after the recorded anchor requires `force` to overwrite.
///
/// `model_override` replaces the model used for assistant message metadata in
/// the normalized path; it does not rewrite a verbatim same-agent replay.
pub fn import_existing(
    trace: &Trace,
    sessions_root: &Path,
    existing_file: Option<&Path>,
    existing_session_id: Option<&str>,
    existing_anchor: Option<&str>,
    force: bool,
    model_override: Option<&str>,
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
        && file.exists()
        && !force
        && has_records_after_anchor(&file, anchor)?
    {
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

    // Events are the single representation. Consecutive events sharing an
    // original_id come from the same native record and are grouped back into a
    // single Pi entry. original_id / parent_original_id are reused as the native
    // ids, and native-only metadata (usage, responseId, ...) is written back, so
    // a same-agent round trip keeps the information.
    let (anchor, message_count) = {
        let header = json!({
            "type": "session",
            "version": 3,
            "id": session_id,
            "timestamp": rfc3339_ms(now),
            "cwd": cwd,
        });
        write_jsonl(&mut writer, &header)?;

        let mut anchor = String::new();
        let mut message_count = 0usize;
        let mut parent_id: Option<String> = None;
        let mut used_ids: HashSet<String> = HashSet::new();
        let mut i = 0usize;
        while i < trace.events.len() {
            let oid = trace.events[i].original_id.clone();
            let mut j = i;
            while j < trace.events.len() && trace.events[j].original_id == oid {
                j += 1;
            }
            let group = &trace.events[i..j];
            for mut entry in render_group(group, now, model_override, parent_id.as_deref(), &used_ids) {
                // Distinct native records must not share an entry id; when the
                // source reuses one original_id (e.g. OpenCode tool call+result)
                // disambiguate the derived Pi ids.
                let base = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut id = base.clone();
                let mut n = 2;
                while !used_ids.insert(id.clone()) {
                    id = format!("{base}_{n}");
                    n += 1;
                }
                entry["id"] = json!(id);
                write_jsonl(&mut writer, &entry)?;
                anchor = id.clone();
                parent_id = Some(id);
                message_count += 1;
            }
            i = j;
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

/// Render one group of events (sharing an original_id) back into Pi entries.
/// A tool result becomes its own record even when it shares an original_id with
/// the tool call (Pi stores them as separate records).
fn render_group(
    group: &[crate::ir::Event],
    now: i64,
    model_override: Option<&str>,
    parent: Option<&str>,
    used_ids: &HashSet<String>,
) -> Vec<Value> {
    let id = group[0].original_id.clone();
    // Keep the source parent only when it refers to a record already written to
    // this file (same-agent round trip). Foreign ids (e.g. OpenCode message ids
    // during a cross-agent sync) fall back to the file's own chain so Pi's
    // parentId-based view never breaks.
    let parent_id = match group[0].parent_original_id.as_deref() {
        Some(pid) if used_ids.contains(pid) => Some(pid.to_string()),
        _ => parent.map(str::to_owned),
    };
    let ts = group.iter().find_map(|e| e.time).unwrap_or(now);
    // Pi-shaped native metadata: the message object minus its content array.
    let native = group
        .iter()
        .find_map(|e| e.native.as_ref().filter(|n| n.get("role").is_some()));

    // Single native record preserved verbatim.
    if group.len() == 1
        && let EventKind::NativeRecord { .. } = &group[0].kind
        && let Some(n) = group[0].native.as_ref()
        && n.get("type").and_then(Value::as_str).is_some()
        && n.get("id").and_then(Value::as_str).is_some()
    {
        return vec![n.clone()];
    }

    // A tool result is always its own Pi record; the tool call (and any text or
    // thinking in the same native message) becomes a separate assistant message.
    let tool_results: Vec<&crate::ir::Event> = group
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ToolResult { .. }))
        .collect();
    if !tool_results.is_empty() {
        let tool_name = group
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::ToolCall { name, .. } => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "tool".into());
        let mut out = Vec::new();

        let rest: Vec<&crate::ir::Event> = group
            .iter()
            .filter(|e| !matches!(e.kind, EventKind::ToolResult { .. }))
            .collect();
        if !rest.is_empty() {
            let blocks = assistant_blocks(&rest);
            let mut message = match native {
                Some(n) if is_pi_native(n) => n.clone(),
                Some(n) => assistant_message_from_native(n, ts),
                None => json!({ "role": "assistant" }),
            };
            message["content"] = json!(blocks);
            if message.get("timestamp").is_none() {
                message["timestamp"] = json!(ts);
            }
            ensure_assistant_required_fields(&mut message);
            out.push(pi_message(&id, parent_id.as_deref(), ts, message));
        }

        for event in tool_results {
            let EventKind::ToolResult {
                call_id, output, ..
            } = &event.kind
            else {
                continue;
            };
            let event_ts = event.time.unwrap_or(ts);
            let mut message = match native {
                Some(n) if is_pi_native(n) => n.clone(),
                _ => json!({ "role": "toolResult" }),
            };
            message["role"] = json!("toolResult");
            message["content"] = json!([{ "type": "text", "text": output }]);
            if message.get("toolCallId").is_none() {
                message["toolCallId"] = json!(call_id);
            }
            if message.get("toolName").is_none() {
                message["toolName"] = json!(tool_name);
            }
            if message.get("timestamp").is_none() {
                message["timestamp"] = json!(event_ts);
            }
            out.push(pi_message(
                &event.original_id,
                parent_id.as_deref(),
                event_ts,
                message,
            ));
        }
        return out;
    }

    if group.len() == 1
        && let EventKind::ModelChange { provider, model } = &group[0].kind
    {
        return vec![json!({
            "type": "model_change",
            "id": id,
            "parentId": parent_id,
            "timestamp": rfc3339_ms(ts),
            "provider": provider.clone().unwrap_or_default(),
            "modelId": model.clone().unwrap_or_default(),
        })];
    }

    if group.len() == 1
        && let EventKind::UserMessage { text } = &group[0].kind
    {
        let mut message = match native {
            Some(n) if is_pi_native(n) => n.clone(),
            _ => json!({ "role": "user" }),
        };
        message["content"] = json!([{ "type": "text", "text": text }]);
        if message.get("timestamp").is_none() {
            message["timestamp"] = json!(ts);
        }
        return vec![pi_message(&id, parent_id.as_deref(), ts, message)];
    }

    // Assistant group: reassemble content blocks in order.
    let mut blocks: Vec<Value> = Vec::new();
    for event in group {
        match &event.kind {
            EventKind::AssistantMessage { text } => {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            EventKind::Reasoning { text } => {
                blocks.push(
                    json!({ "type": "thinking", "thinking": text, "thinkingSignature": "cash" }),
                );
            }
            EventKind::ToolCall {
                id: call_id,
                name,
                arguments,
            } => {
                let args = serde_json::from_str::<Value>(arguments)
                    .unwrap_or(Value::String(arguments.clone()));
                blocks.push(json!({
                    "type": "toolCall",
                    "id": call_id,
                    "name": name,
                    "arguments": args,
                }));
            }
            EventKind::NativeRecord { .. } => {
                // Unmodeled content block: keep the whole message verbatim.
                if let Some(n) = event.native.as_ref().filter(|n| n.get("message").is_some()) {
                    return vec![n.clone()];
                }
            }
            _ => {}
        }
    }

    let mut message = match native {
        Some(n) if is_pi_native(n) => n.clone(),
        Some(n) => assistant_message_from_native(n, ts),
        None => {
            let model = model_override
                .map(str::to_owned)
                .unwrap_or_else(|| "cash".into());
            assistant_message("cash", &model, "stop", ts, json!([]))
        }
    };
    if let Some(model) = model_override {
        message["model"] = json!(model);
    }
    message["content"] = json!(blocks);
    if message.get("timestamp").is_none() {
        message["timestamp"] = json!(ts);
    }
    ensure_assistant_required_fields(&mut message);
    vec![pi_message(&id, parent_id.as_deref(), ts, message)]
}

/// Pi-shaped message metadata carries a numeric `timestamp` (and `api` for
/// assistant messages). OpenCode/Codex messages use different field sets and
/// are normalized away so the Pi file only ever contains native Pi shapes.
fn is_pi_native(native: &Value) -> bool {
    native.get("timestamp").is_some() || native.get("api").and_then(Value::as_str).is_some()
}

/// Build a Pi-native assistant message from cross-agent metadata (e.g. an
/// OpenCode message with providerID/modelID/tokens), mapping the fields Pi
/// actually reads.
fn assistant_message_from_native(native: &Value, ts: i64) -> Value {
    let provider = native
        .get("providerID")
        .and_then(Value::as_str)
        .unwrap_or("cash")
        .to_string();
    let model = native
        .get("modelID")
        .and_then(Value::as_str)
        .unwrap_or("cash")
        .to_string();
    let mut message = assistant_message(&provider, &model, "stop", ts, json!([]));
    if let Some(tokens) = native.get("tokens").and_then(Value::as_object) {
        let num = |k: &str| {
            tokens
                .get(k)
                .and_then(Value::as_f64)
                .map(|f| f as i64)
                .unwrap_or(0)
        };
        let cache = tokens.get("cache").and_then(Value::as_object);
        let cache_read = cache
            .and_then(|c| c.get("read"))
            .and_then(Value::as_f64)
            .map(|f| f as i64)
            .unwrap_or(0);
        let cache_write = cache
            .and_then(|c| c.get("write"))
            .and_then(Value::as_f64)
            .map(|f| f as i64)
            .unwrap_or(0);
        let input = num("input");
        let output = num("output");
        let reasoning = num("reasoning");
        let total = input + output + reasoning + cache_read + cache_write;
        let cost = native.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        message["usage"] = json!({
            "input": input,
            "output": output,
            "cacheRead": cache_read,
            "cacheWrite": cache_write,
            "totalTokens": total,
            "cost": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "total": cost
            }
        });
    }
    message
}

/// Build Pi content blocks from assistant events (text / thinking / toolCall).
fn assistant_blocks(events: &[&crate::ir::Event]) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::AssistantMessage { text } => {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            EventKind::Reasoning { text } => {
                blocks.push(
                    json!({ "type": "thinking", "thinking": text, "thinkingSignature": "cash" }),
                );
            }
            EventKind::ToolCall {
                id: call_id,
                name,
                arguments,
            } => {
                let args = serde_json::from_str::<Value>(arguments)
                    .unwrap_or(Value::String(arguments.clone()));
                blocks.push(json!({
                    "type": "toolCall",
                    "id": call_id,
                    "name": name,
                    "arguments": args,
                }));
            }
            _ => {}
        }
    }
    blocks
}

/// Pi's TUI reads usage unconditionally; make sure an assistant message always
/// carries the required metadata even when the source never recorded it.
fn ensure_assistant_required_fields(message: &mut Value) {
    let obj = message.as_object_mut().expect("message is an object");
    if !obj.contains_key("usage") {
        obj.insert(
            "usage".into(),
            json!({
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            }),
        );
    }
    if !obj.contains_key("stopReason") {
        obj.insert("stopReason".into(), json!("stop"));
    }
    if !obj.contains_key("api") {
        obj.insert("api".into(), json!("cash"));
    }
    if !obj.contains_key("provider") {
        obj.insert("provider".into(), json!("cash"));
    }
    if !obj.contains_key("model") {
        obj.insert("model".into(), json!("cash"));
    }
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
