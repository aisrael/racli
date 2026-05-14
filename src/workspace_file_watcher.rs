//! Bridges filesystem events under the workspace root to LSP `workspace/didChangeWatchedFiles`.
//!
//! Only paths matching `*.rs`, `Cargo.toml`, or `Cargo.lock` are forwarded (after noise-path exclusion).
//! Pure metadata mutations (e.g. mtime or permissions) do not trigger notifications.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use lsp_types::DidChangeWatchedFilesParams;
use lsp_types::FileChangeType;
use lsp_types::FileEvent;
use lsp_types::Uri;
use notify::Event;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use notify::event::ModifyKind;
use tokio::sync::Mutex;

use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::RustAnalyzerSession;
use crate::rust_analyzer::document_uri_from_path;

/// Owns the notify thread and the tokio task that forwards events to rust-analyzer; call [`Self::stop`] before LSP shutdown.
pub struct WorkspaceFileWatcherHandle {
    shutdown_tx: std::sync::mpsc::Sender<()>,
    notify_thread: Option<std::thread::JoinHandle<()>>,
    forward_task: tokio::task::JoinHandle<()>,
}

impl WorkspaceFileWatcherHandle {
    /// Aborts LSP forwarding, stops the watcher thread, and waits for it to finish.
    pub async fn stop(mut self) {
        self.forward_task.abort();
        let _ = self.forward_task.await;
        let _ = self.shutdown_tx.send(());
        if let Some(join) = self.notify_thread.take() {
            let _ = join.join();
        }
    }
}

/// Starts recursive filesystem watching under `workspace_root` and sends `workspace/didChangeWatchedFiles` for `*.rs`, `Cargo.toml`, and `Cargo.lock` paths only.
pub(crate) fn spawn_workspace_file_watcher(
    workspace_root: PathBuf,
    session: Arc<Mutex<RustAnalyzerSession>>,
) -> WorkspaceFileWatcherHandle {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();

    let root_for_thread = workspace_root.clone();
    let notify_thread = match std::thread::Builder::new()
        .name("racli-notify".to_owned())
        .spawn(move || run_notify_thread(root_for_thread, event_tx, shutdown_rx))
    {
        Ok(j) => Some(j),
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn racli-notify thread");
            None
        }
    };

    let forward_root = workspace_root;
    let forward_task = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            tracing::debug!(
                kind = ?event.kind,
                paths = ?event.paths,
                "workspace file watcher detected filesystem change"
            );
            let changes = notify_event_to_lsp_changes(&event, &forward_root);
            if changes.is_empty() {
                tracing::debug!(
                    kind = ?event.kind,
                    paths = ?event.paths,
                    "filesystem change produced no LSP file events after filtering"
                );
                continue;
            }
            let params = DidChangeWatchedFilesParams { changes };
            let mut guard = session.lock().await;
            if let Err(e) = guard.notify_did_change_watched_files(params).await {
                tracing::warn!(
                    error = %e,
                    "workspace/didChangeWatchedFiles notification failed"
                );
            }
        }
    });

    WorkspaceFileWatcherHandle {
        shutdown_tx,
        notify_thread,
        forward_task,
    }
}

fn run_notify_thread(
    root: PathBuf,
    event_tx: tokio::sync::mpsc::UnboundedSender<Event>,
    shutdown_rx: std::sync::mpsc::Receiver<()>,
) {
    let tx = event_tx.clone();
    let watcher_result: notify::Result<RecommendedWatcher> =
        notify::recommended_watcher(move |res: notify::Result<Event>| match res {
            Ok(event) => {
                if file_change_type(event.kind).is_some() {
                    let relevant = event.paths.iter().any(|p| {
                        !path_has_noise_component(p) && path_matches_rust_workspace_watch_filters(p)
                    });
                    if relevant {
                        let _ = tx.send(event);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "notify watcher error"),
        });

    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "failed to create notify watcher");
            return;
        }
    };

    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        tracing::error!(
            error = %e,
            path = %root.display(),
            "failed to watch workspace root"
        );
        return;
    }

    tracing::debug!(path = %root.display(), "workspace file watcher active");
    let _ = shutdown_rx.recv();
    drop(watcher);
}

fn file_change_type(kind: notify::EventKind) -> Option<FileChangeType> {
    match kind {
        notify::EventKind::Create(_) => Some(FileChangeType::CREATED),
        notify::EventKind::Modify(mk) if matches!(mk, ModifyKind::Metadata(_)) => None,
        notify::EventKind::Modify(_) => Some(FileChangeType::CHANGED),
        notify::EventKind::Remove(_) => Some(FileChangeType::DELETED),
        notify::EventKind::Other => Some(FileChangeType::CHANGED),
        notify::EventKind::Any => Some(FileChangeType::CHANGED),
        notify::EventKind::Access(_) => Some(FileChangeType::CHANGED),
    }
}

fn notify_event_to_lsp_changes(event: &Event, workspace: &Path) -> Vec<FileEvent> {
    let Some(typ) = file_change_type(event.kind) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in &event.paths {
        if path_has_noise_component(path) {
            continue;
        }
        if !path_matches_rust_workspace_watch_filters(path) {
            continue;
        }
        match path_to_file_event(path, workspace, typ) {
            Ok(fe) => out.push(fe),
            Err(e) => tracing::warn!(
                error = %e,
                path = %path.display(),
                "skipped path for workspace/didChangeWatchedFiles"
            ),
        }
    }
    out
}

/// Returns true for Rust sources and workspace manifest files we notify rust-analyzer about.
fn path_matches_rust_workspace_watch_filters(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if name == "Cargo.toml" || name == "Cargo.lock" {
        return true;
    }
    path.extension().is_some_and(|ext| ext == "rs")
}

/// Skips common large or irrelevant subtrees so rust-analyzer is not flooded during builds (`target/`, etc.).
fn path_has_noise_component(path: &Path) -> bool {
    const NOISE: &[&str] = &["target", ".git", "node_modules"];
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(x) => Some(x),
            _ => None,
        })
        .any(|part| NOISE.iter().any(|n| part == *n))
}

fn path_to_file_event(
    path: &Path,
    workspace: &Path,
    typ: FileChangeType,
) -> Result<FileEvent, RustAnalyzerError> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    let for_uri = std::fs::canonicalize(&abs).unwrap_or(abs);
    let uri_str = document_uri_from_path(&for_uri)?;
    let uri: Uri = uri_str
        .parse()
        .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
    Ok(FileEvent { uri, typ })
}
