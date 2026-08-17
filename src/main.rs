use std::io::Write;
use std::path::{Path, PathBuf};

use cash::config;
use cash::export;
use cash::import;
use cash::ir;
use cash::ir::AgentKind;
use cash::readers;
use cash::sync;
use cash::util;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cash", about = "CASH — Cross-Agent Session History", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List available sessions of an agent
    List {
        agent: String,
        #[arg(long)]
        codex_root: Option<PathBuf>,
        #[arg(long)]
        pi_root: Option<PathBuf>,
        #[arg(long)]
        opencode_db: Option<PathBuf>,
    },
    /// Export an agent session as a portable CASH seed
    Export {
        agent: String,
        session: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        codex_root: Option<PathBuf>,
        #[arg(long)]
        pi_root: Option<PathBuf>,
        #[arg(long)]
        opencode_db: Option<PathBuf>,
    },
    /// Materialize one agent session into another agent's native history
    Convert {
        source_agent: String,
        session: String,
        target_agent: String,
        #[arg(short, long)]
        seed: Option<PathBuf>,
        #[arg(long)]
        codex_root: Option<PathBuf>,
        #[arg(long)]
        pi_root: Option<PathBuf>,
        #[arg(long)]
        opencode_db: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        model: Option<String>,
    },
    /// Materialize a CASH seed into native agent storage
    Import {
        agent: String,
        #[arg(short, long)]
        seed: Option<PathBuf>,
        #[arg(long)]
        codex_root: Option<PathBuf>,
        #[arg(long)]
        pi_root: Option<PathBuf>,
        #[arg(long)]
        opencode_db: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        model: Option<String>,
    },
    /// Append target-agent continuation back to the original source session
    Sync {
        /// Original source session ID; resolves its seed from CASH_SEED_DIR or config
        session: Option<String>,
        #[arg(short, long)]
        seed: Option<PathBuf>,
        #[arg(long)]
        pi_root: Option<PathBuf>,
        #[arg(long)]
        codex_root: Option<PathBuf>,
        #[arg(long)]
        opencode_db: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Inspect source and target state recorded by a CASH seed
    Status {
        seed: Option<PathBuf>,
        #[arg(long)]
        opencode_db: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn default_codex_root() -> PathBuf {
    config::home_dir().join(".codex/sessions")
}
fn default_pi_root() -> PathBuf {
    config::home_dir().join(".pi/agent/sessions")
}
fn default_opencode_db() -> PathBuf {
    config::home_dir().join(".local/share/opencode/opencode.db")
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::List {
            agent,
            codex_root,
            pi_root,
            opencode_db,
        } => {
            let kind = parse_agent(&agent)?;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let sessions = match kind {
                AgentKind::Codex => readers::codex::list_session_summaries(
                    &codex_root.unwrap_or_else(default_codex_root),
                )?,
                AgentKind::Pi => {
                    readers::pi::list_session_summaries(&pi_root.unwrap_or_else(default_pi_root))?
                }
                AgentKind::OpenCode => readers::opencode::list_session_summaries(
                    &opencode_db.unwrap_or_else(default_opencode_db),
                )?,
            };
            if !emit_session_list(&mut out, kind, &sessions)? {
                return Ok(());
            }
            Ok(())
        }
        Command::Export {
            agent,
            session,
            out,
            codex_root,
            pi_root,
            opencode_db,
        } => {
            let kind = parse_agent(&agent)?;
            let (root, db) = match kind {
                AgentKind::Codex => (
                    codex_root.unwrap_or_else(default_codex_root),
                    default_opencode_db(),
                ),
                AgentKind::Pi => (
                    pi_root.unwrap_or_else(default_pi_root),
                    default_opencode_db(),
                ),
                AgentKind::OpenCode => (
                    default_codex_root(),
                    opencode_db.unwrap_or_else(default_opencode_db),
                ),
            };
            let trace = readers::read_trace(kind, &session, &root, &db)?;
            println!(
                "read {}: session={} events={} (source sha256 {:.12}…)",
                kind, trace.meta.session_id, trace.meta.event_count, &trace.meta.source_file_sha256
            );
            let out = out.unwrap_or_else(|| {
                config::default_seed_output(kind.as_str(), &trace.meta.session_id)
            });
            let manifest = export::write_seed(&trace, &out)?;
            println!("exported to {}", out.display());
            println!("  source events_sha256: {}", manifest.source.events_sha256);
            Ok(())
        }
        Command::Import {
            agent,
            seed,
            codex_root,
            pi_root,
            opencode_db,
            force,
            model,
        } => {
            let kind = parse_agent(&agent)?;
            let seed = resolve_seed_dir(seed)?;
            let db = opencode_db.unwrap_or_else(default_opencode_db);
            let trace = load_trace(&seed)?;
            warn_model_override(&trace, model.as_deref());
            let result = import_and_update_manifest(
                kind,
                &trace,
                &seed,
                ImportOptions {
                    codex_root: codex_root.unwrap_or_else(default_codex_root),
                    pi_root: pi_root.unwrap_or_else(default_pi_root),
                    opencode_db: db,
                    force,
                    model_override: model,
                },
            )?;
            print_import_result(kind, &seed, &result);
            Ok(())
        }
        Command::Convert {
            source_agent,
            session,
            target_agent,
            seed,
            codex_root,
            pi_root,
            opencode_db,
            force,
            model,
        } => {
            let source_kind = parse_agent(&source_agent)?;
            let target_kind = parse_agent(&target_agent)?;
            let db = opencode_db.unwrap_or_else(default_opencode_db);
            let source_root = match source_kind {
                AgentKind::Codex => codex_root.clone().unwrap_or_else(default_codex_root),
                AgentKind::Pi => pi_root.clone().unwrap_or_else(default_pi_root),
                AgentKind::OpenCode => default_codex_root(),
            };
            let trace = readers::read_trace(source_kind, &session, &source_root, &db)?;
            println!(
                "read {}: session={} events={} (source sha256 {:.12}…)",
                source_kind,
                trace.meta.session_id,
                trace.meta.event_count,
                &trace.meta.source_file_sha256
            );
            let seed = seed.unwrap_or_else(|| {
                config::default_seed_output(source_kind.as_str(), &trace.meta.session_id)
            });
            let manifest = export::write_seed(&trace, &seed)?;
            println!("exported seed to {}", seed.display());
            println!("  source events_sha256: {}", manifest.source.events_sha256);

            warn_model_override(&trace, model.as_deref());
            let result = import_and_update_manifest(
                target_kind,
                &trace,
                &seed,
                ImportOptions {
                    codex_root: codex_root.unwrap_or_else(default_codex_root),
                    pi_root: pi_root.unwrap_or_else(default_pi_root),
                    opencode_db: db,
                    force,
                    model_override: model,
                },
            )?;
            print_import_result(target_kind, &seed, &result);
            Ok(())
        }
        Command::Sync {
            session,
            seed,
            pi_root,
            codex_root,
            opencode_db,
            force,
        } => {
            let seed = resolve_sync_seed(session, seed)?;
            sync_continuation(
                &seed,
                &pi_root.unwrap_or_else(default_pi_root),
                &codex_root.unwrap_or_else(default_codex_root),
                &opencode_db.unwrap_or_else(default_opencode_db),
                force,
            )?;
            Ok(())
        }
        Command::Status { seed, opencode_db } => {
            let seed = resolve_seed_dir(seed)?;
            let db = opencode_db.unwrap_or_else(default_opencode_db);
            let manifest = export::load_manifest(&seed)?;
            let report = sync::check(&manifest, &db)?;
            println!("SOURCE  {}", report.source.detail);
            println!(
                "  file hash unchanged: {}",
                yesno(report.source.file_unchanged)
            );
            println!(
                "  events unchanged:    {}",
                yesno(report.source.events_unchanged)
            );
            println!("TARGET  {}", report.target.detail);
            println!(
                "  session present:     {}",
                yesno(report.target.session_present)
            );
            println!(
                "  anchor present:      {}",
                yesno(report.target.anchor_present)
            );
            println!(
                "  continued past seed: {}",
                yesno(report.target.continued_past_seed)
            );
            if report.target.extra_messages > 0 {
                println!(
                    "  extra messages after seed point: {}",
                    report.target.extra_messages
                );
            }
            Ok(())
        }
    }
}

fn sync_continuation(
    seed: &Path,
    pi_root: &Path,
    codex_root: &Path,
    opencode_db: &Path,
    force: bool,
) -> Result<(), String> {
    let mut manifest = export::load_manifest(seed)?;
    let source_kind: AgentKind = manifest.source.agent.parse()?;
    let target = manifest
        .target
        .clone()
        .ok_or_else(|| "seed has no target session; run convert first".to_string())?;
    let target_kind: AgentKind = target.agent.parse()?;
    let target_trace = read_bound_trace(target_kind, &target, pi_root, codex_root, opencode_db)?;
    let sync = manifest.sync.as_ref().filter(|sync| {
        sync.source_agent == manifest.source.agent
            && sync.source_session_id == manifest.source.session_id
            && sync.target_agent == target.agent
            && sync.target_session_id == target.session_id
    });
    let anchor = sync
        .map(|sync| sync.target_anchor_message_id.as_str())
        .unwrap_or(target.anchor_message_id.as_str());
    let anchor_index = target_trace
        .events
        .iter()
        .rposition(|event| anchor_matches(target_kind, &event.original_id, anchor))
        .ok_or_else(|| format!("target sync anchor {anchor} is missing"))?;
    let all_delta = &target_trace.events[anchor_index + 1..];
    if all_delta.is_empty() {
        println!("no new events after {} anchor", target_kind);
        return Ok(());
    }

    let source_trace = read_source_trace(source_kind, &manifest.source, opencode_db)?;
    let expected_hash = sync
        .map(|sync| sync.source_events_sha256.as_str())
        .unwrap_or(manifest.source.events_sha256.as_str());
    if source_trace.meta.events_sha256 != expected_hash && !force {
        return Err(format!(
            "original {} session changed since last sync; refusing to append {} (use --force)",
            source_kind, manifest.source.session_id
        ));
    }

    let delta: Vec<ir::Event> = all_delta
        .iter()
        .filter(|event| is_syncable_event(target_kind, event))
        .cloned()
        .collect();
    if delta.is_empty() {
        save_sync_ref(&mut manifest, &target, &source_trace, &target_trace);
        export::save_manifest(seed, &manifest)?;
        println!("no transferable events after {} anchor", target_kind);
        return Ok(());
    }
    let delta_trace = ir::Trace {
        meta: target_trace.meta.clone(),
        events: delta.clone(),
    };
    let result = match source_kind {
        AgentKind::OpenCode => import::opencode::append_existing(
            &delta_trace,
            opencode_db,
            &manifest.source.session_id,
        )?,
        AgentKind::Pi | AgentKind::Codex => rewrite_source_with_delta(
            source_kind,
            &source_trace,
            &delta,
            Path::new(&manifest.source.file),
            pi_root,
            codex_root,
        )?,
    };
    let updated_source = read_source_trace(source_kind, &manifest.source, opencode_db)?;
    save_sync_ref(&mut manifest, &target, &updated_source, &target_trace);
    export::save_manifest(seed, &manifest)?;
    println!(
        "synced {} {} events into {} session {} ({} records)",
        delta.len(),
        target_kind,
        source_kind,
        manifest.source.session_id,
        result.message_count
    );
    println!("manifest updated: {}", seed.join("manifest.json").display());
    Ok(())
}

fn is_syncable_event(kind: AgentKind, event: &ir::Event) -> bool {
    if matches!(event.kind, ir::EventKind::NativeRecord { .. }) {
        return false;
    }
    if kind != AgentKind::Codex {
        return true;
    }
    if event
        .native
        .as_ref()
        .and_then(|native| native.get("role"))
        .and_then(serde_json::Value::as_str)
        == Some("developer")
    {
        return false;
    }
    !matches!(
        &event.kind,
        ir::EventKind::UserMessage { text } if is_injected_codex_context(text)
    )
}

fn is_injected_codex_context(text: &str) -> bool {
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

fn save_sync_ref(
    manifest: &mut export::Manifest,
    target: &export::TargetRef,
    source_trace: &ir::Trace,
    target_trace: &ir::Trace,
) {
    let last_target_event = target_trace.events.last().expect("target has anchor");
    manifest.sync = Some(export::SyncRef {
        source_agent: manifest.source.agent.clone(),
        source_session_id: manifest.source.session_id.clone(),
        source_file: manifest.source.file.clone(),
        source_events_sha256: source_trace.meta.events_sha256.clone(),
        target_agent: target.agent.clone(),
        target_session_id: target.session_id.clone(),
        target_file: target.file.clone(),
        target_anchor_message_id: last_target_event.original_id.clone(),
        target_events_sha256: target_trace.meta.events_sha256.clone(),
    });
}

fn read_bound_trace(
    kind: AgentKind,
    target: &export::TargetRef,
    pi_root: &Path,
    codex_root: &Path,
    opencode_db: &Path,
) -> Result<ir::Trace, String> {
    match kind {
        AgentKind::OpenCode => readers::opencode::read(opencode_db, &target.session_id),
        AgentKind::Pi | AgentKind::Codex => {
            let root = if kind == AgentKind::Pi {
                pi_root
            } else {
                codex_root
            };
            let path = resolve_bound_file(&target.file, root);
            let trace =
                readers::read_trace(kind, path.to_string_lossy().as_ref(), root, opencode_db)?;
            if trace.meta.session_id != target.session_id {
                return Err(format!(
                    "target {} session ID mismatch: manifest={}, file={}",
                    kind, target.session_id, trace.meta.session_id
                ));
            }
            Ok(trace)
        }
    }
}

fn read_source_trace(
    kind: AgentKind,
    source: &export::SourceRef,
    opencode_db: &Path,
) -> Result<ir::Trace, String> {
    match kind {
        AgentKind::OpenCode => readers::opencode::read(opencode_db, &source.session_id),
        AgentKind::Pi => readers::pi::read(Path::new(&source.file)),
        AgentKind::Codex => readers::codex::read(Path::new(&source.file)),
    }
}

fn rewrite_source_with_delta(
    kind: AgentKind,
    source_trace: &ir::Trace,
    delta: &[ir::Event],
    source_file: &Path,
    pi_root: &Path,
    codex_root: &Path,
) -> Result<import::ImportResult, String> {
    let mut trace = source_trace.clone();
    trace.events.extend_from_slice(delta);
    match kind {
        AgentKind::Pi => import::pi::import_existing(
            &trace,
            pi_root,
            Some(source_file),
            Some(&source_trace.meta.session_id),
            None,
            true,
            None,
        ),
        AgentKind::Codex => import::codex::import_existing(
            &trace,
            codex_root,
            Some(source_file),
            Some(&source_trace.meta.session_id),
            None,
            true,
            None,
        ),
        AgentKind::OpenCode => unreachable!(),
    }
}

fn resolve_bound_file(file: &str, root: &Path) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn anchor_matches(kind: AgentKind, event_id: &str, anchor: &str) -> bool {
    if event_id == anchor {
        return true;
    }
    if kind != AgentKind::Codex {
        return false;
    }
    event_id == readers::codex::source_id_for_response_item_id(anchor)
}

fn parse_agent(s: &str) -> Result<AgentKind, String> {
    s.parse::<AgentKind>()
}

fn load_trace(seed_dir: &Path) -> Result<ir::Trace, String> {
    let p = seed_dir.join("seed.json");
    let raw = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", p.display()))
}

struct ImportOptions {
    codex_root: PathBuf,
    pi_root: PathBuf,
    opencode_db: PathBuf,
    force: bool,
    model_override: Option<String>,
}

fn import_and_update_manifest(
    kind: AgentKind,
    trace: &ir::Trace,
    seed: &Path,
    opts: ImportOptions,
) -> Result<import::ImportResult, String> {
    let mut manifest = export::load_manifest(seed)?;
    let existing = manifest
        .target
        .as_ref()
        .filter(|target| target.agent == kind.as_str());
    let model_override = opts.model_override.as_deref();
    let result = match kind {
        AgentKind::OpenCode => import::opencode::import_existing(
            trace,
            &opts.opencode_db,
            existing.map(|target| target.session_id.as_str()),
            existing.map(|target| target.anchor_message_id.as_str()),
            opts.force,
            model_override,
        )?,
        AgentKind::Pi => import::pi::import_existing(
            trace,
            &opts.pi_root,
            existing.map(|target| Path::new(&target.file)),
            existing.map(|target| target.session_id.as_str()),
            existing.map(|target| target.anchor_message_id.as_str()),
            opts.force,
            model_override,
        )?,
        AgentKind::Codex => import::codex::import_existing(
            trace,
            &opts.codex_root,
            existing.map(|target| Path::new(&target.file)),
            existing.map(|target| target.session_id.as_str()),
            existing.map(|target| target.anchor_message_id.as_str()),
            opts.force,
            model_override,
        )?,
    };
    manifest.target = Some(export::TargetRef {
        agent: kind.as_str().into(),
        session_id: result.session_id.clone(),
        file: result.file.clone(),
        anchor_message_id: result.anchor_message_id.clone(),
        injected_at: chrono::Utc::now().to_rfc3339(),
        events_sha256: export::hash_trace_events(&trace.events),
        seed_event_count: trace.events.len(),
        native_message_count: result.message_count,
        dropped_event_count: result.dropped_event_count,
    });
    export::save_manifest(seed, &manifest)?;
    Ok(result)
}

fn warn_model_override(trace: &ir::Trace, model_override: Option<&str>) {
    let original = trace
        .meta
        .model
        .as_deref()
        .and_then(|m| {
            serde_json::from_str::<serde_json::Value>(m)
                .ok()
                .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(String::from))
        })
        .or_else(|| trace.meta.model.clone())
        .unwrap_or_else(|| "<none>".to_string());
    if let Some(model) = model_override {
        if original != model {
            println!(
                "note: overriding session model {original} -> {model} (the target may not support the source model)"
            );
        }
    } else {
        println!(
            "note: source session model is {original}; if the target does not support it, pass --model <target-model>"
        );
    }
}

fn print_import_result(kind: AgentKind, seed: &Path, result: &import::ImportResult) {
    println!(
        "injected into {} as session {} ({} messages)",
        kind, result.session_id, result.message_count
    );
    if !result.file.is_empty() {
        println!("target file: {}", result.file);
    }
    println!("anchor message id: {}", result.anchor_message_id);
    if result.dropped_event_count > 0 {
        println!(
            "dropped target-native events: {} (seed.json still keeps the full trace)",
            result.dropped_event_count
        );
    }
    println!("manifest updated: {}", seed.join("manifest.json").display());
}

fn resolve_sync_seed(session: Option<String>, seed: Option<PathBuf>) -> Result<PathBuf, String> {
    match (session, seed) {
        (Some(_), Some(_)) => Err("pass either a source session ID or --seed, not both".into()),
        (Some(session_id), None) => {
            let mut matches = Vec::new();
            for kind in [AgentKind::OpenCode, AgentKind::Pi, AgentKind::Codex] {
                let candidate = config::default_seed_output(kind.as_str(), &session_id);
                let Ok(manifest) = export::load_manifest(&candidate) else {
                    continue;
                };
                if manifest.source.agent == kind.as_str()
                    && manifest.source.session_id == session_id
                {
                    matches.push(candidate);
                }
            }
            match matches.len() {
                1 => Ok(matches.remove(0)),
                0 => Err(format!(
                    "no seed found for source session {session_id} under {}",
                    config::default_seed_dir().display()
                )),
                _ => Err(format!(
                    "source session ID {session_id} has multiple seeds; use --seed to choose one"
                )),
            }
        }
        (None, seed) => resolve_seed_dir(seed),
    }
}

fn resolve_seed_dir(seed: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(seed) = seed {
        return Ok(seed);
    }

    let base = config::default_seed_dir();
    if base.join("seed.json").exists() && base.join("manifest.json").exists() {
        return Ok(base);
    }

    latest_seed_dir(&base).ok_or_else(|| {
        format!(
            "no seed specified and no seed found under {} (set CASH_SEED_DIR, ~/.config/cash/config.json, or pass --seed)",
            base.display()
        )
    })
}

fn latest_seed_dir(base: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    walk_seed_dirs(base, &mut best);
    best.map(|(_, p)| p)
}

fn walk_seed_dirs(dir: &Path, best: &mut Option<(std::time::SystemTime, PathBuf)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let manifest = path.join("manifest.json");
            if manifest.exists()
                && let Ok(modified) = manifest.metadata().and_then(|m| m.modified())
            {
                match best {
                    Some((best_time, _)) if *best_time >= modified => {}
                    _ => *best = Some((modified, path.clone())),
                }
            }
            walk_seed_dirs(&path, best);
        }
    }
}

fn yesno(b: bool) -> &'static str {
    if b { "yes" } else { "NO" }
}

fn emit_session_list(
    out: &mut impl Write,
    kind: AgentKind,
    sessions: &[readers::SessionSummary],
) -> Result<bool, String> {
    let agent = agent_display_name(kind);
    if sessions.is_empty() {
        return emit_line(out, &format!("No {agent} sessions found."));
    }

    if !emit_line(
        out,
        &format!("{agent} sessions: {} (newest first)", sessions.len()),
    )? {
        return Ok(false);
    }

    for (index, session) in sessions.iter().enumerate() {
        let title = session
            .title
            .as_deref()
            .map(|text| compact_text(text, 96))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(untitled session)".to_string());
        let time = session
            .time
            .map(util::format_local_ms)
            .unwrap_or_else(|| "(unknown)".to_string());
        let workspace = session
            .cwd
            .as_deref()
            .map(human_path)
            .unwrap_or_else(|| "(unknown)".to_string());
        let lines = [
            String::new(),
            format!("{}. {title}", index + 1),
            format!("   {}:   {time}", session.time_kind.label()),
            format!("   Workspace: {workspace}"),
            format!("   Session:   {}", compact_text(&session.session_id, 128)),
        ];
        for line in lines {
            if !emit_line(out, &line)? {
                return Ok(false);
            }
        }
    }

    Ok(true)
}

fn agent_display_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Codex => "Codex",
        AgentKind::OpenCode => "OpenCode",
        AgentKind::Pi => "Pi",
    }
}

fn human_path(path: &str) -> String {
    let path = Path::new(path);
    let home = config::home_dir();
    let displayed = match path.strip_prefix(&home) {
        Ok(relative) if relative.as_os_str().is_empty() => "~".to_string(),
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    };
    compact_text(&displayed, 128)
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !normalized.is_empty();
        } else if !ch.is_control() {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.push(ch);
        }
    }

    let mut chars = normalized.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn emit_line(out: &mut impl Write, line: &str) -> Result<bool, String> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
