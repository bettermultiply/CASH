use std::path::{Path, PathBuf};
use std::process::Command;

use cash::{config, import, readers};

#[test]
#[ignore = "requires opencode CLI; starts native CLI against a temp XDG data dir"]
fn opencode_cli_can_list_and_export_imported_session() {
    if Command::new("opencode").arg("--version").output().is_err() {
        eprintln!("opencode not found; skipping native startup test");
        return;
    }

    let tmp = std::env::temp_dir().join(format!("cash-native-{}", uuid::Uuid::new_v4().simple()));
    let xdg = tmp.join("xdg-data");
    let opencode_dir = xdg.join("opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();

    let source_db = config::home_dir().join(".local/share/opencode/opencode.db");
    let db = opencode_dir.join("opencode.db");
    copy_opencode_db(&source_db, &db);

    let mut trace = readers::pi::read(&fixture("real/pi_real_sanitized.jsonl")).unwrap();
    trace.meta.cwd = Some(
        std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let result = import::opencode::import(&trace, &db).expect("import into temp opencode db");

    let list = opencode(&xdg, ["session", "list"]);
    assert!(
        list.status.success(),
        "session list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    // `opencode session list` may be empty in some CLI/environment states
    // (e.g. when a live opencode server owns the store); the authoritative
    // native-load check is `export`, asserted below.
    if !list_stdout.trim().is_empty() {
        assert!(
            list_stdout.contains(&result.session_id),
            "imported session not visible in opencode session list"
        );
    }

    let exported = opencode(&xdg, ["export", result.session_id.as_str()]);
    assert!(
        exported.status.success(),
        "session export failed: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let exported_stdout = String::from_utf8_lossy(&exported.stdout);
    assert!(
        exported_stdout.contains(&result.session_id),
        "native export did not include imported session id"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore = "requires pi CLI; starts native CLI export against a temp session root"]
fn pi_cli_can_export_imported_session() {
    let pi_bin = std::env::var("CASH_PI_BIN")
        .or_else(|_| std::env::var("MIGRATE_PI_BIN"))
        .unwrap_or_else(|_| "/home/betmul/.local/bin/pi".into());
    if !Path::new(&pi_bin).exists() {
        eprintln!("pi CLI not found at {pi_bin}; skipping native startup test");
        return;
    }

    let tmp =
        std::env::temp_dir().join(format!("cash-native-pi-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp).unwrap();

    let trace = readers::pi::read(&fixture("real/pi_real_sanitized.jsonl")).unwrap();
    let result =
        import::pi::import(&trace, &tmp.join("pi-root")).expect("import into temp pi root");
    let html = tmp.join("session.html");

    let output = Command::new(&pi_bin)
        .args([
            "--offline",
            "--export",
            result.file.as_str(),
            html.to_str().unwrap(),
        ])
        .output()
        .expect("run pi export");
    assert!(
        output.status.success(),
        "pi export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(html.metadata().map(|m| m.len()).unwrap_or(0) > 0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore = "requires pi CLI and a PTY; starts the imported session without making a model request"]
fn pi_tui_starts_imported_session_without_uncaught_exception() {
    let pi_bin = std::env::var("CASH_PI_BIN")
        .or_else(|_| std::env::var("MIGRATE_PI_BIN"))
        .unwrap_or_else(|_| "/home/betmul/.local/bin/pi".into());
    if !Path::new(&pi_bin).exists() {
        eprintln!("pi CLI not found at {pi_bin}; skipping native startup test");
        return;
    }

    let tmp = std::env::temp_dir().join(format!(
        "cash-native-pi-tui-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let trace = readers::pi::read(&fixture("real/pi_real_sanitized.jsonl")).unwrap();
    let result =
        import::pi::import(&trace, &tmp.join("pi-root")).expect("import into temp pi root");

    let command = format!(
        "timeout --signal=INT --kill-after=2s 4s {} --offline --no-extensions --no-skills --no-themes --no-context-files --session {}",
        shell_quote(&pi_bin),
        shell_quote(&result.file)
    );
    let output = Command::new("script")
        .args(["-qefc", &command, "/dev/null"])
        .output()
        .expect("run pi in a pseudo-terminal");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(124),
        "Pi exited before the startup timeout, indicating a load/render failure:\n{combined}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore = "requires codex CLI, a copied CODEX_HOME, and one real model call"]
fn codex_cli_resumes_imported_session() {
    if Command::new("codex").arg("--version").output().is_err() {
        eprintln!("codex not found; skipping native startup test");
        return;
    }
    let tmp = std::env::temp_dir().join(format!(
        "cash-native-codex-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let codex_home = tmp.join(".codex");
    copy_dir(&config::home_dir().join(".codex"), &codex_home);

    let trace = readers::pi::read(&fixture("real/pi_real_sanitized.jsonl")).unwrap();
    let result = import::codex::import(&trace, &codex_home.join("sessions")).expect("import codex");

    let mut child = Command::new("codex")
        .args(["exec", "resume", &result.session_id, "-", "--json"])
        .env("CODEX_HOME", &codex_home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run codex resume");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Reply with exactly: OK")
        .unwrap();
    let output = child.wait_with_output().expect("wait for codex");
    assert!(
        output.status.success(),
        "codex resume failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("agent_message"),
        "no agent response:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

fn copy_dir(from: &Path, to: &Path) {
    if !from.exists() {
        return;
    }
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            let _ = std::fs::copy(&src, &dst);
        }
    }
}

/// Copy an OpenCode store consistently (live WAL may not be in the main .db).
fn copy_opencode_db(src: &Path, dst: &Path) {
    let conn = rusqlite::Connection::open(src).expect("open source opencode db");
    conn.execute_batch(&format!(
        "VACUUM INTO '{}'",
        dst.to_string_lossy().replace('\'', "''")
    ))
    .expect("vacuum into destination db");
    drop(conn);
}

fn opencode<const N: usize>(xdg: &Path, args: [&str; N]) -> std::process::Output {
    Command::new("opencode")
        .args(args)
        .env("XDG_DATA_HOME", xdg)
        .output()
        .expect("run opencode")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
