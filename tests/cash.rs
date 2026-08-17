use std::path::PathBuf;
use std::process::Command;

use cash::export;
use cash::import;
use cash::ir::EventKind;
use cash::readers;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn real_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/real")
        .join(name)
}

#[test]
fn list_pi_prints_human_readable_summaries_and_resolvable_ids() {
    let root = std::env::temp_dir().join(format!("cash-list-pi-{}", uuid::Uuid::new_v4().simple()));
    let sessions = root.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    write_jsonl_values(
        &sessions.join("2026-01-01T00-00-00-000Z_ugly-old-selector.jsonl"),
        &[
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": "pi-old",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "cwd": "/tmp/old-work"
            }),
            serde_json::json!({
                "type": "message",
                "id": "old-message",
                "parentId": null,
                "timestamp": "2026-01-01T00:00:01.000Z",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": "older request"}]
                }
            }),
        ],
    );
    write_jsonl_values(
        &sessions.join("2026-01-02T00-00-00-000Z_ugly-new-selector.jsonl"),
        &[
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": "pi-new",
                "timestamp": "2026-01-02T00:00:00.000Z",
                "cwd": "/tmp/new-work"
            }),
            serde_json::json!({
                "type": "message",
                "id": "new-message",
                "parentId": null,
                "timestamp": "2026-01-02T00:00:01.000Z",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": "newest\nrequest\twith spacing"}]
                }
            }),
        ],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cash"))
        .args(["list", "pi", "--pi-root", sessions.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Pi sessions: 2 (newest first)"));
    assert!(stdout.contains("1. newest request with spacing"));
    assert!(stdout.contains("2. older request"));
    assert!(stdout.contains("Started:   2026-01-02"));
    assert!(stdout.contains("Workspace: /tmp/new-work"));
    assert!(stdout.contains("Session:   pi-new"));
    assert!(!stdout.contains("ugly-new-selector"));
    assert!(!stdout.contains(".jsonl"));

    let seed = root.join("seed");
    let export = Command::new(env!("CARGO_BIN_EXE_cash"))
        .args([
            "export",
            "pi",
            "pi-new",
            "--pi-root",
            sessions.to_str().unwrap(),
            "--out",
            seed.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "export by listed ID failed: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(seed.join("manifest.json").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_codex_prints_human_readable_summaries_and_resolvable_ids() {
    let root =
        std::env::temp_dir().join(format!("cash-list-codex-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();

    write_jsonl_values(
        &root.join("rollout-ugly-old-selector.jsonl"),
        &[
            serde_json::json!({
                "type": "session_meta",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "payload": {
                    "id": "codex-old",
                    "timestamp": "2026-01-01T00:00:00.000Z",
                    "cwd": "/tmp/old-codex"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "payload": {
                    "id": "old-message",
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "older codex request"}]
                }
            }),
        ],
    );
    write_jsonl_values(
        &root.join("rollout-ugly-new-selector.jsonl"),
        &[
            serde_json::json!({
                "type": "session_meta",
                "timestamp": "2026-01-02T00:00:00.000Z",
                "payload": {
                    "id": "codex-new",
                    "timestamp": "2026-01-02T00:00:00.000Z",
                    "cwd": "/tmp/new-codex"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-02T00:00:01.000Z",
                "payload": {
                    "id": "context-message",
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "<environment_context>injected metadata</environment_context>"
                    }]
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-02T00:00:02.000Z",
                "payload": {
                    "id": "new-message",
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "newest codex request"}]
                }
            }),
        ],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cash"))
        .args(["list", "codex", "--codex-root", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Codex sessions: 2 (newest first)"));
    assert!(stdout.contains("1. newest codex request"));
    assert!(stdout.contains("2. older codex request"));
    assert!(stdout.contains("Started:   2026-01-02"));
    assert!(stdout.contains("Workspace: /tmp/new-codex"));
    assert!(stdout.contains("Session:   codex-new"));
    assert!(!stdout.contains("injected metadata"));
    assert!(!stdout.contains("ugly-new-selector"));
    assert!(!stdout.contains(".jsonl"));

    let seed = root.join("seed");
    let export = Command::new(env!("CARGO_BIN_EXE_cash"))
        .args([
            "export",
            "codex",
            "codex-new",
            "--codex-root",
            root.to_str().unwrap(),
            "--out",
            seed.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "export by listed ID failed: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(seed.join("manifest.json").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_opencode_prints_human_readable_summaries() {
    let root = std::env::temp_dir().join(format!(
        "cash-list-opencode-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("opencode.db");
    create_schema(&db);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let older = cash::util::parse_ts("2026-01-01T00:00:00Z").unwrap();
    let newer = cash::util::parse_ts("2026-01-02T00:00:00Z").unwrap();
    for (id, directory, title, created, updated) in [
        (
            "opencode-old",
            "/tmp/old-opencode",
            "Older OpenCode session",
            older,
            older,
        ),
        (
            "opencode-new",
            "/tmp/new-opencode",
            "Newest\nOpenCode session",
            older,
            newer,
        ),
    ] {
        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES (?1, 'global', ?1, ?2, ?3, 'test', ?4, ?5)",
            rusqlite::params![id, directory, title, created, updated],
        )
        .unwrap();
    }
    drop(conn);

    let output = Command::new(env!("CARGO_BIN_EXE_cash"))
        .args(["list", "opencode", "--opencode-db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("OpenCode sessions: 2 (newest first)"));
    assert!(stdout.contains("1. Newest OpenCode session"));
    assert!(stdout.contains("2. Older OpenCode session"));
    assert!(stdout.contains("Updated:   2026-01-02"));
    assert!(stdout.contains("Workspace: /tmp/new-opencode"));
    assert!(stdout.contains("Session:   opencode-new"));
    assert!(!stdout.contains("opencode.db"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_writes_seed_files_and_manifest() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("cash-test-{}", uuid::Uuid::new_v4().simple()));
    let manifest = export::write_seed(&trace, &dir).expect("write seed");

    assert!(dir.join("seed.json").exists());
    assert!(dir.join("seed.md").exists());
    assert!(dir.join("manifest.json").exists());
    assert_eq!(manifest.copies().len(), 1, "seed starts with one peer copy");
    assert_eq!(manifest.copies()[0].agent, "pi");
    assert_eq!(manifest.copies()[0].session_id, "pi-sess-1");

    // seed.json round-trips back to an equivalent trace
    let raw = std::fs::read_to_string(dir.join("seed.json")).unwrap();
    let re: cash::ir::Trace = serde_json::from_str(&raw).unwrap();
    assert_eq!(re.events.len(), trace.events.len());
    assert_eq!(re.meta.events_sha256, trace.meta.events_sha256);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_into_opencode_round_trips() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("cash-db-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    create_schema(&db);

    let result = import::opencode::import(&trace, &db).expect("import");
    assert!(!result.session_id.is_empty());
    assert!(result.session_id.starts_with("ses_"));
    assert_eq!(result.session_id.len(), 30);
    assert!(!result.anchor_message_id.is_empty());
    assert!(result.anchor_message_id.starts_with("msg_"));
    assert_eq!(result.anchor_message_id.len(), 30);
    // 4 native records -> 4 groups; the model_change group is dropped by the
    // OpenCode target, leaving user + assistant + toolResult = 3 messages.
    assert_eq!(result.message_count, 3);

    // re-read the imported session: model_change is the only dropped event
    let back = readers::opencode::read(&db, &result.session_id).expect("re-read");
    assert_eq!(back.events.len(), trace.events.len() - 1);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let (agent, model): (String, String) = conn
        .query_row(
            "SELECT agent, model FROM session WHERE id = ?1",
            [&result.session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(agent, "build");
    let model: serde_json::Value = serde_json::from_str(&model).unwrap();
    assert_eq!(model["id"], "deepseek-v4-flash");
    assert_eq!(model["providerID"], "deepseek");

    let messages: Vec<(String, serde_json::Value)> = conn
        .prepare("SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created, id")
        .unwrap()
        .query_map([&result.session_id], |row| {
            let id: String = row.get(0)?;
            let raw: String = row.get(1)?;
            Ok((id, serde_json::from_str(&raw).unwrap()))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(messages[0].1["role"], "user");
    assert!(messages[0].1.get("parentID").is_none());
    assert_eq!(messages[1].1["role"], "assistant");
    assert_eq!(messages[1].1["parentID"], messages[0].0);
    assert_eq!(messages[2].1["role"], "assistant");
    assert_eq!(messages[2].1["parentID"], messages[1].0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_into_pi_writes_assistant_metadata() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("cash-pi-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();

    let result = import::pi::import(&trace, &dir).expect("import pi");
    assert!(std::path::Path::new(&result.file).exists());
    assert!(!result.anchor_message_id.is_empty());

    let mut assistant_count = 0;
    for line in std::fs::read_to_string(&result.file).unwrap().lines() {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        if entry.get("type").and_then(serde_json::Value::as_str) != Some("message") {
            continue;
        }
        let message = &entry["message"];
        if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
            continue;
        }
        assistant_count += 1;
        let usage = &message["usage"];
        assert!(
            usage.is_object(),
            "assistant usage metadata is required by Pi TUI"
        );
        for field in ["input", "output", "cacheRead", "cacheWrite", "totalTokens"] {
            assert!(
                usage
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .is_some()
            );
        }
        assert!(
            usage["cost"]
                .get("total")
                .and_then(serde_json::Value::as_f64)
                .is_some()
        );
        assert!(
            message
                .get("provider")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
        assert!(
            message
                .get("model")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
        assert!(
            message
                .get("stopReason")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
    }
    assert!(assistant_count > 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_import_produces_resumable_rollout() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("cash-codex-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();

    let result = import::codex::import(&trace, &dir).expect("import codex");
    let lines = load_jsonl(std::path::Path::new(&result.file));
    assert_eq!(lines[0]["type"], "session_meta");
    assert_eq!(lines[0]["payload"]["id"], lines[0]["payload"]["session_id"]);
    assert_eq!(lines[0]["payload"]["history_mode"], "legacy");
    // Codex restores the session provider on resume; an empty value breaks
    // `codex resume` (`Model provider `` not found`), so the import must record
    // the target's default provider (here: no config.toml, so "openai").
    assert_eq!(lines[0]["payload"]["model_provider"], "openai");

    let mut counts = std::collections::HashMap::new();
    let mut event_msg_types = std::collections::HashMap::new();
    let mut response_ids = std::collections::HashSet::new();
    for line in &lines[1..] {
        let t = line["type"].as_str().unwrap();
        let ptype = line["payload"]["type"].as_str().unwrap_or("");
        if t == "response_item" {
            let id = line["payload"]["id"].as_str().unwrap();
            assert!(
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
                "Codex response-item id is not API-safe: {id}"
            );
            assert!(
                response_ids.insert(id.to_string()),
                "duplicate response_item id {id}"
            );
            *counts.entry(ptype.to_string()).or_insert(0usize) += 1;
        } else {
            assert_eq!(t, "event_msg", "unexpected record type {t}");
            *event_msg_types.entry(ptype.to_string()).or_insert(0usize) += 1;
        }
    }
    assert!(counts.contains_key("message"));
    assert!(counts.contains_key("function_call"));
    assert!(counts.contains_key("function_call_output"));
    // Codex has no model-change event; default-model policy drops it.
    assert!(!counts.contains_key("model_change"));
    // Codex's resume picker and transcript discover user/assistant turns from
    // event_msg records; without them the imported session is invisible.
    assert!(event_msg_types.contains_key("user_message"));
    assert!(event_msg_types.contains_key("agent_message"));

    // every function call links to an output via call_id
    let calls: Vec<_> = lines[1..]
        .iter()
        .filter(|l| l["payload"]["type"] == "function_call")
        .collect();
    assert!(!calls.is_empty());
    for call in &calls {
        assert!(
            call["payload"]["call_id"]
                .as_str()
                .unwrap()
                .starts_with("call_")
        );
    }

    // The written file must not look like it continued past its own anchor:
    // sibling events (reasoning + message sharing one original_id) get unique
    // ids, so re-imports reuse the target without `--force`.
    assert!(
        !import::codex::has_records_after_anchor(
            std::path::Path::new(&result.file),
            &result.anchor_message_id
        )
        .expect("anchor check")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_import_encodes_unsafe_ids_without_losing_the_trace() {
    let mut trace = readers::codex::read(&fixture("codex.jsonl")).unwrap();
    for event in &mut trace.events {
        event.original_id = "msg with spaces~and punctuation".into();
        match &mut event.kind {
            EventKind::ToolCall { id, .. } => *id = "call with spaces~and punctuation".into(),
            EventKind::ToolResult { call_id, .. } => {
                *call_id = "call with spaces~and punctuation".into()
            }
            _ => {}
        }
    }

    let dir = std::env::temp_dir().join(format!(
        "cash-codex-unsafe-id-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let result = import::codex::import(&trace, &dir).expect("import codex");
    let lines = load_jsonl(std::path::Path::new(&result.file));

    for line in lines.iter().filter(|line| line["type"] == "response_item") {
        for field in ["id", "call_id"] {
            let Some(id) = line["payload"][field].as_str() else {
                continue;
            };
            assert!(
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
                "Codex {field} is not API-safe: {id}"
            );
        }
    }

    let back = readers::codex::read(std::path::Path::new(&result.file)).unwrap();
    assert_eq!(
        serde_json::to_string(&trace.events).unwrap(),
        serde_json::to_string(&back.events).unwrap(),
        "Codex ID encoding changed the event trace"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_to_codex_round_trip_preserves_events() {
    let src = real_fixture("codex_real_sanitized.jsonl");
    let trace = readers::codex::read(&src).unwrap();
    assert!(trace.events.len() > 100);

    let dir = std::env::temp_dir().join(format!(
        "cash-codex-lossless-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let result = import::codex::import(&trace, &dir).expect("import codex");

    // session -> events is lossless: re-reading the materialized session yields
    // the same events (same original ids, content, turn linkage).
    let back = readers::codex::read(std::path::Path::new(&result.file)).unwrap();
    let a = serde_json::to_string(&trace.events).unwrap();
    let b = serde_json::to_string(&back.events).unwrap();
    assert_eq!(a, b, "codex -> codex round trip changed the event trace");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pi_to_pi_round_trip_preserves_events() {
    let src = real_fixture("pi_real_sanitized.jsonl");
    let trace = readers::pi::read(&src).unwrap();
    assert!(trace.events.len() > 100);

    let dir = std::env::temp_dir().join(format!(
        "cash-pi-lossless-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let result = import::pi::import(&trace, &dir).expect("import pi");

    // session -> events is lossless: re-reading the materialized session yields
    // the same events (same original ids, parent chain, content, native metadata).
    let back = readers::pi::read(std::path::Path::new(&result.file)).unwrap();
    let a = serde_json::to_string(&trace.events).unwrap();
    let b = serde_json::to_string(&back.events).unwrap();
    assert_eq!(a, b, "pi -> pi round trip changed the event trace");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opencode_to_opencode_round_trip_preserves_content() {
    let dir = std::env::temp_dir().join(format!(
        "cash-open-lossless-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    create_schema(&db);
    let sql = std::fs::read_to_string(real_fixture("opencode_real_sanitized.sql")).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(&sql).unwrap();
    drop(conn);

    let trace = readers::opencode::read(&db, "opencode-real-sanitized").unwrap();
    assert!(trace.events.len() > 100);

    let out = dir.join("out.db");
    create_schema(&out);
    let result = import::opencode::import(&trace, &out).expect("import opencode");
    let back = readers::opencode::read(&out, &result.session_id).unwrap();

    // OpenCode regenerates message ids on import, so a strict event-equality
    // round trip is impossible. The guarantee is content fidelity: the same
    // event kinds and payloads in the same order, ignoring regenerated ids
    // and the native metadata that is rebuilt around them.
    let strip = |events: &[cash::ir::Event]| -> String {
        let items: Vec<String> = events
            .iter()
            .map(|e| {
                let mut e = e.clone();
                e.original_id = String::new();
                e.parent_original_id = None;
                e.native = None;
                e.time = None;
                serde_json::to_string(&e).unwrap()
            })
            .collect();
        serde_json::to_string(&items).unwrap()
    };
    assert_eq!(
        strip(&trace.events),
        strip(&back.events),
        "opencode -> opencode round trip changed the trace content"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn load_jsonl(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn write_jsonl_values(path: &std::path::Path, values: &[serde_json::Value]) {
    let mut raw = values
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    raw.push('\n');
    std::fs::write(path, raw).unwrap();
}

#[test]
fn sanitized_real_fixtures_cover_rich_shapes() {
    let pi = readers::pi::read(&real_fixture("pi_real_sanitized.jsonl")).unwrap();
    assert_rich(&pi, "pi", 0);

    let codex = readers::codex::read(&real_fixture("codex_real_sanitized.jsonl")).unwrap();
    assert_rich(&codex, "codex", 20);

    let dir = std::env::temp_dir().join(format!("cash-real-sql-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    create_schema(&db);
    let sql = std::fs::read_to_string(real_fixture("opencode_real_sanitized.sql")).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(&sql).unwrap();
    drop(conn);
    let opencode = readers::opencode::read(&db, "opencode-real-sanitized").unwrap();
    assert_rich(&opencode, "opencode", 0);

    let _ = std::fs::remove_dir_all(&dir);
}

fn assert_rich(trace: &cash::ir::Trace, label: &str, min_tools: usize) {
    assert!(trace.events.len() > 100, "{label} fixture too small");
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::Reasoning { .. })),
        "{label} fixture has no reasoning"
    );
    let tool_calls = trace
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ToolCall { .. }))
        .count();
    let tool_results = trace
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ToolResult { .. }))
        .count();
    assert!(tool_calls > min_tools, "{label} fixture has too few tool calls");
    assert!(
        tool_results > min_tools,
        "{label} fixture has too few tool results"
    );
}

#[test]
fn sync_broadcasts_pi_continuation_to_all_copies() {
    let (env, _codex_root) = three_copy_env("broadcast-pi");
    let oc_session = manifest_node(&env.seed, "opencode")["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let codex_file = manifest_node(&env.seed, "codex")["file"]
        .as_str()
        .unwrap()
        .to_string();

    append_pi_continuation(&env, "continue in pi", "pi continuation");

    let synced = run_sync_session(&env, "id_0001", false);
    assert!(
        synced.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&synced.stderr)
    );
    assert!(
        String::from_utf8_lossy(&synced.stdout).contains("synced"),
        "unexpected sync output: {}",
        String::from_utf8_lossy(&synced.stdout)
    );

    let oc_trace = readers::opencode::read(&env.db, &oc_session).unwrap();
    assert_has_user(&oc_trace, "continue in pi");
    assert_has_assistant(&oc_trace, "pi continuation");

    let codex_trace = readers::codex::read(std::path::Path::new(&codex_file)).unwrap();
    assert_has_user(&codex_trace, "continue in pi");
    assert_has_assistant(&codex_trace, "pi continuation");

    // idempotent: a repeated sync appends nothing
    let repeated = run_sync_session(&env, "id_0001", false);
    assert!(repeated.status.success());
    assert!(
        String::from_utf8_lossy(&repeated.stdout).contains("no new events"),
        "repeated sync should be a no-op: {}",
        String::from_utf8_lossy(&repeated.stdout)
    );
    assert_eq!(
        readers::opencode::read(&env.db, &oc_session).unwrap().events.len(),
        oc_trace.events.len()
    );
    assert_eq!(
        readers::codex::read(std::path::Path::new(&codex_file))
            .unwrap()
            .events
            .len(),
        codex_trace.events.len()
    );

    let _ = std::fs::remove_dir_all(&env.root);
}

#[test]
fn sync_broadcasts_codex_continuation_to_all_copies() {
    let (env, _codex_root) = three_copy_env("broadcast-codex");
    let oc_session = manifest_node(&env.seed, "opencode")["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let codex_file = manifest_node(&env.seed, "codex")["file"]
        .as_str()
        .unwrap()
        .to_string();

    // Continue in codex, mixing real content with machine-injected context that
    // must NOT be propagated back as user messages.
    append_codex_records(
        &codex_file,
        &[
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-02T00:00:00.000Z",
                "payload": {"type": "message", "id": "developer-context", "role": "developer", "content": [{"type": "input_text", "text": "<permissions instructions>internal</permissions instructions>"}]}
            }),
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-02T00:00:01.000Z",
                "payload": {"type": "message", "id": "environment-context", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>internal</environment_context>"}]}
            }),
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-02T00:00:02.000Z",
                "payload": {"type": "message", "id": "codex-user", "role": "user", "content": [{"type": "input_text", "text": "continued in codex"}]}
            }),
            serde_json::json!({
                "type": "response_item",
                "timestamp": "2026-01-02T00:00:03.000Z",
                "payload": {"type": "message", "id": "codex-assistant", "role": "assistant", "content": [{"type": "output_text", "text": "codex continuation"}]}
            }),
        ],
    );

    let synced = run_sync_session(&env, "id_0001", false);
    assert!(
        synced.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&synced.stderr)
    );
    assert!(
        String::from_utf8_lossy(&synced.stdout).contains("synced"),
        "unexpected sync output: {}",
        String::from_utf8_lossy(&synced.stdout)
    );

    let is_injected = |text: &str| {
        text.contains("environment_context") || text.contains("permissions instructions")
    };
    let pi_trace = readers::pi::read(&find_jsonl(&env.pi_root)).unwrap();
    assert_has_user(&pi_trace, "continued in codex");
    assert_has_assistant(&pi_trace, "codex continuation");
    assert!(!pi_trace.events.iter().any(|event| {
        matches!(&event.kind, EventKind::UserMessage { text } if is_injected(text))
    }));

    let oc_trace = readers::opencode::read(&env.db, &oc_session).unwrap();
    assert_has_user(&oc_trace, "continued in codex");
    assert_has_assistant(&oc_trace, "codex continuation");
    assert!(!oc_trace.events.iter().any(|event| {
        matches!(&event.kind, EventKind::UserMessage { text } if is_injected(text))
    }));

    // idempotent: a repeated sync appends nothing
    let repeated = run_sync_session(&env, "id_0001", false);
    assert!(repeated.status.success());
    assert!(
        String::from_utf8_lossy(&repeated.stdout).contains("no new events"),
        "repeated sync should be a no-op: {}",
        String::from_utf8_lossy(&repeated.stdout)
    );
    assert_eq!(
        readers::pi::read(&find_jsonl(&env.pi_root)).unwrap().events.len(),
        pi_trace.events.len()
    );

    let _ = std::fs::remove_dir_all(&env.root);
}

#[test]
fn convert_refuses_continued_pi_target_unless_forced() {
    let env = cli_test_env("conflict");
    let first = run_convert(&env, false);
    assert!(
        first.status.success(),
        "first convert failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let target = manifest_target(&env.seed);
    let target_file = PathBuf::from(target["file"].as_str().unwrap());
    let anchor = target["anchor_message_id"].as_str().unwrap();
    let extra = serde_json::json!({
        "type": "thinking_level_change",
        "id": "extra001",
        "parentId": anchor,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "thinkingLevel": "high"
    });
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&target_file)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&extra).unwrap()).unwrap();

    let conflict = run_convert(&env, false);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("--force"));

    let forced = run_convert(&env, true);
    assert!(
        forced.status.success(),
        "forced convert failed: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    let forced_target = manifest_target(&env.seed);
    assert_eq!(forced_target["session_id"], target["session_id"]);
    assert_eq!(forced_target["file"], target["file"]);
    assert_eq!(count_jsonl(&env.pi_root), 1);
    assert!(
        !std::fs::read_to_string(&target_file)
            .unwrap()
            .contains("extra001")
    );

    let _ = std::fs::remove_dir_all(&env.root);
}

#[test]
fn opencode_import_updates_same_session_and_protects_continuation() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let root = std::env::temp_dir().join(format!(
        "cash-open-update-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("opencode.db");
    create_schema(&db);

    let first = import::opencode::import_existing(&trace, &db, None, None, false, None).unwrap();
    let second = import::opencode::import_existing(
        &trace,
        &db,
        Some(&first.session_id),
        Some(&first.anchor_message_id),
        false,
        None,
    )
    .unwrap();
    assert_eq!(second.session_id, first.session_id);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1);

    let last_time: i64 = conn
        .query_row(
            "SELECT MAX(time_created) FROM message WHERE session_id = ?1",
            [&first.session_id],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('msg_continued', ?1, ?2, ?2, '{\"role\":\"user\"}')",
        rusqlite::params![&first.session_id, last_time + 1],
    )
    .unwrap();
    drop(conn);

    let conflict = import::opencode::import_existing(
        &trace,
        &db,
        Some(&first.session_id),
        Some(&second.anchor_message_id),
        false,
        None,
    );
    assert!(conflict.unwrap_err().contains("--force"));

    let forced = import::opencode::import_existing(
        &trace,
        &db,
        Some(&first.session_id),
        Some(&second.anchor_message_id),
        true,
        None,
    )
    .unwrap();
    assert_eq!(forced.session_id, first.session_id);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let continued: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message WHERE id = 'msg_continued'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, 0);
    // --force must also rebuild the v2 event log and projection consistently
    // (regression: the force path used to leave stale rows behind).
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1",
            [&first.session_id],
            |r| r.get(0),
        )
        .unwrap();
    let seq_row: i64 = conn
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?1",
            [&first.session_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(seq_row, events, "event_sequence must match the event log");
    assert!(events > 0);
    let projected: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_message WHERE session_id = ?1",
            [&first.session_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(projected, 3); // user + assistant + toolResult groups
    let messages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message WHERE session_id = ?1",
            [&first.session_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(projected, messages, "projection and legacy rows must agree");

    let _ = std::fs::remove_dir_all(&root);
}

/// Bug 1 regression: after `convert pi -> opencode`, the OpenCode CLI TUI reads
/// the legacy `message`/`part` tables with a strict v1 schema, and the web app
/// reads the v2 `session_message` projection backed by the durable `event` log.
/// All three stores must be displayable and consistent.
#[test]
fn opencode_import_writes_displayable_legacy_and_v2_state() {
    let trace = readers::pi::read(&real_fixture("pi_real_sanitized.jsonl")).unwrap();
    let dir =
        std::env::temp_dir().join(format!("cash-v2db-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    create_schema(&db);

    let result = import::opencode::import(&trace, &db).expect("import");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let session = &result.session_id;

    // -- legacy message rows satisfy the strict v1 schema the TUI decodes
    let messages: Vec<serde_json::Value> = conn
        .prepare("SELECT data FROM message WHERE session_id = ?1 ORDER BY time_created, id")
        .unwrap()
        .query_map([session], |row| {
            let raw: String = row.get(0)?;
            Ok(serde_json::from_str(&raw).unwrap())
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(messages.len() > 50, "rich fixture should import many messages");
    let mut user_count = 0;
    for message in &messages {
        let role = message["role"].as_str().expect("role");
        assert!(
            message
                .pointer("/time/created")
                .and_then(serde_json::Value::as_i64)
                .is_some(),
            "message missing time.created: {message}"
        );
        assert!(message.get("agent").and_then(serde_json::Value::as_str).is_some());
        match role {
            "user" => {
                user_count += 1;
                assert!(message
                    .pointer("/model/providerID")
                    .and_then(serde_json::Value::as_str)
                    .is_some());
                assert!(message
                    .pointer("/model/modelID")
                    .and_then(serde_json::Value::as_str)
                    .is_some());
            }
            "assistant" => {
                assert!(message.get("parentID").and_then(serde_json::Value::as_str).is_some());
                for field in ["modelID", "providerID", "mode", "agent", "cost", "finish"] {
                    assert!(message.get(field).is_some(), "assistant missing {field}");
                }
                assert!(message.pointer("/path/cwd").and_then(serde_json::Value::as_str).is_some());
                assert!(message.pointer("/path/root").and_then(serde_json::Value::as_str).is_some());
                assert!(message
                    .pointer("/tokens/input")
                    .and_then(serde_json::Value::as_i64)
                    .is_some());
                assert!(message
                    .pointer("/tokens/cache/read")
                    .and_then(serde_json::Value::as_i64)
                    .is_some());
            }
            other => panic!("unexpected role {other}"),
        }
    }
    assert!(user_count >= 3);

    // -- legacy parts: reasoning carries time; tool parts carry callID/tool/state
    let parts: Vec<serde_json::Value> = conn
        .prepare("SELECT data FROM part WHERE session_id = ?1")
        .unwrap()
        .query_map([session], |row| {
            let raw: String = row.get(0)?;
            Ok(serde_json::from_str(&raw).unwrap())
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for part in &parts {
        match part["type"].as_str().unwrap_or_default() {
            "reasoning" => {
                assert!(part
                    .pointer("/time/start")
                    .and_then(serde_json::Value::as_i64)
                    .is_some());
                assert!(part
                    .pointer("/time/end")
                    .and_then(serde_json::Value::as_i64)
                    .is_some());
            }
            "tool" => {
                assert!(part.get("callID").and_then(serde_json::Value::as_str).is_some());
                assert!(part.get("tool").and_then(serde_json::Value::as_str).is_some());
                let status = part
                    .pointer("/state/status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                assert!(
                    matches!(status, "pending" | "running" | "completed" | "error"),
                    "bad tool state {status}"
                );
            }
            _ => {}
        }
    }
    assert!(parts.iter().any(|p| p["type"] == "reasoning"));
    assert!(parts.iter().any(|p| p["type"] == "tool"));

    // -- v2 session_message projection rows
    let projected: Vec<(String, String, i64, serde_json::Value)> = conn
        .prepare("SELECT id, type, seq, data FROM session_message WHERE session_id = ?1 ORDER BY seq")
        .unwrap()
        .query_map([session], |row| {
            let raw: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                serde_json::from_str(&raw).unwrap(),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(projected.len(), messages.len());
    let mut prev_seq = 0i64;
    for (id, mtype, seq, data) in &projected {
        assert!(seq > &prev_seq, "seq must be strictly increasing");
        prev_seq = *seq;
        assert!(id.starts_with("msg_"));
        assert!(data
            .pointer("/time/created")
            .and_then(serde_json::Value::as_i64)
            .is_some());
        match mtype.as_str() {
            "user" => {
                assert!(data.get("text").and_then(serde_json::Value::as_str).is_some());
                assert!(data.get("agent").is_none());
            }
            "assistant" => {
                assert!(data.get("agent").and_then(serde_json::Value::as_str).is_some());
                assert!(data
                    .pointer("/model/id")
                    .and_then(serde_json::Value::as_str)
                    .is_some());
                assert!(data
                    .pointer("/model/providerID")
                    .and_then(serde_json::Value::as_str)
                    .is_some());
                assert!(data.get("finish").and_then(serde_json::Value::as_str).is_some());
                assert!(data
                    .pointer("/tokens/input")
                    .and_then(serde_json::Value::as_i64)
                    .is_some());
                let content = data.get("content").and_then(serde_json::Value::as_array).unwrap();
                for item in content {
                    let t = item["type"].as_str().expect("content type");
                    assert!(matches!(t, "text" | "reasoning" | "tool"));
                }
            }
            other => panic!("unexpected projected type {other}"),
        }
    }

    // -- v2 durable event log: versioned types, seq continuity, ordering
    let events: Vec<(i64, String, serde_json::Value)> = conn
        .prepare("SELECT seq, type, data FROM event WHERE aggregate_id = ?1 ORDER BY seq")
        .unwrap()
        .query_map([session], |row| {
            let raw: String = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&raw).unwrap()))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let seq_row: i64 = conn
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?1",
            [session],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(seq_row as usize, events.len());
    for (i, (seq, etype, data)) in events.iter().enumerate() {
        assert_eq!(*seq, i as i64 + 1, "seq must start at 1 and be contiguous");
        assert!(
            etype.ends_with(".1") || etype.ends_with(".2"),
            "unversioned event {etype}"
        );
        assert!(data.get("sessionID").and_then(serde_json::Value::as_str) == Some(session));
        assert!(data
            .get("timestamp")
            .and_then(serde_json::Value::as_i64)
            .is_some());
    }

    // per assistant message the event order must be:
    // step.started -> content events (text/reasoning/tool) -> step.ended,
    // so the event replay reducer can attach parts to the message.
    let mut order: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for (_, etype, data) in &events {
        let msg = data
            .get("assistantMessageID")
            .or_else(|| data.get("messageID"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let short = etype.trim_end_matches(".1").trim_end_matches(".2");
        order.entry(msg).or_default().push(short);
    }
    for (msg, seqs) in &order {
        if msg.is_empty() {
            continue;
        }
        let steps: Vec<&&str> = seqs
            .iter()
            .filter(|t| t.starts_with("session.next.step."))
            .collect();
        if steps.is_empty() {
            continue; // user prompts only carry session.next.prompted
        }
        assert_eq!(
            steps.first(),
            Some(&&"session.next.step.started"),
            "step.started must come first for {msg}"
        );
        assert_eq!(
            steps.last(),
            Some(&&"session.next.step.ended"),
            "step.ended must come last for {msg}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Bug 2 regression: `sync` (opencode -> pi) must append records that Pi's TUI
/// can actually display: Pi-native message shapes and a parentId chain that
/// reaches every record (Pi rebuilds its view by walking parentId from the
/// leaf, so foreign ids from OpenCode used to truncate the chain).
#[test]
fn sync_broadcasts_opencode_continuation_to_all_copies() {
    let (env, _codex_root) = three_copy_env("broadcast-opencode");
    let oc_session = manifest_node(&env.seed, "opencode")["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let codex_file = manifest_node(&env.seed, "codex")["file"]
        .as_str()
        .unwrap()
        .to_string();

    // Simulate the OpenCode server continuing the session (it writes legacy
    // v1-shaped messages, some of which have no parts when generation fails).
    //   A: user with a text part
    //   B: assistant with NO parts (failed generation) -- the reader skips it,
    //      but the next message still references it as parent
    //   C: user with a text part
    //   D: assistant with reasoning + text parts
    let conn = rusqlite::Connection::open(&env.db).unwrap();
    let last_time: i64 = conn
        .query_row(
            "SELECT MAX(time_created) FROM message WHERE session_id = ?1",
            [&oc_session],
            |r| r.get(0),
        )
        .unwrap();
    let insert_message = |conn: &rusqlite::Connection,
                          id: &str,
                          time: i64,
                          data: serde_json::Value| {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![id, &oc_session, time, serde_json::to_string(&data).unwrap()],
        )
        .unwrap();
    };
    let insert_part = |conn: &rusqlite::Connection,
                       id: &str,
                       message_id: &str,
                       time: i64,
                       data: serde_json::Value| {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![
                id,
                message_id,
                &oc_session,
                time,
                serde_json::to_string(&data).unwrap()
            ],
        )
        .unwrap();
    };
    let user_data = |t: i64| {
        serde_json::json!({
            "role": "user",
            "time": {"created": t},
            "agent": "build",
            "model": {"providerID": "deepseek", "modelID": "deepseek-v4-flash"},
            "summary": {"diffs": []}
        })
    };
    let assistant_data = |parent: &str, t: i64| {
        serde_json::json!({
            "parentID": parent,
            "role": "assistant",
            "mode": "build",
            "agent": "build",
            "path": {"cwd": "/tmp", "root": "/tmp"},
            "cost": 0.000123,
            "tokens": {"input": 10, "output": 20, "reasoning": 5, "cache": {"read": 7, "write": 0}},
            "modelID": "deepseek-v4-flash",
            "providerID": "deepseek",
            "time": {"created": t, "completed": t + 100}
        })
    };
    let mut t = last_time;
    t += 1;
    insert_message(&conn, "msg_cont_user_a", t, user_data(t));
    insert_part(&conn, "prt_cont_a", "msg_cont_user_a", t, serde_json::json!({"type": "text", "text": "continue in opencode"}));
    t += 1;
    insert_message(&conn, "msg_cont_empty_b", t, assistant_data("msg_cont_user_a", t));
    t += 1;
    insert_message(&conn, "msg_cont_user_c", t, user_data(t));
    insert_part(&conn, "prt_cont_c", "msg_cont_user_c", t, serde_json::json!({"type": "text", "text": "and again"}));
    t += 1;
    insert_message(&conn, "msg_cont_assistant_d", t, assistant_data("msg_cont_user_c", t));
    insert_part(
        &conn,
        "prt_cont_d1",
        "msg_cont_assistant_d",
        t,
        serde_json::json!({
            "type": "reasoning",
            "text": "thinking about it",
            "time": {"start": t, "end": t}
        }),
    );
    insert_part(
        &conn,
        "prt_cont_d2",
        "msg_cont_assistant_d",
        t,
        serde_json::json!({"type": "text", "text": "the continuation answer"}),
    );
    drop(conn);

    let synced = run_sync_session(&env, "id_0001", false);
    assert!(
        synced.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&synced.stderr)
    );
    assert!(
        String::from_utf8_lossy(&synced.stdout).contains("synced"),
        "unexpected sync output: {}",
        String::from_utf8_lossy(&synced.stdout)
    );

    // ---- pi copy: A, C, D as Pi-native records, B skipped ----
    let file = find_jsonl(&env.pi_root);
    let entries = load_jsonl(&file);
    let by_id: std::collections::HashMap<&str, &serde_json::Value> = entries
        .iter()
        .map(|e| (e["id"].as_str().unwrap(), e))
        .collect();
    let synced_entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| {
            e.get("id")
                .and_then(serde_json::Value::as_str)
                .map(|id| id.starts_with("msg_"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        synced_entries.len(),
        3,
        "expected synced records A, C, D: {}",
        serde_json::to_string(&synced_entries).unwrap()
    );

    // every record's parentId must exist in the file (or be null)
    for entry in &entries {
        match entry["parentId"].as_str() {
            None | Some("") => {}
            Some(pid) => assert!(
                by_id.contains_key(pid),
                "dangling parentId {pid} in {}",
                serde_json::to_string(entry).unwrap()
            ),
        }
    }

    // Pi's buildSessionPath: walking parentId from the leaf reaches every
    // record except the session header (nothing references it).
    let leaf = entries.last().unwrap();
    let mut path = Vec::new();
    let mut current = Some(leaf);
    while let Some(entry) = current {
        path.push(entry);
        current = entry["parentId"].as_str().and_then(|pid| by_id.get(pid).copied());
    }
    let non_header = entries
        .iter()
        .filter(|e| e.get("type").and_then(serde_json::Value::as_str) != Some("session"))
        .count();
    assert_eq!(
        path.len(),
        non_header,
        "parentId chain must reach every record (Pi renders only the chain)"
    );

    // synced assistant is Pi-native and maps provider/model/usage from OpenCode
    let d_msg = &by_id["msg_cont_assistant_d"]["message"];
    assert_eq!(d_msg["role"], "assistant");
    assert_eq!(d_msg["provider"], "deepseek");
    assert_eq!(d_msg["model"], "deepseek-v4-flash");
    for field in ["api", "usage", "stopReason", "timestamp"] {
        assert!(d_msg.get(field).is_some(), "assistant missing {field}");
    }
    for foreign in ["agent", "modelID", "providerID", "path", "tokens", "time", "finish", "cost", "summary"] {
        assert!(
            d_msg.get(foreign).is_none(),
            "opencode field {foreign} leaked into pi assistant record"
        );
    }
    let content = d_msg["content"].as_array().unwrap();
    assert!(content.iter().any(|c| c["type"] == "thinking" && c["thinking"] == "thinking about it"));
    let text = content
        .iter()
        .find(|c| c["type"] == "text")
        .and_then(|c| c["text"].as_str())
        .unwrap();
    assert_eq!(text, "the continuation answer");
    assert_eq!(d_msg["usage"]["totalTokens"].as_i64(), Some(42)); // 10+20+5+7

    // ---- codex copy: same real content arrives as native records ----
    let codex_trace = readers::codex::read(std::path::Path::new(&codex_file)).unwrap();
    assert_has_user(&codex_trace, "continue in opencode");
    assert_has_assistant(&codex_trace, "the continuation answer");

    // idempotent: a second sync appends nothing
    let before = count_jsonl(&env.pi_root);
    let repeated = run_sync_session(&env, "id_0001", false);
    assert!(repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("no new events"));
    assert_eq!(count_jsonl(&env.pi_root), before);
    assert_eq!(
        readers::codex::read(std::path::Path::new(&codex_file))
            .unwrap()
            .events
            .len(),
        codex_trace.events.len()
    );

    let _ = std::fs::remove_dir_all(&env.root);
}

/// Codex import is idempotent like the Pi/OpenCode targets: re-importing the
/// same trace reuses the same session and file, a continued target is refused
/// unless forced, and `--force` replaces the content while keeping the session
/// identity.
#[test]
fn codex_import_updates_same_session_and_protects_continuation() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let root = std::env::temp_dir().join(format!(
        "cash-codex-update-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let first = import::codex::import(&trace, &root).expect("first codex import");
    let clean_back = readers::codex::read(std::path::Path::new(&first.file)).unwrap();

    let second = import::codex::import_existing(
        &trace,
        &root,
        Some(std::path::Path::new(&first.file)),
        Some(&first.session_id),
        Some(&first.anchor_message_id),
        false,
        None,
    )
    .expect("second codex import");
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.file, first.file);
    assert_eq!(count_jsonl(&root), 1);

    // Simulate continuing the codex session after the imported anchor.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&first.file)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-01-02T00:00:00.000Z",
            "payload": {"type": "message", "id": "msg_continued", "role": "user", "content": [{"type": "input_text", "text": "continued in codex"}]}
        }))
        .unwrap()
    )
    .unwrap();
    drop(file);

    let conflict = import::codex::import_existing(
        &trace,
        &root,
        Some(std::path::Path::new(&first.file)),
        Some(&first.session_id),
        Some(&first.anchor_message_id),
        false,
        None,
    );
    assert!(
        conflict.unwrap_err().contains("--force"),
        "continued codex target must be refused without --force"
    );

    let forced = import::codex::import_existing(
        &trace,
        &root,
        Some(std::path::Path::new(&first.file)),
        Some(&first.session_id),
        Some(&first.anchor_message_id),
        true,
        None,
    )
    .expect("forced codex import");
    assert_eq!(forced.session_id, first.session_id);
    assert!(
        !std::fs::read_to_string(&first.file).unwrap().contains("msg_continued"),
        "--force must remove the appended continuation"
    );

    // The replaced file is byte-for-byte equivalent to the fresh import.
    let forced_back = readers::codex::read(std::path::Path::new(&first.file)).unwrap();
    assert_eq!(
        serde_json::to_string(&clean_back.events).unwrap(),
        serde_json::to_string(&forced_back.events).unwrap(),
        "forced re-import changed the event trace"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_refuses_when_multiple_copies_changed() {
    let (env, _codex_root) = three_copy_env("sync-conflict");
    let codex_file = manifest_node(&env.seed, "codex")["file"]
        .as_str()
        .unwrap()
        .to_string();

    // Two copies gain independent content at the same time.
    append_pi_continuation(&env, "continue in pi", "pi continuation");
    append_codex_records(
        &codex_file,
        &[serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-01-02T00:00:00.000Z",
            "payload": {"type": "message", "id": "codex-user", "role": "user", "content": [{"type": "input_text", "text": "continued in codex"}]}
        })],
    );

    let synced = run_sync_session(&env, "id_0001", false);
    assert!(
        !synced.status.success(),
        "sync must refuse to merge divergent copies"
    );
    let stderr = String::from_utf8_lossy(&synced.stderr);
    assert!(
        stderr.contains("conflict"),
        "expected a conflict error, got: {stderr}"
    );

    // Nothing was propagated.
    let oc_session = manifest_node(&env.seed, "opencode")["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let oc_trace = readers::opencode::read(&env.db, &oc_session).unwrap();
    assert!(!oc_trace.events.iter().any(|e| {
        matches!(&e.kind, EventKind::UserMessage { text } if text == "continue in pi" || text == "continued in codex")
    }));

    let _ = std::fs::remove_dir_all(&env.root);
}

#[test]
fn sync_refuses_when_unchanged_copy_diverged() {
    let (env, _codex_root) = three_copy_env("sync-divergence");
    let codex_file = manifest_node(&env.seed, "codex")["file"]
        .as_str()
        .unwrap()
        .to_string();

    // The codex copy gains only non-syncable injected context: its event hash
    // changes but it produces no syncable delta, so sync must treat it as an
    // unchanged copy that diverged, and refuse rather than clobber it.
    append_codex_records(
        &codex_file,
        &[serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-01-02T00:00:00.000Z",
            "payload": {"type": "message", "id": "environment-context", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>injected</environment_context>"}]}
        })],
    );
    append_pi_continuation(&env, "continue in pi", "pi continuation");

    let synced = run_sync_session(&env, "id_0001", false);
    assert!(
        !synced.status.success(),
        "sync must refuse when an unchanged copy changed independently"
    );
    let stderr = String::from_utf8_lossy(&synced.stderr);
    assert!(
        stderr.contains("changed independently"),
        "expected a divergence error, got: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&env.root);
}

struct CliTestEnv {
    root: PathBuf,
    db: PathBuf,
    seed: PathBuf,
    pi_root: PathBuf,
}
fn cli_test_env(label: &str) -> CliTestEnv {
    let root = std::env::temp_dir().join(format!(
        "cash-cli-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("opencode.db");
    create_schema(&db);
    let sql = std::fs::read_to_string(real_fixture("opencode_real_sanitized.sql")).unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(&sql)
        .unwrap();
    CliTestEnv {
        seed: root.join("opencode").join("opencode-real-sanitized"),
        pi_root: root.join("pi-root"),
        root,
        db,
    }
}

fn run_sync_session(env: &CliTestEnv, session: &str, force: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cash"));
    command.args([
        "sync",
        session,
        "--pi-root",
        env.pi_root.to_str().unwrap(),
        "--opencode-db",
        env.db.to_str().unwrap(),
        "--codex-root",
        env.root.join("codex-root").to_str().unwrap(),
    ]);
    command.env("CASH_SEED_DIR", &env.root);
    if force {
        command.arg("--force");
    }
    command.output().unwrap()
}

fn run_convert(env: &CliTestEnv, force: bool) -> std::process::Output {
    run_convert_session(env, "opencode", "opencode-real-sanitized", "pi", force)
}

/// Environment whose source is the real Pi fixture (`pi_real_sanitized.jsonl`,
/// session id `id_0001`) instead of the OpenCode fixture.
fn cli_pi_source_env(label: &str) -> CliTestEnv {
    let root = std::env::temp_dir().join(format!(
        "cash-cli-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let pi_root = root.join("pi-root");
    std::fs::create_dir_all(&pi_root).unwrap();
    std::fs::copy(
        real_fixture("pi_real_sanitized.jsonl"),
        pi_root.join("session.jsonl"),
    )
    .unwrap();
    let db = root.join("opencode.db");
    create_schema(&db);
    CliTestEnv {
        seed: root.join("pi").join("id_0001"),
        pi_root,
        root,
        db,
    }
}

fn run_convert_pi(env: &CliTestEnv, force: bool) -> std::process::Output {
    run_convert_session(env, "pi", "id_0001", "opencode", force)
}

fn run_convert_session(
    env: &CliTestEnv,
    source: &str,
    session: &str,
    target: &str,
    force: bool,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cash"));
    command.args([
        "convert",
        source,
        session,
        target,
        "--seed",
        env.seed.to_str().unwrap(),
        "--pi-root",
        env.pi_root.to_str().unwrap(),
        "--opencode-db",
        env.db.to_str().unwrap(),
    ]);
    if force {
        command.arg("--force");
    }
    command.output().unwrap()
}

fn find_jsonl(root: &std::path::Path) -> PathBuf {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            let found = find_jsonl(&path);
            if found.exists() {
                return found;
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            return path;
        }
    }
    panic!("no jsonl under {}", root.display())
}

fn manifest_target(seed: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(seed.join("manifest.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()["nodes"]
        .as_array()
        .and_then(|nodes| nodes.last().cloned())
        .expect("manifest has at least one node")
}

fn manifest_node(seed: &std::path::Path, agent: &str) -> serde_json::Value {
    let raw = std::fs::read_to_string(seed.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
    manifest["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .find(|node| node["agent"] == agent)
        .cloned()
        .unwrap_or_else(|| panic!("no {agent} node in manifest"))
}

/// Build a three-copy peer group (pi + opencode + codex) from the real pi
/// fixture, so sync tests can verify broadcast propagation in every direction.
fn three_copy_env(label: &str) -> (CliTestEnv, PathBuf) {
    let env = cli_pi_source_env(label);
    let codex_root = env.root.join("codex-root");
    std::fs::create_dir_all(&codex_root).unwrap();

    let to_opencode = run_convert_pi(&env, false);
    assert!(
        to_opencode.status.success(),
        "pi -> opencode convert failed: {}",
        String::from_utf8_lossy(&to_opencode.stderr)
    );
    let oc_session = manifest_target(&env.seed)["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    let to_codex = Command::new(env!("CARGO_BIN_EXE_cash"))
        .args([
            "convert",
            "opencode",
            &oc_session,
            "codex",
            "--seed",
            env.seed.to_str().unwrap(),
            "--pi-root",
            env.pi_root.to_str().unwrap(),
            "--opencode-db",
            env.db.to_str().unwrap(),
            "--codex-root",
            codex_root.to_str().unwrap(),
        ])
        .env("CASH_SEED_DIR", &env.root)
        .output()
        .unwrap();
    assert!(
        to_codex.status.success(),
        "opencode -> codex convert failed: {}",
        String::from_utf8_lossy(&to_codex.stderr)
    );
    (env, codex_root)
}

fn append_pi_continuation(env: &CliTestEnv, user: &str, assistant: &str) {
    let source = env.pi_root.join("session.jsonl");
    let trace = readers::pi::read(&source).unwrap();
    let last_id = trace.events.last().unwrap().original_id.clone();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap();
    for value in [
        serde_json::json!({
            "type": "message",
            "id": "cont-user",
            "parentId": last_id,
            "timestamp": "2026-01-02T00:00:00.000Z",
            "message": {"role": "user", "content": [{"type": "text", "text": user}]}
        }),
        serde_json::json!({
            "type": "message",
            "id": "cont-assistant",
            "parentId": "cont-user",
            "timestamp": "2026-01-02T00:00:01.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": assistant}], "model": "cash", "provider": "cash", "api": "cash", "stopReason": "stop", "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}}
        }),
    ] {
        use std::io::Write;
        writeln!(file, "{}", serde_json::to_string(&value).unwrap()).unwrap();
    }
    drop(file);
}

fn append_codex_records(file: &str, values: &[serde_json::Value]) {
    let mut f = std::fs::OpenOptions::new().append(true).open(file).unwrap();
    for value in values {
        use std::io::Write;
        writeln!(f, "{}", serde_json::to_string(value).unwrap()).unwrap();
    }
    drop(f);
}

fn assert_has_user(trace: &cash::ir::Trace, text: &str) {
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::UserMessage { text: t } if t == text)),
        "missing user message: {text}"
    );
}

fn assert_has_assistant(trace: &cash::ir::Trace, text: &str) {
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::AssistantMessage { text: t } if t == text)),
        "missing assistant message: {text}"
    );
}

fn count_jsonl(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                count_jsonl(&path)
            } else {
                usize::from(path.extension().and_then(|s| s.to_str()) == Some("jsonl"))
            }
        })
        .sum()
}

fn create_schema(db: &std::path::Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, workspace_id TEXT,
            parent_id TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL, path TEXT,
            title TEXT NOT NULL, version TEXT NOT NULL, share_url TEXT,
            summary_additions INTEGER, summary_deletions INTEGER, summary_files INTEGER,
            summary_diffs TEXT, metadata TEXT, cost REAL DEFAULT 0 NOT NULL,
            tokens_input INTEGER DEFAULT 0, tokens_output INTEGER DEFAULT 0,
            tokens_reasoning INTEGER DEFAULT 0, tokens_cache_read INTEGER DEFAULT 0,
            tokens_cache_write INTEGER DEFAULT 0, revert TEXT, permission TEXT,
            agent TEXT, model TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
            time_compacting INTEGER, time_archived INTEGER
        );
        CREATE TABLE project (
            id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT, name TEXT,
            icon_url TEXT, icon_url_override TEXT, icon_color TEXT,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
            time_initialized INTEGER, sandboxes TEXT NOT NULL, commands TEXT
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL
        );
        CREATE TABLE part (
            id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL
        );
        -- v2 event-sourced tables (OpenCode >= 1.18): the importer writes the
        -- durable event log and the session_message projection, and the event
        -- foreign key enforces that event_sequence exists before any event.
        CREATE TABLE event_sequence (
            aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT
        );
        CREATE TABLE event (
            id TEXT PRIMARY KEY,
            aggregate_id TEXT NOT NULL REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE,
            seq INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL
        );
        CREATE TABLE session_message (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            type TEXT NOT NULL, seq INTEGER NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL
        );
        CREATE TABLE session_input (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            prompt TEXT NOT NULL, delivery TEXT NOT NULL,
            admitted_seq INTEGER NOT NULL, promoted_seq INTEGER, time_created INTEGER NOT NULL
        );
        CREATE TABLE session_context_epoch (
            session_id TEXT PRIMARY KEY,
            baseline TEXT NOT NULL, snapshot TEXT NOT NULL,
            baseline_seq INTEGER NOT NULL, agent TEXT DEFAULT 'build' NOT NULL
        );
        CREATE INDEX session_message_session_seq_idx ON session_message (session_id, seq);
        CREATE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);",
    )
    .unwrap();
    conn.execute("INSERT INTO project (id, worktree, name, time_created, time_updated, sandboxes) VALUES ('global', '/', '', 0, 0, '[]')", [])
        .unwrap();
}
