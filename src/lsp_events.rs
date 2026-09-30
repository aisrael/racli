//! Fan-out hub for server-to-client LSP notifications from rust-analyzer (e.g. diagnostics).

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::broadcast;

use crate::proto::racli::LspEvent;

/// LSP method whose latest notification per document is cached for late subscribers.
pub const PUBLISH_DIAGNOSTICS: &str = "textDocument/publishDiagnostics";

/// rust-analyzer's health/quiescence notification; its latest value is cached for late subscribers.
pub const SERVER_STATUS: &str = "experimental/serverStatus";

/// Server-to-client notification methods forwarded from rust-analyzer to [`LspEvents`] subscribers.
pub const FORWARDED_METHODS: [&str; 4] = [
    PUBLISH_DIAGNOSTICS,
    SERVER_STATUS,
    "window/showMessage",
    "window/logMessage",
];

/// Capacity of the broadcast channel; a subscriber lagging further behind skips events.
const CHANNEL_CAPACITY: usize = 1024;

/// Broadcasts LSP notifications and remembers the latest state-carrying ones (diagnostics per
/// document URI, server status) so late subscribers start from the current state.
pub struct LspEvents {
    tx: broadcast::Sender<LspEvent>,
    /// Latest cached events, keyed by document URI (diagnostics) or method name (server status).
    latest: Mutex<HashMap<String, LspEvent>>,
}

impl Default for LspEvents {
    fn default() -> Self {
        Self {
            tx: broadcast::channel(CHANNEL_CAPACITY).0,
            latest: Mutex::new(HashMap::new()),
        }
    }
}

/// Returns the cache key for a state-carrying notification, or `None` if it isn't cached.
fn cache_key(method: &str, params: &serde_json::Value) -> Option<String> {
    match method {
        PUBLISH_DIAGNOSTICS => params
            .get("uri")
            .and_then(|u| u.as_str())
            .map(str::to_string),
        SERVER_STATUS => Some(SERVER_STATUS.to_string()),
        _ => None,
    }
}

impl LspEvents {
    /// Records `params` (if it carries state) and broadcasts it to current subscribers.
    pub fn publish(&self, method: &str, params: &serde_json::Value) {
        let event = LspEvent {
            method: method.to_string(),
            params_json: params.to_string(),
        };
        // Hold the lock while sending so `subscribe` never misses or duplicates a cached update.
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = cache_key(method, params) {
            latest.insert(key, event.clone());
        }
        let _ = self.tx.send(event);
    }

    /// Returns the cached events followed by a receiver for all later events.
    pub fn subscribe(&self) -> (Vec<LspEvent>, broadcast::Receiver<LspEvent>) {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        (latest.values().cloned().collect(), self.tx.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn late_subscriber_gets_latest_diagnostics_per_uri() {
        let events = LspEvents::default();
        events.publish(
            PUBLISH_DIAGNOSTICS,
            &json!({"uri": "file:///a", "diagnostics": [1]}),
        );
        events.publish(
            PUBLISH_DIAGNOSTICS,
            &json!({"uri": "file:///a", "diagnostics": [2]}),
        );
        events.publish("window/logMessage", &json!({"type": 3, "message": "hi"}));

        let (snapshot, _rx) = events.subscribe();
        assert_eq!(snapshot.len(), 1);
        let params: serde_json::Value = serde_json::from_str(&snapshot[0].params_json).unwrap();
        assert_eq!(params["diagnostics"], json!([2]));
    }

    #[test]
    fn late_subscriber_gets_latest_server_status() {
        let events = LspEvents::default();
        events.publish(SERVER_STATUS, &json!({"health": "ok", "quiescent": false}));
        events.publish(SERVER_STATUS, &json!({"health": "ok", "quiescent": true}));

        let (snapshot, _rx) = events.subscribe();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].method, SERVER_STATUS);
        let params: serde_json::Value = serde_json::from_str(&snapshot[0].params_json).unwrap();
        assert_eq!(params["quiescent"], json!(true));
    }

    #[tokio::test]
    async fn subscriber_receives_later_events() {
        let events = LspEvents::default();
        let (snapshot, mut rx) = events.subscribe();
        assert!(snapshot.is_empty());
        events.publish("window/showMessage", &json!({"type": 1, "message": "x"}));
        let event = rx.recv().await.unwrap();
        assert_eq!(event.method, "window/showMessage");
    }
}
