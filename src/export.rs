use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::ir::{Event, EventKind, Trace};
use crate::util::sha256_hex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub agent: String,
    pub session_id: String,
    pub file: String,
    pub file_sha256: String,
    pub events_sha256: String,
    pub event_count: usize,
    pub exported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetRef {
    pub agent: String,
    pub session_id: String,
    #[serde(default)]
    pub file: String,
    /// Anchor message id: the last message injected by the importer, used to
    /// detect whether the target agent continued past the seed point.
    pub anchor_message_id: String,
    pub injected_at: String,
    pub events_sha256: String,
    #[serde(default)]
    pub seed_event_count: usize,
    #[serde(default)]
    pub native_message_count: usize,
    #[serde(default)]
    pub dropped_event_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncRef {
    pub source_agent: String,
    pub source_session_id: String,
    pub source_file: String,
    pub source_events_sha256: String,
    pub target_agent: String,
    pub target_session_id: String,
    pub target_file: String,
    pub target_anchor_message_id: String,
    pub target_events_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub seed_files: Vec<String>,
    pub source: SourceRef,
    pub target: Option<TargetRef>,
    #[serde(default)]
    pub sync: Option<SyncRef>,
}

/// Write seed.json (canonical IR), seed.md (markdown transcript) and
/// manifest.json into `out_dir`. Returns the manifest.
pub fn write_seed(trace: &Trace, out_dir: &Path) -> Result<Manifest, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;

    let existing_manifest = load_manifest(out_dir).ok().filter(|manifest| {
        manifest.source.agent == trace.meta.source.as_str()
            && manifest.source.session_id == trace.meta.session_id
    });
    let existing_target = existing_manifest
        .as_ref()
        .and_then(|manifest| manifest.target.clone());
    let existing_sync = existing_manifest.and_then(|manifest| {
        if manifest.source.events_sha256 == trace.meta.events_sha256 {
            manifest.sync
        } else {
            None
        }
    });

    let seed_json = out_dir.join("seed.json");
    let seed_md = out_dir.join("seed.md");
    let manifest_path = out_dir.join("manifest.json");

    let json_text = serde_json::to_string_pretty(trace).map_err(|e| e.to_string())?;
    std::fs::write(&seed_json, json_text)
        .map_err(|e| format!("write {}: {e}", seed_json.display()))?;

    let md = to_markdown(trace);
    std::fs::write(&seed_md, md).map_err(|e| format!("write {}: {e}", seed_md.display()))?;

    let manifest = Manifest {
        version: 1,
        seed_files: vec!["seed.json".into(), "seed.md".into()],
        source: SourceRef {
            agent: trace.meta.source.as_str().into(),
            session_id: trace.meta.session_id.clone(),
            file: trace.meta.file.clone(),
            file_sha256: trace.meta.source_file_sha256.clone(),
            events_sha256: trace.meta.events_sha256.clone(),
            event_count: trace.meta.event_count,
            exported_at: Utc::now().to_rfc3339(),
        },
        target: existing_target,
        sync: existing_sync,
    };
    let mjson = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(&manifest_path, mjson)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;

    Ok(manifest)
}

pub fn load_manifest(out_dir: &Path) -> Result<Manifest, String> {
    let p = out_dir.join("manifest.json");
    let raw = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", p.display()))
}

pub fn save_manifest(out_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let p = out_dir.join("manifest.json");
    let raw = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(&p, raw).map_err(|e| format!("write {}: {e}", p.display()))
}

pub fn hash_trace_events(events: &[Event]) -> String {
    let canonical = serde_json::to_string(events).unwrap_or_default();
    sha256_hex(&canonical)
}

/// Render the trace as a self-contained markdown transcript.
pub fn to_markdown(trace: &Trace) -> String {
    let m = &trace.meta;
    let mut s = String::new();
    s.push_str("# 执行轨迹 / Execution Trace\n\n");
    s.push_str(&format!(
        "- **源 agent**: `{}`\n- **session**: `{}`\n- **工作目录**: {}\n",
        m.source,
        m.session_id,
        m.cwd.clone().unwrap_or_default()
    ));
    if let Some(title) = &m.title {
        s.push_str(&format!("- **标题**: {title}\n"));
    }
    if let Some(model) = &m.model {
        s.push_str(&format!("- **模型**: {model}\n"));
    }
    s.push_str(&format!("- **事件数**: {}\n\n---\n\n", m.event_count));

    for ev in &trace.events {
        match &ev.kind {
            EventKind::UserMessage { text } => {
                s.push_str("## 用户\n\n");
                s.push_str(text.trim());
                s.push_str("\n\n");
            }
            EventKind::AssistantMessage { text } => {
                s.push_str("### 助手\n\n");
                s.push_str(text.trim());
                s.push_str("\n\n");
            }
            EventKind::Reasoning { text } => {
                s.push_str("> 🔒 推理过程（reasoning）\n>\n");
                for line in text.lines().take(30) {
                    s.push_str("> ");
                    s.push_str(line);
                    s.push('\n');
                }
                if text.lines().count() > 30 {
                    s.push_str("> ...（已截断）\n");
                }
                s.push('\n');
            }
            EventKind::ToolCall {
                id,
                name,
                arguments,
            } => {
                s.push_str(&format!("### 🛠 工具调用 `{name}` (id: {id})\n\n"));
                s.push_str("```json\n");
                let pretty = serde_json::from_str::<serde_json::Value>(arguments)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or_else(|| arguments.clone());
                s.push_str(&pretty);
                s.push_str("\n```\n\n");
            }
            EventKind::ToolResult {
                call_id,
                output,
                exit_code,
                error,
            } => {
                s.push_str(&format!("#### ↳ 结果 (call: {call_id})\n"));
                if let Some(code) = exit_code {
                    s.push_str(&format!("- exit code: `{code}`\n"));
                }
                if let Some(err) = error {
                    s.push_str(&format!("- error: {err}\n"));
                }
                s.push_str("\n```text\n");
                s.push_str(&truncate_md(output, 8000));
                s.push_str("\n```\n\n");
            }
            EventKind::ModelChange { provider, model } => {
                s.push_str(&format!(
                    "> 模型切换: {}\n\n",
                    model
                        .clone()
                        .or_else(|| provider.clone())
                        .unwrap_or_default()
                ));
            }
            EventKind::NativeRecord { record_type } => {
                s.push_str(&format!("> 原生记录 `{record_type}`（保留原文）\n\n"));
            }
        }
    }
    s
}

fn truncate_md(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}\n...[truncated {} chars]", s.chars().count())
    }
}
