pub mod codex;
pub mod opencode;
pub mod pi;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ir::{AgentKind, Trace};

#[derive(Debug, Clone, Copy)]
pub enum SessionTimeKind {
    Started,
    Updated,
}

impl SessionTimeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Started => "Started",
            Self::Updated => "Updated",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Native ID accepted by `cash export` and `cash convert`.
    pub session_id: String,
    pub time: Option<i64>,
    pub time_kind: SessionTimeKind,
    pub cwd: Option<String>,
    /// Native title, or the first user message for agents without titles.
    pub title: Option<String>,
}

pub(crate) fn sort_session_summaries(summaries: &mut [SessionSummary]) {
    summaries.sort_by(|a, b| {
        b.time
            .cmp(&a.time)
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
}

/// Parse JSONL records until `visit` reports that all summary fields were found.
pub(crate) fn scan_jsonl_until(
    path: &Path,
    mut visit: impl FnMut(&Value) -> bool,
) -> Result<(), String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_str(&line)
            .map_err(|e| format!("{}:{}: bad json: {e}", path.display(), i + 1))?;
        if visit(&value) {
            break;
        }
    }
    Ok(())
}

/// Read a trace from a given agent source. `session` is a native session ID,
/// a legacy Codex/Pi file stem, or an existing file path. `sessions_root` /
/// `db_path` allow overriding the default storage location (useful for testing).
pub fn read_trace(kind: AgentKind, session: &str, root: &Path, db: &Path) -> Result<Trace, String> {
    match kind {
        AgentKind::Codex => {
            let path = resolve_session_file(kind, session, root)?;
            codex::read(&path)
        }
        AgentKind::Pi => {
            let path = resolve_session_file(kind, session, root)?;
            pi::read(&path)
        }
        AgentKind::OpenCode => opencode::read(db, session),
    }
}

fn resolve_session_file(
    kind: AgentKind,
    session: &str,
    root: &Path,
) -> Result<std::path::PathBuf, String> {
    let direct = std::path::Path::new(session);
    if direct.exists() && direct.is_file() {
        return Ok(direct.to_path_buf());
    }
    match kind {
        AgentKind::Codex => resolve_native_file_session(
            codex::list_sessions(root)?,
            session,
            "codex",
            codex::native_session_id,
        )?
        .ok_or_else(|| format!("codex session {session} not found under {}", root.display())),
        AgentKind::Pi => resolve_native_file_session(
            pi::list_sessions(root)?,
            session,
            "pi",
            pi::native_session_id,
        )?
        .ok_or_else(|| format!("pi session {session} not found under {}", root.display())),
        AgentKind::OpenCode => Err("opencode session must be a uuid".into()),
    }
}

fn resolve_native_file_session(
    sessions: Vec<(String, PathBuf)>,
    session: &str,
    agent: &str,
    native_session_id: impl Fn(&Path) -> Result<Option<String>, String>,
) -> Result<Option<PathBuf>, String> {
    if let Some((_, path)) = sessions.iter().find(|(stem, _)| stem == session) {
        return Ok(Some(path.clone()));
    }

    let mut matched: Option<PathBuf> = None;
    for (_, path) in sessions {
        let Ok(Some(id)) = native_session_id(&path) else {
            continue;
        };
        if id != session {
            continue;
        }
        if let Some(previous) = matched {
            return Err(format!(
                "{agent} session ID {session} is ambiguous: {} and {}",
                previous.display(),
                path.display()
            ));
        }
        matched = Some(path);
    }
    Ok(matched)
}
