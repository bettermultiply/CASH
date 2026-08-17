use std::path::{Path, PathBuf};

use cash::ir::{Event, EventKind, Trace};
use cash::{config, import, readers};

/// Copy an OpenCode store consistently. The live DB may keep data in the WAL,
/// so a plain file copy is unreliable; `VACUUM INTO` produces a clean snapshot.
fn copy_opencode_db(src: &Path, dst: &Path) {
    let conn = rusqlite::Connection::open(src).expect("open source opencode db");
    conn.execute_batch(&format!(
        "VACUUM INTO '{}'",
        dst.to_string_lossy().replace('\'', "''")
    ))
    .expect("vacuum into destination db");
    drop(conn);
}

/// Real-data smoke across all three agents. For each agent, one real session is
/// read from the local histories and verified in two ways:
/// - same-agent round trip stays lossless (per each agent's standard), and
/// - every real user prompt survives conversion into each of the other two
///   agents (the handoff-critical content).
///
/// Any leg whose agent has no suitable local session is skipped with a notice.
#[test]
#[ignore = "reads local agent histories; writes only temp copies / temp roots"]
fn real_sessions_smoke_across_all_agents() {
    let home = config::home_dir();
    let pi_root = home.join(".pi/agent/sessions");
    let codex_root = home.join(".codex/sessions");
    let opencode_db = std::env::var("CASH_REAL_OPENCODE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".local/share/opencode/opencode.db"));

    let tmp = std::env::temp_dir().join(format!("cash-real-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp).unwrap();
    // One consistent copy of the real OpenCode store; every opencode-target
    // import adds a fresh session to it.
    let oc_copy = tmp.join("opencode.db");
    copy_opencode_db(&opencode_db, &oc_copy);

    if let Some(trace) = real_pi_trace(&pi_root) {
        same_agent_round_trip(&trace, "pi", &tmp, &oc_copy);
        for target in ["opencode", "codex"] {
            assert_prompts_survive(&trace, target, &tmp, &oc_copy);
        }
    } else {
        eprintln!("no suitable real pi session found; skipping pi leg");
    }

    if let Some(trace) = real_opencode_trace(&oc_copy) {
        same_agent_round_trip(&trace, "opencode", &tmp, &oc_copy);
        for target in ["pi", "codex"] {
            assert_prompts_survive(&trace, target, &tmp, &oc_copy);
        }
    } else {
        eprintln!("no suitable real opencode session found; skipping opencode leg");
    }

    if let Some(trace) = real_codex_trace(&codex_root) {
        same_agent_round_trip(&trace, "codex", &tmp, &oc_copy);
        for target in ["pi", "opencode"] {
            assert_prompts_survive(&trace, target, &tmp, &oc_copy);
        }
    } else {
        eprintln!("no suitable real codex session found; skipping codex leg");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

fn real_pi_trace(pi_root: &Path) -> Option<Trace> {
    let path = if let Ok(v) = std::env::var("CASH_REAL_PI_SESSION") {
        let direct = PathBuf::from(&v);
        if direct.exists() {
            return readers::pi::read(&direct).ok();
        }
        readers::pi::list_sessions(pi_root)
            .ok()?
            .into_iter()
            .find(|(id, _)| id == &v)
            .map(|(_, path)| path)?
    } else {
        readers::pi::list_sessions(pi_root)
            .ok()?
            .into_iter()
            .map(|(_, path)| path)
            .find(|path| {
                readers::pi::read(path)
                    .map(|t| t.events.len() > 5)
                    .unwrap_or(false)
            })?
    };
    readers::pi::read(&path).ok()
}

fn real_opencode_trace(db: &Path) -> Option<Trace> {
    let id = if let Ok(v) = std::env::var("CASH_REAL_OPENCODE_SESSION") {
        v
    } else {
        readers::opencode::list_session_summaries(db)
            .ok()?
            .into_iter()
            .map(|summary| summary.session_id)
            .find(|id| {
                readers::opencode::read(db, id)
                    .map(|t| t.events.len() > 5)
                    .unwrap_or(false)
            })?
    };
    readers::opencode::read(db, &id).ok()
}

fn real_codex_trace(codex_root: &Path) -> Option<Trace> {
    let path = if let Ok(v) = std::env::var("CASH_REAL_CODEX_SESSION") {
        let direct = PathBuf::from(&v);
        if direct.exists() {
            return readers::codex::read(&direct).ok();
        }
        readers::codex::list_sessions(codex_root)
            .ok()?
            .into_iter()
            .find(|(stem, _)| stem == &v)
            .map(|(_, path)| path)?
    } else {
        readers::codex::list_sessions(codex_root)
            .ok()?
            .into_iter()
            .map(|(_, path)| path)
            .find(|path| {
                readers::codex::read(path)
                    .map(|t| t.events.len() > 5)
                    .unwrap_or(false)
            })?
    };
    readers::codex::read(&path).ok()
}

/// Same-agent round trip on a real session, checked lossless per the agent's
/// own standard (pi/codex strict event equality, opencode content equality).
fn same_agent_round_trip(trace: &Trace, agent: &str, tmp: &Path, oc_copy: &Path) {
    match agent {
        "pi" => {
            let result = import::pi::import(trace, &tmp.join("rt-pi")).expect("pi -> pi");
            let back = readers::pi::read(Path::new(&result.file)).expect("re-read pi");
            assert_eq!(
                back.meta.events_sha256, trace.meta.events_sha256,
                "pi -> pi round trip on real data changed the trace"
            );
        }
        "codex" => {
            let result = import::codex::import(trace, &tmp.join("rt-codex")).expect("codex -> codex");
            let back = readers::codex::read(Path::new(&result.file)).expect("re-read codex");
            assert_eq!(
                serde_json::to_string(&trace.events).unwrap(),
                serde_json::to_string(&back.events).unwrap(),
                "codex -> codex round trip on real data changed the trace"
            );
        }
        "opencode" => {
            let result = import::opencode::import(trace, oc_copy).expect("opencode -> opencode");
            let back = readers::opencode::read(oc_copy, &result.session_id).expect("re-read opencode");
            assert_eq!(
                strip_identity(&trace.events),
                strip_identity(&back.events),
                "opencode -> opencode round trip on real data changed the content"
            );
        }
        other => panic!("unknown agent {other}"),
    }
}

/// Convert a real source session into `target` and require that every real
/// user prompt survives (Codex-injected context is not a real prompt).
fn assert_prompts_survive(source: &Trace, target: &str, tmp: &Path, oc_copy: &Path) {
    let user_texts: Vec<&String> = source
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::UserMessage { text } => Some(text),
            _ => None,
        })
        .filter(|text| !is_injected_context(text))
        .collect();
    assert!(!user_texts.is_empty(), "source has no user prompts");

    let back = match target {
        "pi" => {
            let result = import::pi::import(source, &tmp.join("pi")).expect("import pi");
            readers::pi::read(Path::new(&result.file)).expect("re-read pi")
        }
        "opencode" => {
            let result = import::opencode::import(source, oc_copy).expect("import opencode");
            readers::opencode::read(oc_copy, &result.session_id).expect("re-read opencode")
        }
        "codex" => {
            let result = import::codex::import(source, &tmp.join("codex")).expect("import codex");
            readers::codex::read(Path::new(&result.file)).expect("re-read codex")
        }
        other => panic!("unknown target {other}"),
    };
    for text in user_texts {
        assert!(
            back.events.iter().any(|e| matches!(&e.kind, EventKind::UserMessage { text: t } if t == text)),
            "user prompt {text:?} lost in real {target} conversion"
        );
    }
}

fn is_injected_context(text: &str) -> bool {
    let text = text.trim_start();
    [
        "<environment_context>",
        "<permissions instructions>",
        "<collaboration_mode>",
        "<plugins_instructions>",
        "<skills_instructions>",
        "<user_instructions>",
        "# AGENTS.md instructions",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

/// OpenCode regenerates message ids on import, so strict event equality is
/// impossible; content fidelity is the guarantee. Strip the identity/metadata
/// fields and compare the event payloads and ordering.
fn strip_identity(events: &[Event]) -> String {
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
}
