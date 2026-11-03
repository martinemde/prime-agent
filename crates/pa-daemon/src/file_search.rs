//! `search_files`: the `@` file-completion walk for a client on another machine. The walk
//! itself lives in `pa_core::file_search`; this resolves the typed scope against the request's
//! cwd and this machine's home directory, then answers off the executor thread.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::protocol::{response_failure, response_line, response_success};

/// One `search_files` request.
pub(crate) struct SearchFilesRequest {
    pub(crate) cwd: String,
    pub(crate) scope: Option<String>,
    pub(crate) pattern: Option<String>,
    pub(crate) case_insensitive: bool,
    pub(crate) full_path: bool,
}

/// Answer one request as a response line: the walk `root` (`null` when the scope is not a
/// directory here) and the matched `entries`.
pub(crate) async fn respond(
    command_id: &str,
    type_name: &str,
    request: SearchFilesRequest,
) -> Value {
    let response = match tokio::task::spawn_blocking(move || search(&request)).await {
        Ok(Ok(data)) => response_success(Some(command_id), type_name, Some(data)),
        Ok(Err(error)) => response_failure(Some(command_id), type_name, &error, None),
        Err(error) => response_failure(Some(command_id), type_name, &error.to_string(), None),
    };
    response_line(&response)
}

fn search(request: &SearchFilesRequest) -> Result<Value, String> {
    let cwd = Path::new(&request.cwd);
    let root = match request.scope.as_deref() {
        Some(scope) => resolve_scope(cwd, scope)?,
        None => cwd.to_path_buf(),
    };
    if !root.is_dir() {
        return Ok(json!({ "root": null, "entries": [] }));
    }
    let entries = pa_core::file_search::walk_matching(
        &root,
        pa_core::file_search::FileMatch {
            pattern: request.pattern.as_deref(),
            case_insensitive: request.case_insensitive,
            full_path: request.full_path,
        },
    )?;
    Ok(json!({ "root": root.to_string_lossy(), "entries": entries }))
}

/// The typed scope as this machine reads it: `~/` under this home, absolute as given, anything
/// else below the request's cwd.
fn resolve_scope(cwd: &Path, scope: &str) -> Result<PathBuf, String> {
    let expanded = crate::paths::expand_tilde(scope).map_err(|error| error.to_string())?;
    Ok(if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    })
}
