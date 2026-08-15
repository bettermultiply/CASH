use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::Value;

use crate::export::Manifest;
use crate::ir::AgentKind;
use crate::readers;

pub struct SourceStatus {
    pub file_unchanged: bool,
    pub events_unchanged: bool,
    pub detail: String,
}

pub struct TargetStatus {
    pub session_present: bool,
    pub anchor_present: bool,
    pub continued_past_seed: bool,
    pub extra_messages: usize,
    pub detail: String,
}

pub struct StatusReport {
    pub source: SourceStatus,
    pub target: TargetStatus,
}

pub fn check(manifest: &Manifest, opencode_db: &Path) -> Result<StatusReport, String> {
    let kind: AgentKind = manifest
        .source
        .agent
        .parse()
        .map_err(|e: String| format!("unknown agent: {e}"))?;

    // --- Source ---
    let source_detail;
    let (file_unchanged, events_unchanged) = match kind {
        AgentKind::Codex | AgentKind::Pi => {
            let path = Path::new(&manifest.source.file);
            if !path.exists() {
                source_detail = format!("source file missing: {}", path.display());
                (false, false)
            } else {
                let file_hash = crate::util::sha256_file(path)?;
                let file_unchanged = file_hash == manifest.source.file_sha256;
                let events_unchanged =
                    recheck_events(kind, &manifest.source.file, &manifest.source.events_sha256);
                let mut detail = format!("source file: {}", path.display());
                if !file_unchanged {
                    detail.push_str("  [CHANGED]");
                }
                source_detail = detail;
                (file_unchanged, events_unchanged)
            }
        }
        AgentKind::OpenCode => {
            let (db, _sid) = split_db_session(&manifest.source.file);
            let db = Path::new(db);
            let hash = opencode_session_hash(db, &manifest.source.session_id)?;
            let unchanged = hash == manifest.source.events_sha256;
            let mut detail = format!(
                "opencode source: {} ({})",
                manifest.source.session_id,
                db.display()
            );
            if !unchanged {
                detail.push_str("  [CHANGED]");
            }
            source_detail = detail;
            (unchanged, unchanged)
        }
    };

    // --- Target ---
    let target_status = match &manifest.target {
        None => TargetStatus {
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            detail: "no target seeded yet (run `import opencode`)".into(),
        },
        Some(target) => check_target(target, opencode_db),
    };

    Ok(StatusReport {
        source: SourceStatus {
            file_unchanged,
            events_unchanged,
            detail: source_detail,
        },
        target: target_status,
    })
}

fn check_target(target: &crate::export::TargetRef, db: &Path) -> TargetStatus {
    match target.agent.as_str() {
        "opencode" => check_opencode_target(target, db),
        "pi" => check_pi_target(target),
        _ => TargetStatus {
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            detail: format!("target agent {} not supported by status yet", target.agent),
        },
    }
}

fn check_opencode_target(target: &crate::export::TargetRef, db: &Path) -> TargetStatus {
    let conn = match Connection::open(db) {
        Ok(c) => c,
        Err(e) => {
            return TargetStatus {
                session_present: false,
                anchor_present: false,
                continued_past_seed: false,
                extra_messages: 0,
                detail: format!("open {}: {e}", db.display()),
            };
        }
    };
    let session_present = conn
        .query_row(
            "SELECT 1 FROM session WHERE id = ?1",
            [&target.session_id],
            |_| Ok(()),
        )
        .is_ok();
    if !session_present {
        return TargetStatus {
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            detail: format!(
                "target session {} missing (was it deleted or replaced?)",
                target.session_id
            ),
        };
    }
    let anchor_created: Option<i64> = conn
        .query_row(
            "SELECT time_created FROM message WHERE id = ?1 AND session_id = ?2",
            params![&target.anchor_message_id, &target.session_id],
            |r| r.get(0),
        )
        .ok();
    let anchor_present = anchor_created.is_some();
    let extra_messages = match anchor_created {
        Some(t) => conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1 AND time_created > ?2",
                params![&target.session_id, t],
                |r| r.get(0),
            )
            .unwrap_or(0),
        None => 0,
    };
    let continued = anchor_present && extra_messages > 0;
    TargetStatus {
        session_present,
        anchor_present,
        continued_past_seed: continued,
        extra_messages,
        detail: if continued {
            format!("target continued past seed point (+{extra_messages} messages after anchor)")
        } else if anchor_present {
            "target is at the seed point (no new messages)".into()
        } else {
            "anchor message missing in target".into()
        },
    }
}

fn check_pi_target(target: &crate::export::TargetRef) -> TargetStatus {
    let path = Path::new(&target.file);
    if target.file.is_empty() || !path.exists() {
        return TargetStatus {
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            detail: format!("target pi session file missing: {}", target.file),
        };
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            return TargetStatus {
                session_present: false,
                anchor_present: false,
                continued_past_seed: false,
                extra_messages: 0,
                detail: format!("read {}: {e}", path.display()),
            };
        }
    };

    let mut anchor_present = false;
    let mut after_anchor = false;
    let mut extra_messages = 0usize;
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if after_anchor && v.get("type").and_then(Value::as_str) != Some("session") {
            extra_messages += 1;
        }
        if v.get("id").and_then(Value::as_str) == Some(target.anchor_message_id.as_str()) {
            anchor_present = true;
            after_anchor = true;
        }
    }

    let events_match = readers::pi::read(path)
        .map(|trace| trace.meta.events_sha256 == target.events_sha256)
        .unwrap_or(false);
    let continued = anchor_present && extra_messages > 0;
    TargetStatus {
        session_present: true,
        anchor_present,
        continued_past_seed: continued,
        extra_messages,
        detail: if continued {
            format!(
                "target pi session continued past seed point (+{extra_messages} records after anchor)"
            )
        } else if anchor_present && events_match {
            "target pi session is at the seed point (event hash matches)".into()
        } else if anchor_present {
            "target pi session is at the seed point (event hash differs after native normalization)"
                .into()
        } else {
            "target pi anchor record missing".into()
        },
    }
}

fn recheck_events(kind: AgentKind, file: &str, expected: &str) -> bool {
    let trace = match kind {
        AgentKind::Codex => readers::codex::read(Path::new(file)),
        AgentKind::Pi => readers::pi::read(Path::new(file)),
        _ => return false,
    };
    match trace {
        Ok(t) => t.meta.events_sha256 == expected,
        Err(_) => false,
    }
}

fn opencode_session_hash(db: &Path, session_id: &str) -> Result<String, String> {
    let trace = readers::opencode::read(db, session_id)?;
    Ok(trace.meta.events_sha256)
}

fn split_db_session(s: &str) -> (&str, &str) {
    // stored as "<db_path>(<session_id>)"
    match s.rfind('(') {
        Some(idx) if s.ends_with(')') => (&s[..idx], &s[idx + 1..s.len() - 1]),
        _ => (s, ""),
    }
}
