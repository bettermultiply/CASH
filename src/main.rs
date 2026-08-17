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
            let events_sha256 = manifest
                .find_node(kind.as_str(), &trace.meta.session_id)
                .map(|node| node.events_sha256.clone())
                .unwrap_or_default();
            println!("  source events_sha256: {events_sha256}");
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
            let (result, _) = import_and_update_manifest(
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
            // Group-aware: a seed is a peer group of copies of one logical
            // session. When the convert source is already a member of an
            // existing seed (e.g. pi -> opencode, then opencode -> codex), we
            // reuse that group and just add the new copy.
            let seed = match seed {
                Some(explicit) => explicit,
                None => {
                    match find_seed_containing_node(source_kind.as_str(), &trace.meta.session_id)? {
                        Some(found) => found,
                        None => config::default_seed_output(
                            source_kind.as_str(),
                            &trace.meta.session_id,
                        ),
                    }
                }
            };
            if seed.join("manifest.json").exists() {
                let existing = export::load_manifest(&seed)?;
                if !existing.nodes.iter().any(|node| {
                    node.agent == source_agent && node.session_id == trace.meta.session_id
                }) {
                    return Err(format!(
                        "seed {} already tracks a different session; pass a fresh --seed directory",
                        seed.display()
                    ));
                }
                export::write_trace_files(&trace, &seed)?;
            } else {
                let manifest = export::write_seed(&trace, &seed)?;
                println!("exported seed to {}", seed.display());
                if let Some(node) = manifest.find_node(source_kind.as_str(), &trace.meta.session_id)
                {
                    println!("  source events_sha256: {}", node.events_sha256);
                }
            }

            warn_model_override(&trace, model.as_deref());
            let (result, mut manifest) = import_and_update_manifest(
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
            // Advance the converted copy's sync anchor past this trace so a
            // later `sync` does not re-propagate already-converted events.
            let last_id = trace
                .events
                .last()
                .map(|event| event.original_id.clone())
                .unwrap_or_default();
            manifest.update_node(
                &source_agent,
                &trace.meta.session_id,
                &last_id,
                &trace.meta.events_sha256,
            );
            export::save_manifest(&seed, &manifest)?;
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
            for node in report.nodes {
                println!("NODE {} ({})", node.agent, node.session_id);
                println!("  detail: {}", node.detail);
                println!("  session present: {}", yesno(node.session_present));
                println!("  anchor present: {}", yesno(node.anchor_present));
                println!("  continued past seed: {}", yesno(node.continued_past_seed));
                if node.extra_messages > 0 {
                    println!(
                        "  extra messages after seed point: {}",
                        node.extra_messages
                    );
                }
                println!("  file hash unchanged: {}", yesno(node.file_unchanged));
                println!("  events unchanged: {}", yesno(node.events_unchanged));
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
    _force: bool,
) -> Result<(), String> {
    let mut manifest = export::load_manifest(seed)?;
    let nodes = manifest.copies();
    if nodes.len() < 2 {
        println!("seed has no target copies; run convert first");
        return Ok(());
    }

    // Read every copy's current trace and compute its delta past its anchor.
    let mut traces: Vec<ir::Trace> = Vec::with_capacity(nodes.len());
    for node in &nodes {
        traces.push(read_node_trace(node, pi_root, codex_root, opencode_db)?);
    }
    struct CopyState {
        delta: Vec<ir::Event>,
        has_unconsumed: bool,
    }
    let states: Vec<CopyState> = nodes
        .iter()
        .zip(&traces)
        .map(|(node, trace)| {
            let kind: AgentKind = node.agent.parse().unwrap();
            match trace
                .events
                .iter()
                .rposition(|event| anchor_matches(kind, &event.original_id, &node.anchor_message_id))
            {
                Some(index) => CopyState {
                    delta: trace.events[index + 1..]
                        .iter()
                        .filter(|event| is_syncable_event(kind, event))
                        .cloned()
                        .collect(),
                    has_unconsumed: index + 1 < trace.events.len(),
                },
                None => CopyState {
                    delta: Vec::new(),
                    has_unconsumed: false,
                },
            }
        })
        .collect();

    let changed: Vec<usize> = states
        .iter()
        .enumerate()
        .filter(|(_, state)| !state.delta.is_empty())
        .map(|(i, _)| i)
        .collect();

    if changed.is_empty() {
        // Copies may still have gained events that are not transferable (e.g.
        // Codex-injected context); advance their anchors so we do not rescan.
        let mut advanced = false;
        for (i, node) in nodes.iter().enumerate() {
            if states[i].has_unconsumed {
                let last_id = traces[i]
                    .events
                    .last()
                    .map(|event| event.original_id.clone())
                    .unwrap_or_default();
                manifest.update_node(
                    &node.agent,
                    &node.session_id,
                    &last_id,
                    &traces[i].meta.events_sha256,
                );
                advanced = true;
            }
        }
        if advanced {
            export::save_manifest(seed, &manifest)?;
            println!("no transferable events after {} copy anchors", nodes.len());
        } else {
            println!("no new events after {} copy anchors", nodes.len());
        }
        return Ok(());
    }

    // Linear writeback + stop on conflict: only a single changed copy is
    // propagated. If several copies gained events independently we refuse;
    // --force cannot merge divergent copies.
    if changed.len() > 1 {
        let names: Vec<&str> = changed.iter().map(|&i| nodes[i].agent.as_str()).collect();
        return Err(format!(
            "conflict: multiple copies gained new events ({}); refusing to merge — resolve the divergence manually",
            names.join(", ")
        ));
    }
    let changed_index = changed[0];
    let delta = states[changed_index].delta.clone();
    if delta.is_empty() {
        return Ok(());
    }

    // Every other copy must be untouched since the last sync; otherwise we
    // would clobber independent work.
    for i in 0..nodes.len() {
        if i == changed_index {
            continue;
        }
        if traces[i].meta.events_sha256 != nodes[i].events_sha256 {
            return Err(format!(
                "{} copy {} changed independently since the last sync; refusing to propagate (resolve the divergence first)",
                nodes[i].agent, nodes[i].session_id
            ));
        }
    }

    for i in 0..nodes.len() {
        if i == changed_index {
            continue;
        }
        let kind: AgentKind = nodes[i].agent.parse()?;
        append_to_node(kind, &nodes[i], &traces[i], &delta, pi_root, codex_root, opencode_db)?;
        let updated = read_node_trace(&nodes[i], pi_root, codex_root, opencode_db)?;
        let last_id = updated
            .events
            .last()
            .map(|event| event.original_id.clone())
            .unwrap_or_default();
        manifest.update_node(
            &nodes[i].agent,
            &nodes[i].session_id,
            &last_id,
            &updated.meta.events_sha256,
        );
    }
    // The changed copy's anchor also advances past its own delta (already
    // propagated to every other copy), so a later sync does not re-propagate
    // consumed events.
    let changed_node = &nodes[changed_index];
    let last_id = traces[changed_index]
        .events
        .last()
        .map(|event| event.original_id.clone())
        .unwrap_or_default();
    manifest.update_node(
        &changed_node.agent,
        &changed_node.session_id,
        &last_id,
        &traces[changed_index].meta.events_sha256,
    );
    export::save_manifest(seed, &manifest)?;
    println!(
        "synced {} {} events into the other {} copies",
        delta.len(),
        nodes[changed_index].agent,
        nodes.len() - 1
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

fn read_node_trace(
    node: &export::NodeRef,
    pi_root: &Path,
    codex_root: &Path,
    opencode_db: &Path,
) -> Result<ir::Trace, String> {
    let kind: AgentKind = node.agent.parse()?;
    match kind {
        AgentKind::OpenCode => readers::opencode::read(opencode_db, &node.session_id),
        AgentKind::Pi => readers::pi::read(&resolve_bound_file(&node.file, pi_root)),
        AgentKind::Codex => readers::codex::read(&resolve_bound_file(&node.file, codex_root)),
    }
}

/// Append `delta` into one copy of the logical session, preserving its native
/// session identity.
fn append_to_node(
    kind: AgentKind,
    node: &export::NodeRef,
    trace: &ir::Trace,
    delta: &[ir::Event],
    pi_root: &Path,
    codex_root: &Path,
    opencode_db: &Path,
) -> Result<import::ImportResult, String> {
    match kind {
        AgentKind::OpenCode => {
            let delta_trace = ir::Trace {
                meta: trace.meta.clone(),
                events: delta.to_vec(),
            };
            import::opencode::append_existing(&delta_trace, opencode_db, &node.session_id)
        }
        AgentKind::Pi => {
            let mut merged = trace.clone();
            merged.events.extend_from_slice(delta);
            let file = resolve_bound_file(&node.file, pi_root);
            import::pi::import_existing(&merged, pi_root, Some(&file), Some(&node.session_id), None, true, None)
        }
        AgentKind::Codex => {
            let mut merged = trace.clone();
            merged.events.extend_from_slice(delta);
            let file = resolve_bound_file(&node.file, codex_root);
            import::codex::import_existing(&merged, codex_root, Some(&file), Some(&node.session_id), None, true, None)
        }
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
) -> Result<(import::ImportResult, export::Manifest), String> {
    let mut manifest = export::load_manifest(seed)?;
    let existing = manifest.nodes.iter().find(|node| node.agent == kind.as_str());
    let model_override = opts.model_override.as_deref();
    let result = match kind {
        AgentKind::OpenCode => import::opencode::import_existing(
            trace,
            &opts.opencode_db,
            existing.map(|node| node.session_id.as_str()),
            existing.map(|node| node.anchor_message_id.as_str()),
            opts.force,
            model_override,
        )?,
        AgentKind::Pi => import::pi::import_existing(
            trace,
            &opts.pi_root,
            existing.map(|node| Path::new(&node.file)),
            existing.map(|node| node.session_id.as_str()),
            existing.map(|node| node.anchor_message_id.as_str()),
            opts.force,
            model_override,
        )?,
        AgentKind::Codex => import::codex::import_existing(
            trace,
            &opts.codex_root,
            existing.map(|node| Path::new(&node.file)),
            existing.map(|node| node.session_id.as_str()),
            existing.map(|node| node.anchor_message_id.as_str()),
            opts.force,
            model_override,
        )?,
    };
    // Record the copy's own event hash as materialized in the target, not the
    // source trace hash: a target that drops events (e.g. Codex/OpenCode drop
    // model_change) would otherwise never match its file on re-read, and `sync`
    // would treat a pristine copy as independently diverged.
    let events_sha256 = match kind {
        AgentKind::OpenCode => export::hash_trace_events(
            &readers::opencode::read(&opts.opencode_db, &result.session_id)?.events,
        ),
        AgentKind::Pi => {
            export::hash_trace_events(&readers::pi::read(Path::new(&result.file))?.events)
        }
        AgentKind::Codex => {
            export::hash_trace_events(&readers::codex::read(Path::new(&result.file))?.events)
        }
    };
    manifest.upsert_node(export::NodeRef {
        agent: kind.as_str().into(),
        session_id: result.session_id.clone(),
        file: result.file.clone(),
        anchor_message_id: result.anchor_message_id.clone(),
        events_sha256,
        injected_at: chrono::Utc::now().to_rfc3339(),
        seed_event_count: trace.events.len(),
        native_message_count: result.message_count,
        dropped_event_count: result.dropped_event_count,
        ..Default::default()
    });
    export::save_manifest(seed, &manifest)?;
    Ok((result, manifest))
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
        (Some(_), Some(_)) => Err("pass either a session ID or --seed, not both".into()),
        (Some(session_id), None) => {
            let matches = find_seed_containing_session(&session_id)?;
            match matches.len() {
                1 => Ok(matches.into_iter().next().expect("one match")),
                0 => Err(format!(
                    "no seed found for session {session_id} under {}",
                    config::default_seed_dir().display()
                )),
                _ => Err(format!(
                    "session {session_id} has multiple seeds; use --seed to choose one"
                )),
            }
        }
        (None, seed) => resolve_seed_dir(seed),
    }
}

/// Find the seed whose peer group contains a copy for `agent`/`session`.
/// Used by `convert` so a chain (e.g. pi -> opencode, then opencode -> codex)
/// extends the same group instead of starting a new seed.
fn find_seed_containing_node(agent: &str, session: &str) -> Result<Option<PathBuf>, String> {
    let matches = find_seed_containing(|manifest| {
        manifest
            .copies()
            .iter()
            .any(|node| node.agent == agent && node.session_id == session)
    })?;
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().expect("one match"))),
        _ => Err(format!(
            "source {agent} session {session} matches multiple seeds; pass --seed to choose one"
        )),
    }
}

fn find_seed_containing_session(session: &str) -> Result<Vec<PathBuf>, String> {
    find_seed_containing(|manifest| {
        manifest
            .copies()
            .iter()
            .any(|node| node.session_id == session)
    })
}

fn find_seed_containing(
    matches_node: impl Fn(&export::Manifest) -> bool,
) -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::new();
    collect_seed_dirs(&config::default_seed_dir(), &mut dirs);
    let mut found = Vec::new();
    for dir in dirs {
        if let Ok(manifest) = export::load_manifest(&dir)
            && matches_node(&manifest)
        {
            found.push(dir);
        }
    }
    Ok(found)
}

fn collect_seed_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("manifest.json").exists() && path.join("seed.json").exists() {
            out.push(path.clone());
        } else {
            collect_seed_dirs(&path, out);
        }
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
