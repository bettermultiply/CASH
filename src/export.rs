use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::ir::{Event, EventKind, Trace};
use crate::util::sha256_hex;

/// 一个对等副本节点：同一个逻辑 session 在某个 agent 中的一份拷贝。
/// 组内所有节点地位完全相同，没有顺序或层级语义。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeRef {
    pub agent: String,
    pub session_id: String,
    pub file: String,
    /// 上次转换/同步时该副本的最后一条事件 id，用于计算增量与检测续写。
    pub anchor_message_id: String,
    /// 该副本在锚点处的 events 哈希。
    pub events_sha256: String,
    // ---- 导出元数据（seed 注册节点携带） ----
    pub file_sha256: String,
    pub event_count: usize,
    pub exported_at: String,
    // ---- 导入元数据（convert 生成的副本携带） ----
    pub injected_at: String,
    pub seed_event_count: usize,
    pub native_message_count: usize,
    pub dropped_event_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub version: u32,
    pub seed_files: Vec<String>,
    /// 对等副本组：同一逻辑 session 的全部拷贝，无顺序。`sync` 把单条变更
    /// 副本的增量广播到组内其余所有副本。
    pub nodes: Vec<NodeRef>,
}

impl Manifest {
    /// 组内全部对等副本。
    pub fn copies(&self) -> Vec<NodeRef> {
        self.nodes.clone()
    }

    /// 查找组内与 `agent`/`session_id` 匹配的副本。
    pub fn find_node(&self, agent: &str, session_id: &str) -> Option<&NodeRef> {
        self.nodes
            .iter()
            .find(|node| node.agent == agent && node.session_id == session_id)
    }

    /// 更新组内匹配副本的同步锚点与事件哈希。
    pub fn update_node(&mut self, agent: &str, session_id: &str, anchor: &str, events_sha256: &str) {
        if let Some(node) = self
            .nodes
            .iter_mut()
            .find(|node| node.agent == agent && node.session_id == session_id)
        {
            node.anchor_message_id = anchor.to_string();
            node.events_sha256 = events_sha256.to_string();
        }
    }

    /// 插入或替换组内一个副本（按 agent + session id 匹配）。
    pub fn upsert_node(&mut self, node: NodeRef) {
        if let Some(existing) = self
            .nodes
            .iter_mut()
            .find(|n| n.agent == node.agent && n.session_id == node.session_id)
        {
            *existing = node;
        } else {
            self.nodes.push(node);
        }
    }
}

/// Build the registration copy for a freshly exported trace.
pub fn node_from_trace(trace: &Trace) -> NodeRef {
    NodeRef {
        agent: trace.meta.source.as_str().into(),
        session_id: trace.meta.session_id.clone(),
        file: trace.meta.file.clone(),
        anchor_message_id: trace
            .events
            .last()
            .map(|event| event.original_id.clone())
            .unwrap_or_default(),
        events_sha256: trace.meta.events_sha256.clone(),
        file_sha256: trace.meta.source_file_sha256.clone(),
        event_count: trace.meta.event_count,
        exported_at: Utc::now().to_rfc3339(),
        ..Default::default()
    }
}

/// Write seed.json (canonical IR), seed.md (markdown transcript) and
/// manifest.json into `out_dir`. Returns the manifest.
pub fn write_seed(trace: &Trace, out_dir: &Path) -> Result<Manifest, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;

    let existing = load_manifest(out_dir).ok();
    let mut nodes = existing.as_ref().map(|m| m.nodes.clone()).unwrap_or_default();
    if !nodes
        .iter()
        .any(|node| node.agent == trace.meta.source.as_str() && node.session_id == trace.meta.session_id)
    {
        nodes.push(node_from_trace(trace));
    }

    write_trace_files(trace, out_dir)?;

    let manifest = Manifest {
        version: 1,
        seed_files: vec!["seed.json".into(), "seed.md".into()],
        nodes,
        ..Default::default()
    };
    save_manifest(out_dir, &manifest)?;

    Ok(manifest)
}

/// Write the seed data files (seed.json + seed.md) without touching the
/// manifest, used when refreshing a seed during a chain-extending convert.
pub fn write_trace_files(trace: &Trace, out_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let seed_json = out_dir.join("seed.json");
    let seed_md = out_dir.join("seed.md");

    let json_text = serde_json::to_string_pretty(trace).map_err(|e| e.to_string())?;
    std::fs::write(&seed_json, json_text)
        .map_err(|e| format!("write {}: {e}", seed_json.display()))?;

    let md = to_markdown(trace);
    std::fs::write(&seed_md, md).map_err(|e| format!("write {}: {e}", seed_md.display()))?;
    Ok(())
}

pub fn load_manifest(out_dir: &Path) -> Result<Manifest, String> {
    let p = out_dir.join("manifest.json");
    let raw = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", p.display()))?;
    Ok(manifest)
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
