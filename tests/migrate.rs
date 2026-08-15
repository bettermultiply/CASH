use std::path::PathBuf;
use std::process::Command;

use migrate::export;
use migrate::import;
use migrate::ir::EventKind;
use migrate::readers;

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
fn codex_reader_extracts_events() {
    let trace = readers::codex::read(&fixture("codex.jsonl")).expect("read codex fixture");
    assert_eq!(trace.meta.session_id, "codex-sess-1");
    assert_eq!(trace.meta.cwd.as_deref(), Some("/tmp/work"));
    assert_eq!(trace.meta.source_file_sha256.len(), 64);

    let types: Vec<&str> = trace
        .events
        .iter()
        .map(|e| match &e.kind {
            EventKind::UserMessage { .. } => "user",
            EventKind::AssistantMessage { .. } => "assistant",
            EventKind::Reasoning { .. } => "reasoning",
            EventKind::ToolCall { .. } => "tool_call",
            EventKind::ToolResult { .. } => "tool_result",
            EventKind::ModelChange { .. } => "model_change",
        })
        .collect();
    assert_eq!(
        types,
        vec!["user", "assistant", "tool_call", "tool_result", "reasoning"]
    );
    assert_eq!(trace.meta.event_count, 5);

    // tool call args preserved verbatim
    let tool_call = trace
        .events
        .iter()
        .find(|e| matches!(e.kind, EventKind::ToolCall { .. }))
        .unwrap();
    match &tool_call.kind {
        EventKind::ToolCall {
            id,
            name,
            arguments,
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "bash");
            assert_eq!(arguments, r#"{"cmd":"ls"}"#);
        }
        _ => unreachable!(),
    }
}

#[test]
fn pi_reader_extracts_events() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).expect("read pi fixture");
    assert_eq!(trace.meta.session_id, "pi-sess-1");
    assert_eq!(trace.meta.cwd.as_deref(), Some("/tmp/work"));

    let types: Vec<&str> = trace
        .events
        .iter()
        .map(|e| match &e.kind {
            EventKind::UserMessage { .. } => "user",
            EventKind::AssistantMessage { .. } => "assistant",
            EventKind::Reasoning { .. } => "reasoning",
            EventKind::ToolCall { .. } => "tool_call",
            EventKind::ToolResult { .. } => "tool_result",
            EventKind::ModelChange { .. } => "model_change",
        })
        .collect();
    assert_eq!(
        types,
        vec![
            "model_change",
            "user",
            "reasoning",
            "assistant",
            "tool_call",
            "tool_result"
        ]
    );
    assert_eq!(trace.meta.event_count, 6);

    let tool_result = trace
        .events
        .iter()
        .find(|e| matches!(e.kind, EventKind::ToolResult { .. }))
        .unwrap();
    match &tool_result.kind {
        EventKind::ToolResult {
            call_id, output, ..
        } => {
            assert_eq!(call_id, "call_a");
            assert_eq!(output, "2026-01-01");
        }
        _ => unreachable!(),
    }
}

#[test]
fn export_writes_seed_files_and_manifest() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("migrate-test-{}", uuid::Uuid::new_v4().simple()));
    let manifest = export::write_seed(&trace, &dir).expect("write seed");

    assert!(dir.join("seed.json").exists());
    assert!(dir.join("seed.md").exists());
    assert!(dir.join("manifest.json").exists());
    assert_eq!(manifest.source.agent, "pi");
    assert!(manifest.target.is_none());

    // seed.json round-trips back to an equivalent trace
    let raw = std::fs::read_to_string(dir.join("seed.json")).unwrap();
    let re: migrate::ir::Trace = serde_json::from_str(&raw).unwrap();
    assert_eq!(re.events.len(), trace.events.len());
    assert_eq!(re.meta.events_sha256, trace.meta.events_sha256);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_into_opencode_round_trips() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("migrate-db-{}", uuid::Uuid::new_v4().simple()));
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
    assert_eq!(result.message_count, 4);

    // re-read the imported session: model_change is the only dropped event
    let back = readers::opencode::read(&db, &result.session_id).expect("re-read");
    assert_eq!(back.events.len(), trace.events.len() - 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_into_pi_round_trips_all_events() {
    let trace = readers::pi::read(&fixture("pi.jsonl")).unwrap();
    let dir = std::env::temp_dir().join(format!("migrate-pi-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();

    let result = import::pi::import(&trace, &dir).expect("import pi");
    assert!(std::path::Path::new(&result.file).exists());
    assert_eq!(result.message_count, trace.events.len());
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

    let back = readers::pi::read(std::path::Path::new(&result.file)).expect("re-read pi");
    assert_eq!(back.events.len(), trace.events.len());
    assert_eq!(back.meta.events_sha256, trace.meta.events_sha256);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sanitized_real_pi_fixture_covers_rich_session_shape() {
    let trace = readers::pi::read(&real_fixture("pi_real_sanitized.jsonl")).unwrap();
    assert!(trace.events.len() > 100);
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::Reasoning { .. }))
    );
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolCall { .. }))
    );
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolResult { .. }))
    );
}

#[test]
fn sanitized_real_codex_fixture_covers_modern_tool_shapes() {
    let trace = readers::codex::read(&real_fixture("codex_real_sanitized.jsonl")).unwrap();
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
    assert!(trace.events.len() > 100);
    assert!(tool_calls > 20);
    assert!(tool_results > 20);
}

#[test]
fn sanitized_real_opencode_fixture_parses_sql_session() {
    let dir = std::env::temp_dir().join(format!(
        "migrate-real-sql-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    create_schema(&db);
    let sql = std::fs::read_to_string(real_fixture("opencode_real_sanitized.sql")).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(&sql).unwrap();

    let trace = readers::opencode::read(&db, "opencode-real-sanitized").unwrap();
    assert!(trace.events.len() > 100);
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::Reasoning { .. }))
    );
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolCall { .. }))
    );
    assert!(
        trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolResult { .. }))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn convert_command_updates_same_pi_session_instead_of_duplicating() {
    let env = cli_test_env("idempotent");
    let first = run_convert(&env, false);
    assert!(
        first.status.success(),
        "first convert failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_target = manifest_target(&env.seed);

    let second = run_convert(&env, false);
    assert!(
        second.status.success(),
        "second convert failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_target = manifest_target(&env.seed);

    assert_eq!(second_target["session_id"], first_target["session_id"]);
    assert_eq!(second_target["file"], first_target["file"]);
    assert_eq!(count_jsonl(&env.pi_root), 1);

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
        "migrate-open-update-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("opencode.db");
    create_schema(&db);

    let first = import::opencode::import_existing(&trace, &db, None, None, false).unwrap();
    let second = import::opencode::import_existing(
        &trace,
        &db,
        Some(&first.session_id),
        Some(&first.anchor_message_id),
        false,
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
    );
    assert!(conflict.unwrap_err().contains("--force"));

    let forced = import::opencode::import_existing(
        &trace,
        &db,
        Some(&first.session_id),
        Some(&second.anchor_message_id),
        true,
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

    let _ = std::fs::remove_dir_all(&root);
}

struct CliTestEnv {
    root: PathBuf,
    db: PathBuf,
    seed: PathBuf,
    pi_root: PathBuf,
}

fn cli_test_env(label: &str) -> CliTestEnv {
    let root = std::env::temp_dir().join(format!(
        "migrate-cli-{label}-{}",
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
        seed: root.join("seed"),
        pi_root: root.join("pi-root"),
        root,
        db,
    }
}

fn run_convert(env: &CliTestEnv, force: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_migrate"));
    command.args([
        "convert",
        "opencode",
        "opencode-real-sanitized",
        "pi",
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

fn manifest_target(seed: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(seed.join("manifest.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()["target"].clone()
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
        );",
    )
    .unwrap();
    conn.execute("INSERT INTO project (id, worktree, name, time_created, time_updated, sandboxes) VALUES ('global', '/', '', 0, 0, '[]')", [])
        .unwrap();
}
