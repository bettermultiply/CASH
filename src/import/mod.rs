#[derive(Debug, Clone)]
pub struct ImportResult {
    pub session_id: String,
    pub file: String,
    pub anchor_message_id: String,
    pub message_count: usize,
    pub dropped_event_count: usize,
}

pub mod opencode;
pub mod pi;
