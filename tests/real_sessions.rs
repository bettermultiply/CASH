use std::path::{Path, PathBuf};

use cash::ir::EventKind;
use cash::{config, import, readers};

/// Copy an OpenCode store consistently. The live DB may keep data in the WAL,
/// so a plain file copy is unreliable; `VACUUM INTO` produces a clean snapshot.
fn copy_opencode_db(src: &std::path::Path, dst: &std::path::Path) {
    let conn = rusqlite::Connection::open(src).expect("open source opencode db");
    conn.execute_batch(&format!(
        "VACUUM INTO '{}'",
        dst.to_string_lossy().replace('\'', "''")
    ))
    .expect("vacuum into destination db");
    drop(conn);
}

#[test]
#[ignore = "reads local agent histories; writes only temp copies / temp roots"]
fn real_pi_and_opencode_sessions_smoke() {
    let home = config::home_dir();
    let pi_root = home.join(".pi/agent/sessions");
    let opencode_db = std::env::var("CASH_REAL_OPENCODE_DB")
        .or_else(|_| std::env::var("MIGRATE_REAL_OPENCODE_DB"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".local/share/opencode/opencode.db"));

    let pi_path = pick_pi_session(&pi_root);
    let tmp = std::env::temp_dir().join(format!("cash-real-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp).unwrap();

    let pi_trace = readers::pi::read(&pi_path).expect("read real pi session");
    assert!(pi_trace.events.len() > 5);

    let pi_to_pi = import::pi::import(&pi_trace, &tmp.join("pi-root")).expect("pi -> pi");
    let pi_back = readers::pi::read(Path::new(&pi_to_pi.file)).expect("read imported pi");
    assert_eq!(pi_back.events.len(), pi_trace.events.len());
    assert_eq!(pi_back.meta.events_sha256, pi_trace.meta.events_sha256);

    let copied_db = tmp.join("opencode.db");
    copy_opencode_db(&opencode_db, &copied_db);
    let pi_to_open = import::opencode::import(&pi_trace, &copied_db).expect("pi -> opencode copy");
    let open_back = readers::opencode::read(&copied_db, &pi_to_open.session_id)
        .expect("read imported opencode");
    assert!(!open_back.events.is_empty());
    assert!(open_back.events.len() <= pi_trace.events.len());

    let open_session = pick_opencode_session(&copied_db);
    let open_trace =
        readers::opencode::read(&copied_db, &open_session).expect("read real opencode session");
    assert!(open_trace.events.len() > 5);
    let open_to_pi =
        import::pi::import(&open_trace, &tmp.join("pi-root2")).expect("opencode -> pi");
    let open_pi_back =
        readers::pi::read(Path::new(&open_to_pi.file)).expect("read opencode imported to pi");
    // Cross-agent materialization is best-effort; require the handoff-critical
    // content to survive: the same user prompts, and a non-empty session.
    assert!(!open_pi_back.events.is_empty());
    let user_texts: Vec<String> = open_trace
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::UserMessage { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(!user_texts.is_empty());
    for text in &user_texts {
        assert!(
            open_pi_back
                .events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::UserMessage { text: t } if t == text)),
            "user prompt lost in opencode -> pi conversion"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

fn pick_pi_session(root: &Path) -> PathBuf {
    if let Ok(v) =
        std::env::var("CASH_REAL_PI_SESSION").or_else(|_| std::env::var("MIGRATE_REAL_PI_SESSION"))
    {
        let direct = PathBuf::from(&v);
        if direct.exists() {
            return direct;
        }
        return readers::pi::list_sessions(root)
            .unwrap()
            .into_iter()
            .find(|(id, _)| id == &v)
            .map(|(_, path)| path)
            .expect("CASH_REAL_PI_SESSION not found");
    }
    readers::pi::list_sessions(root)
        .unwrap()
        .into_iter()
        .map(|(_, path)| path)
        .find(|path| {
            readers::pi::read(path)
                .map(|t| t.events.len() > 5)
                .unwrap_or(false)
        })
        .expect("no suitable real pi session found")
}

fn pick_opencode_session(db: &Path) -> String {
    if let Ok(v) = std::env::var("CASH_REAL_OPENCODE_SESSION")
        .or_else(|_| std::env::var("MIGRATE_REAL_OPENCODE_SESSION"))
    {
        return v;
    }
    readers::opencode::list_sessions(db)
        .unwrap()
        .into_iter()
        .map(|(id, _, _, _)| id)
        .find(|id| {
            readers::opencode::read(db, id)
                .map(|t| t.events.len() > 5)
                .unwrap_or(false)
        })
        .expect("no suitable real opencode session found")
}
