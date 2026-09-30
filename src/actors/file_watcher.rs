//! Actor that watches the workspace with `notify` and forwards changes to rust-analyzer.

use std::path::PathBuf;

use lsp_types::DidChangeWatchedFilesParams;
use notify::Event;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;

use crate::actors::rust_analyzer::RustAnalyzerMsg;
use crate::workspace_file_watcher::is_relevant_event;
use crate::workspace_file_watcher::notify_event_to_lsp_changes;

/// Messages handled by [`FileWatcherActor`].
pub(crate) enum FileWatcherMsg {
    /// A relevant filesystem event from the notify callback.
    Fs(Event),
}

/// Startup arguments for [`FileWatcherActor`].
pub(crate) struct FileWatcherArgs {
    /// Directory watched recursively.
    pub workspace_root: PathBuf,
    /// Receiver of `workspace/didChangeWatchedFiles` notifications.
    pub rust_analyzer: ActorRef<RustAnalyzerMsg>,
}

/// State of [`FileWatcherActor`]; dropping `_watcher` stops notify's background thread.
pub(crate) struct FileWatcherState {
    workspace_root: PathBuf,
    rust_analyzer: ActorRef<RustAnalyzerMsg>,
    _watcher: Option<RecommendedWatcher>,
}

/// Bridges `notify` events for `*.rs`, `Cargo.toml`, and `Cargo.lock` to LSP `workspace/didChangeWatchedFiles`.
pub(crate) struct FileWatcherActor;

impl Actor for FileWatcherActor {
    type Msg = FileWatcherMsg;
    type State = FileWatcherState;
    type Arguments = FileWatcherArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Watch failures are logged, not fatal: requests still work, just without change notifications.
        let watcher = start_watcher(&args.workspace_root, myself);
        Ok(FileWatcherState {
            workspace_root: args.workspace_root,
            rust_analyzer: args.rust_analyzer,
            _watcher: watcher,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let FileWatcherMsg::Fs(event) = message;
        tracing::debug!(
            kind = ?event.kind,
            paths = ?event.paths,
            "workspace file watcher detected filesystem change"
        );
        let changes = notify_event_to_lsp_changes(&event, &state.workspace_root);
        if changes.is_empty() {
            tracing::debug!(
                kind = ?event.kind,
                paths = ?event.paths,
                "filesystem change produced no LSP file events after filtering"
            );
            return Ok(());
        }
        let params = DidChangeWatchedFilesParams { changes };
        if let Err(e) = state
            .rust_analyzer
            .cast(RustAnalyzerMsg::DidChangeWatchedFiles(params))
        {
            tracing::debug!(error = %e, "rust-analyzer actor unavailable; dropping file change");
        }
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        state._watcher = None;
        tracing::debug!("workspace file watcher stopped");
        Ok(())
    }
}

/// Creates a recursive notify watcher on `root` whose callback casts relevant events to `myself`.
fn start_watcher(
    root: &std::path::Path,
    myself: ActorRef<FileWatcherMsg>,
) -> Option<RecommendedWatcher> {
    let watcher_result = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            if is_relevant_event(&event) {
                let _ = myself.cast(FileWatcherMsg::Fs(event));
            }
        }
        Err(e) => tracing::warn!(error = %e, "notify watcher error"),
    });
    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "failed to create notify watcher");
            return None;
        }
    };
    if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
        tracing::error!(
            error = %e,
            path = %root.display(),
            "failed to watch workspace root"
        );
        return None;
    }
    tracing::debug!(path = %root.display(), "workspace file watcher active");
    Some(watcher)
}
