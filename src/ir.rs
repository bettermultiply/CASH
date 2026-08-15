use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Codex,
    OpenCode,
    Pi,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
            AgentKind::Pi => "pi",
        }
    }
}

impl std::str::FromStr for AgentKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "codex" => Ok(AgentKind::Codex),
            "opencode" => Ok(AgentKind::OpenCode),
            "pi" => Ok(AgentKind::Pi),
            _ => Err(format!("unknown agent {s:?} (expected codex|opencode|pi)")),
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMeta {
    pub source: AgentKind,
    pub session_id: String,
    /// Original storage file / source identifier the trace was read from.
    pub file: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    /// Epoch millis.
    pub started_at: Option<i64>,
    /// Epoch millis.
    pub ended_at: Option<i64>,
    /// SHA-256 of the raw source file the trace was extracted from.
    pub source_file_sha256: String,
    /// SHA-256 over the canonical serialization of `events`.
    pub events_sha256: String,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub meta: TraceMeta,
    pub events: Vec<Event>,
}

/// One entry in the unified, portable execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Epoch millis when the event happened, when the source exposes it.
    pub time: Option<i64>,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    UserMessage {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    /// Chain-of-thought / reasoning (never shown to the user in the UI).
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        /// Raw tool arguments, preserved verbatim as JSON text.
        arguments: String,
    },
    ToolResult {
        call_id: String,
        output: String,
        exit_code: Option<i32>,
        error: Option<String>,
    },
    ModelChange {
        provider: Option<String>,
        model: Option<String>,
    },
}
