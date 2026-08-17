use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::Value;

use crate::export::{Manifest, NodeRef};
use crate::readers;

/// 组内单个副本的状态。所有副本对等报告，无 source/target 之分。
pub struct NodeStatus {
    pub agent: String,
    pub session_id: String,
    pub session_present: bool,
    pub anchor_present: bool,
    pub continued_past_seed: bool,
    pub extra_messages: usize,
    pub file_unchanged: bool,
    pub events_unchanged: bool,
    pub detail: String,
}

pub struct StatusReport {
    pub nodes: Vec<NodeStatus>,
}

pub fn check(manifest: &Manifest, opencode_db: &Path) -> Result<StatusReport, String> {
    let mut nodes = Vec::new();
    for node in manifest.copies() {
        nodes.push(check_node(&node, opencode_db));
    }
    Ok(StatusReport { nodes })
}

fn check_node(node: &NodeRef, db: &Path) -> NodeStatus {
    match node.agent.as_str() {
        "opencode" => check_opencode_node(node, db),
        "pi" => check_pi_node(node),
        "codex" => check_codex_node(node),
        _ => NodeStatus {
            agent: node.agent.clone(),
            session_id: node.session_id.clone(),
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            file_unchanged: false,
            events_unchanged: false,
            detail: format!("unknown agent {}", node.agent),
        },
    }
}

fn check_opencode_node(node: &NodeRef, db: &Path) -> NodeStatus {
    let conn = match Connection::open(db) {
        Ok(c) => c,
        Err(e) => {
            return NodeStatus {
                agent: node.agent.clone(),
                session_id: node.session_id.clone(),
                session_present: false,
                anchor_present: false,
                continued_past_seed: false,
                extra_messages: 0,
                file_unchanged: false,
                events_unchanged: false,
                detail: format!("open {}: {e}", db.display()),
            };
        }
    };
    let session_present = conn
        .query_row(
            "SELECT 1 FROM session WHERE id = ?1",
            [&node.session_id],
            |_| Ok(()),
        )
        .is_ok();
    if !session_present {
        return NodeStatus {
            agent: node.agent.clone(),
            session_id: node.session_id.clone(),
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            file_unchanged: false,
            events_unchanged: false,
            detail: format!(
                "session {} missing (was it deleted or replaced?)",
                node.session_id
            ),
        };
    }
    let anchor_created: Option<i64> = conn
        .query_row(
            "SELECT time_created FROM message WHERE id = ?1 AND session_id = ?2",
            params![&node.anchor_message_id, &node.session_id],
            |r| r.get(0),
        )
        .ok();
    let anchor_present = anchor_created.is_some();
    let extra_messages = match anchor_created {
        Some(t) => conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1 AND time_created > ?2",
                params![&node.session_id, t],
                |r| r.get(0),
            )
            .unwrap_or(0),
        None => 0,
    };
    let continued = anchor_present && extra_messages > 0;
    let events_unchanged = opencode_session_hash(db, &node.session_id)
        .map(|hash| hash == node.events_sha256)
        .unwrap_or(false);
    NodeStatus {
        agent: node.agent.clone(),
        session_id: node.session_id.clone(),
        session_present,
        anchor_present,
        continued_past_seed: continued,
        extra_messages,
        file_unchanged: events_unchanged,
        events_unchanged,
        detail: if continued {
            format!("continued past seed point (+{extra_messages} messages after anchor)")
        } else if anchor_present {
            "at the seed point (no new messages)".into()
        } else {
            "anchor message missing".into()
        },
    }
}

fn check_pi_node(node: &NodeRef) -> NodeStatus {
    let path = Path::new(&node.file);
    if node.file.is_empty() || !path.exists() {
        return NodeStatus {
            agent: node.agent.clone(),
            session_id: node.session_id.clone(),
            session_present: false,
            anchor_present: false,
            continued_past_seed: false,
            extra_messages: 0,
            file_unchanged: false,
            events_unchanged: false,
            detail: format!("pi session file missing: {}", node.file),
        };
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            return NodeStatus {
                agent: node.agent.clone(),
                session_id: node.session_id.clone(),
                session_present: false,
                anchor_present: false,
                continued_past_seed: false,
                extra_messages: 0,
                file_unchanged: false,
                events_unchanged: false,
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
        if v.get("id").and_then(Value::as_str) == Some(node.anchor_message_id.as_str()) {
            anchor_present = true;
            after_anchor = true;
        }
    }

    let events_unchanged = readers::pi::read(path)
        .map(|trace| trace.meta.events_sha256 == node.events_sha256)
        .unwrap_or(false);
    let file_unchanged = if node.file_sha256.is_empty() {
        events_unchanged
    } else {
        crate::util::sha256_file(path)
            .map(|hash| hash == node.file_sha256)
            .unwrap_or(false)
    };
    let continued = anchor_present && extra_messages > 0;
    NodeStatus {
        agent: node.agent.clone(),
        session_id: node.session_id.clone(),
        session_present: true,
        anchor_present,
        continued_past_seed: continued,
        extra_messages,
        file_unchanged,
        events_unchanged,
        detail: if continued {
            format!("continued past seed point (+{extra_messages} records after anchor)")
        } else if anchor_present && events_unchanged {
            "at the seed point (event hash matches)".into()
        } else if anchor_present {
            "at the seed point (event hash differs after native normalization)".into()
        } else {
            "anchor record missing".into()
        },
    }
}

fn check_codex_node(node: &NodeRef) -> NodeStatus {
    let path = Path::new(&node.file);
    let trace = match readers::codex::read(path) {
        Ok(trace) => trace,
        Err(e) => {
            return NodeStatus {
                agent: node.agent.clone(),
                session_id: node.session_id.clone(),
                session_present: false,
                anchor_present: false,
                continued_past_seed: false,
                extra_messages: 0,
                file_unchanged: false,
                events_unchanged: false,
                detail: e,
            };
        }
    };
    let source_anchor = readers::codex::source_id_for_response_item_id(&node.anchor_message_id);
    let anchor_index = trace.events.iter().rposition(|event| {
        event.original_id == node.anchor_message_id || event.original_id == source_anchor
    });
    let anchor_present = anchor_index.is_some();
    let extra_messages = anchor_index
        .map(|index| trace.events.len().saturating_sub(index + 1))
        .unwrap_or(0);
    let continued = anchor_present && extra_messages > 0;
    let events_unchanged = trace.meta.events_sha256 == node.events_sha256;
    let file_unchanged = if node.file_sha256.is_empty() {
        events_unchanged
    } else {
        crate::util::sha256_file(path)
            .map(|hash| hash == node.file_sha256)
            .unwrap_or(false)
    };
    NodeStatus {
        agent: node.agent.clone(),
        session_id: node.session_id.clone(),
        session_present: true,
        anchor_present,
        continued_past_seed: continued,
        extra_messages,
        file_unchanged,
        events_unchanged,
        detail: if continued {
            format!("continued past seed point (+{extra_messages} events)")
        } else if anchor_present && events_unchanged {
            "at the seed point (event hash matches)".into()
        } else if anchor_present {
            "at the seed point (event hash differs after native normalization)".into()
        } else {
            "anchor record missing".into()
        },
    }
}

fn opencode_session_hash(db: &Path, session_id: &str) -> Result<String, String> {
    let trace = readers::opencode::read(db, session_id)?;
    Ok(trace.meta.events_sha256)
}
