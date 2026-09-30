//! Bridges filesystem events under the workspace root to LSP `workspace/didChangeWatchedFiles`.
//!
//! Only paths matching `*.rs`, `Cargo.toml`, or `Cargo.lock` are forwarded (after noise-path exclusion).
//! Pure metadata mutations (e.g. mtime or permissions) do not trigger notifications.

use std::path::Path;

use lsp_types::FileChangeType;
use lsp_types::FileEvent;
use lsp_types::Uri;
use notify::Event;
use notify::event::ModifyKind;

use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::document_uri_from_path;

/// Returns true if notify `event` is a content change touching at least one watched, non-noise path.
pub(crate) fn is_relevant_event(event: &Event) -> bool {
    file_change_type(event.kind).is_some()
        && event
            .paths
            .iter()
            .any(|p| !path_has_noise_component(p) && path_matches_rust_workspace_watch_filters(p))
}

fn file_change_type(kind: notify::EventKind) -> Option<FileChangeType> {
    match kind {
        notify::EventKind::Create(_) => Some(FileChangeType::CREATED),
        notify::EventKind::Modify(ModifyKind::Metadata(_)) => None,
        notify::EventKind::Modify(_) => Some(FileChangeType::CHANGED),
        notify::EventKind::Remove(_) => Some(FileChangeType::DELETED),
        notify::EventKind::Other => Some(FileChangeType::CHANGED),
        notify::EventKind::Any => Some(FileChangeType::CHANGED),
        notify::EventKind::Access(_) => Some(FileChangeType::CHANGED),
    }
}

/// Maps a notify `event` to LSP file events for watched paths under `workspace` (empty if none apply).
pub(crate) fn notify_event_to_lsp_changes(event: &Event, workspace: &Path) -> Vec<FileEvent> {
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
