use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use cash::{config, ir::EventKind, readers};
use rusqlite::Connection;
use serde_json::{Value, json};

fn main() -> Result<(), String> {
    let out = std::env::args()
        .skip_while(|arg| arg != "--out")
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/real"));
    std::fs::create_dir_all(&out).map_err(|e| format!("mkdir {}: {e}", out.display()))?;

    let home = config::home_dir();
    let pi_root = home.join(".pi/agent/sessions");
    let codex_root = home.join(".codex/sessions");
    let opencode_db = home.join(".local/share/opencode/opencode.db");

    let pi_path =
        env_path("CASH_FIXTURE_PI_SESSION", "MIGRATE_FIXTURE_PI_SESSION").unwrap_or_else(|| {
            pick_jsonl(&pi_root, &["toolCall", "toolResult", "thinking"]).expect("pick pi session")
        });
    sanitize_jsonl(&pi_path, &out.join("pi_real_sanitized.jsonl"))?;

    let codex_path = env_path(
        "CASH_FIXTURE_CODEX_SESSION",
        "MIGRATE_FIXTURE_CODEX_SESSION",
    )
    .unwrap_or_else(|| {
        pick_jsonl(&codex_root, &["function_call", "function_call_output"])
            .expect("pick codex session")
    });
    sanitize_jsonl(&codex_path, &out.join("codex_real_sanitized.jsonl"))?;

    let opencode_session = env_value(
        "CASH_FIXTURE_OPENCODE_SESSION",
        "MIGRATE_FIXTURE_OPENCODE_SESSION",
    )
    .unwrap_or_else(|| pick_opencode_session(&opencode_db).expect("pick opencode session"));
    sanitize_opencode_sql(
        &opencode_db,
        &opencode_session,
        &out.join("opencode_real_sanitized.sql"),
    )?;

    println!("wrote sanitized fixtures to {}", out.display());
    Ok(())
}

fn env_path(name: &str, legacy: &str) -> Option<PathBuf> {
    env_value(name, legacy)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn env_value(name: &str, legacy: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| std::env::var(legacy).ok())
}

fn pick_jsonl(root: &Path, needles: &[&str]) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let raw = std::fs::read_to_string(&path).ok()?;
                if needles.iter().all(|needle| raw.contains(needle)) {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn pick_opencode_session(db: &Path) -> Result<String, String> {
    for (id, _, _, _) in readers::opencode::list_sessions(db)? {
        let trace = readers::opencode::read(db, &id)?;
        let has_tool = trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolCall { .. }));
        let has_reasoning = trace
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::Reasoning { .. }));
        if trace.events.len() > 20 && has_tool && has_reasoning {
            return Ok(id);
        }
    }
    Err("no suitable opencode session found".into())
}

fn sanitize_jsonl(input: &Path, output: &Path) -> Result<(), String> {
    let raw =
        std::fs::read_to_string(input).map_err(|e| format!("read {}: {e}", input.display()))?;
    let mut sanitizer = Sanitizer::default();
    let mut out =
        std::fs::File::create(output).map_err(|e| format!("create {}: {e}", output.display()))?;
    for line in raw.lines() {
        let mut value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        sanitizer.sanitize_value(None, &mut value);
        serde_json::to_writer(&mut out, &value).map_err(|e| e.to_string())?;
        out.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn sanitize_opencode_sql(db: &Path, session_id: &str, output: &Path) -> Result<(), String> {
    let conn = Connection::open(db).map_err(|e| format!("open {}: {e}", db.display()))?;
    let mut sanitizer = Sanitizer::default();
    let mut out = String::new();
    out.push_str("INSERT OR IGNORE INTO project (id, worktree, name, time_created, time_updated, sandboxes) VALUES ('global', '/', '', 0, 0, '[]');\n");
    out.push_str("INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated, agent, model) VALUES ('opencode-real-sanitized', 'global', 'sanitized-fixture', '/workspace/project', 'Sanitized OpenCode Fixture', 'fixture', 0, 0, 'fixture', '{}');\n");

    let mut msg_ids = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT id, data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created, id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for (idx, row) in rows.enumerate() {
        let (msg_id, data, time) = row.map_err(|e| e.to_string())?;
        let new_id = format!("msg_{idx:04}");
        msg_ids.insert(msg_id, new_id.clone());
        let mut data: Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        sanitizer.sanitize_value(None, &mut data);
        out.push_str(&format!(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ({}, 'opencode-real-sanitized', {time}, {time}, {});\n",
            sql_string(&new_id),
            sql_string(&serde_json::to_string(&data).unwrap())
        ));
    }

    let mut stmt = conn
        .prepare("SELECT id, message_id, data, time_created FROM part WHERE session_id = ?1 ORDER BY time_created, id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for (idx, row) in rows.enumerate() {
        let (_, msg_id, data, time) = row.map_err(|e| e.to_string())?;
        let Some(new_msg_id) = msg_ids.get(&msg_id) else {
            continue;
        };
        let part_id = format!("prt_{idx:04}");
        let mut data: Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        sanitizer.sanitize_value(None, &mut data);
        out.push_str(&format!(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES ({}, {}, 'opencode-real-sanitized', {time}, {time}, {});\n",
            sql_string(&part_id),
            sql_string(new_msg_id),
            sql_string(&serde_json::to_string(&data).unwrap())
        ));
    }

    std::fs::write(output, out).map_err(|e| format!("write {}: {e}", output.display()))
}

#[derive(Default)]
struct Sanitizer {
    ids: HashMap<String, String>,
    calls: HashMap<String, String>,
    next_id: usize,
    next_call: usize,
}

impl Sanitizer {
    fn sanitize_value(&mut self, key: Option<&str>, value: &mut Value) {
        match value {
            Value::Object(map) => {
                if key == Some("base_instructions") {
                    *value = json!({"text": "[REDACTED_BASE_INSTRUCTIONS]"});
                    return;
                }
                let keys: Vec<String> = map.keys().cloned().collect();
                for old in keys {
                    let new = sanitize_key(&old);
                    if new != old
                        && let Some(v) = map.remove(&old)
                    {
                        map.insert(new, v);
                    }
                }
                for (k, v) in map.iter_mut() {
                    self.sanitize_value(Some(k), v);
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.sanitize_value(None, item);
                }
            }
            Value::String(s) => {
                if matches!(key, Some("cwd" | "directory" | "worktree")) {
                    *s = "/workspace/project".into();
                } else if matches!(key, Some("id" | "parentId" | "message_id" | "session_id")) {
                    *s = self.id_for(s);
                } else if matches!(key, Some("call_id" | "callID" | "toolCallId")) {
                    *s = self.call_for(s);
                } else if key == Some("arguments") {
                    if let Ok(mut parsed) = serde_json::from_str::<Value>(s) {
                        self.sanitize_value(None, &mut parsed);
                        *s = serde_json::to_string(&parsed).unwrap_or_else(|_| "{}".into());
                    } else {
                        *s = sanitize_text(s);
                    }
                } else if key == Some("encrypted_content") {
                    *s = "[REDACTED_ENCRYPTED_CONTENT]".into();
                } else {
                    *s = sanitize_text(s);
                }
            }
            _ => {}
        }
    }

    fn id_for(&mut self, raw: &str) -> String {
        if raw.is_empty() {
            return String::new();
        }
        if let Some(id) = self.ids.get(raw) {
            return id.clone();
        }
        self.next_id += 1;
        let id = format!("id_{:04}", self.next_id);
        self.ids.insert(raw.into(), id.clone());
        id
    }

    fn call_for(&mut self, raw: &str) -> String {
        if raw.is_empty() {
            return String::new();
        }
        if let Some(id) = self.calls.get(raw) {
            return id.clone();
        }
        self.next_call += 1;
        let id = format!("call_{:04}", self.next_call);
        self.calls.insert(raw.into(), id.clone());
        id
    }
}

fn sanitize_text(input: &str) -> String {
    let mut s = input.to_string();
    if let Ok(home) = std::env::var("HOME") {
        s = s.replace(&home, "/home/user");
        if let Some(name) = Path::new(&home).file_name().and_then(|p| p.to_str()) {
            s = s.replace(name, "user");
        }
    }
    s = s.replace("BETMUL", "WORKSPACE");
    s = s.replace("betmul", "user");
    s = s.replace("sk-", "[REDACTED_API_KEY]-");
    s = redact_long_tokens(&s);
    truncate(&s, 2_000)
}

fn sanitize_key(input: &str) -> String {
    if input.contains("/home/") || input.contains("BETMUL") || input.contains("betmul") {
        sanitize_text(input)
    } else {
        input.into()
    }
}

fn redact_long_tokens(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            let clean =
                token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-');
            if clean.len() > 80
                && clean
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                token.replace(clean, "[REDACTED_TOKEN]")
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.into()
    } else {
        let prefix: String = input.chars().take(max).collect();
        format!("{prefix}\n[TRUNCATED]")
    }
}

fn sql_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
