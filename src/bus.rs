//! The one event stream. Everything that changes (a message, a thread's status, an agent's live output, a terminal's
//! bytes) is published here; the UI's web sockets, the log and remote clients all subscribe to the same stream.

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// What happened: "message", "thread", "activity", "project", "memory", "artifact", "agents", "term", ...
    pub kind: String,
    /// The project it belongs to, when it belongs to one (clients filter on it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    pub data: Value,
    pub at: i64,
}

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Event>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Bus { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn emit(&self, kind: &str, project: Option<&str>, thread: Option<&str>, data: Value) {
        let ev = Event {
            kind: kind.to_string(),
            project: project.map(str::to_string),
            thread: thread.map(str::to_string),
            data,
            at: crate::util::now_ms(),
        };
        if !matches!(kind, "delta" | "term") {
            tracing::debug!(target: "bus", kind, project = ?ev.project, thread = ?ev.thread);
        }
        let _ = self.tx.send(ev);
    }
}
