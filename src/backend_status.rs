//! The backend's readiness, sent with every gRPC response as metadata (ADR 004).

use serde_json::Value;
use tonic::metadata::MetadataMap;
use tonic::metadata::MetadataValue;

/// gRPC metadata key carrying the backend's state: `ready`, `indexing` or `error`.
pub const BACKEND_STATUS_KEY: &str = "racli-backend";

/// gRPC binary metadata key carrying rust-analyzer's message when the backend's state is `error`.
pub const BACKEND_MESSAGE_KEY: &str = "racli-backend-message-bin";

/// Whether rust-analyzer had finished loading and indexing the workspace when a response was produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendStatus {
    /// rust-analyzer has loaded the workspace and has no pending work.
    Ready,
    /// rust-analyzer is still starting, loading or indexing, so answers may be incomplete.
    Indexing,
    /// rust-analyzer reported an error, such as a workspace that failed to load.
    Error(String),
    /// The server sent no status, as a racli from before ADR 004 doesn't.
    Unknown,
}

impl BackendStatus {
    /// The status from rust-analyzer's latest `experimental/serverStatus` params; `Indexing` before the first one.
    pub fn from_server_status(params: Option<&Value>) -> Self {
        let Some(params) = params else {
            return Self::Indexing;
        };
        if params["health"] == "error" {
            let message = params["message"].as_str().unwrap_or_default();
            Self::Error(message.to_string())
        } else if params["quiescent"] == true {
            Self::Ready
        } else {
            Self::Indexing
        }
    }

    /// Adds this status to a response's `metadata`; `Unknown` adds nothing.
    pub fn insert_into(&self, metadata: &mut MetadataMap) {
        let value = match self {
            Self::Ready => "ready",
            Self::Indexing => "indexing",
            Self::Error(_) => "error",
            Self::Unknown => return,
        };
        metadata.insert(BACKEND_STATUS_KEY, MetadataValue::from_static(value));
        if let Self::Error(message) = self {
            metadata.insert_bin(
                BACKEND_MESSAGE_KEY,
                MetadataValue::from_bytes(message.as_bytes()),
            );
        }
    }

    /// The status in a response's `metadata`; `Unknown` if it has none or one this racli doesn't know.
    pub fn from_metadata(metadata: &MetadataMap) -> Self {
        let value = metadata
            .get(BACKEND_STATUS_KEY)
            .and_then(|v| v.to_str().ok());
        match value {
            Some("ready") => Self::Ready,
            Some("indexing") => Self::Indexing,
            Some("error") => {
                let message = metadata
                    .get_bin(BACKEND_MESSAGE_KEY)
                    .and_then(|v| v.to_bytes().ok())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                Self::Error(message)
            }
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn server_status_maps_quiescence_and_health() {
        assert_eq!(
            BackendStatus::from_server_status(None),
            BackendStatus::Indexing
        );
        let status = |v: Value| BackendStatus::from_server_status(Some(&v));
        assert_eq!(
            status(json!({"health": "ok", "quiescent": false})),
            BackendStatus::Indexing
        );
        assert_eq!(
            status(json!({"health": "ok", "quiescent": true})),
            BackendStatus::Ready
        );
        assert_eq!(
            status(json!({"health": "warning", "quiescent": true})),
            BackendStatus::Ready
        );
        assert_eq!(
            status(json!({"health": "error", "quiescent": true, "message": "no workspace"})),
            BackendStatus::Error("no workspace".into())
        );
    }

    #[test]
    fn metadata_round_trips() {
        for status in [
            BackendStatus::Ready,
            BackendStatus::Indexing,
            BackendStatus::Error("failed to load\nworkspace ✗".into()),
        ] {
            let mut metadata = MetadataMap::new();
            status.insert_into(&mut metadata);
            assert_eq!(BackendStatus::from_metadata(&metadata), status);
        }
        assert_eq!(
            BackendStatus::from_metadata(&MetadataMap::new()),
            BackendStatus::Unknown
        );
    }
}
