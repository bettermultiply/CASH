pub mod codex;
pub mod opencode;
pub mod pi;

use std::path::Path;

use crate::ir::{AgentKind, Trace};

/// Read a trace from a given agent source. `session` is either the id
/// (for opencode: session uuid; for codex/pi: rollout file stem) or an
/// existing file path. `sessions_root` / `db_path` allow overriding the
/// default storage location (useful for testing).
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
        AgentKind::Codex => codex::list_sessions(root)?
            .into_iter()
            .find(|(stem, _)| stem == session)
            .map(|(_, p)| p)
            .ok_or_else(|| format!("codex session {session} not found under {}", root.display())),
        AgentKind::Pi => pi::list_sessions(root)?
            .into_iter()
            .find(|(stem, _)| stem == session)
            .map(|(_, p)| p)
            .ok_or_else(|| format!("pi session {session} not found under {}", root.display())),
        AgentKind::OpenCode => Err("opencode session must be a uuid".into()),
    }
}
