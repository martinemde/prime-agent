//! The `@` file search against a remote daemon's filesystem: the daemon walks
//! (`search_files`, advertised as `file_search`), this client ranks the entries exactly as
//! it ranks a local walk. Prior art: the Codex app-server's `fuzzyFileSearch`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use pa_types::daemon::DaemonCommand;
use serde_json::Value;

use super::fuzzy_file_search::{
    fd_case_insensitive, fd_pattern, spawn_search, split_scope, suggestions, top_ranked,
    with_search, TypedQuery,
};
use super::{parse_path_prefix, SuggestionLookup, Suggestions};
use crate::daemon_client::{DaemonClient, DaemonHost};

/// The daemon capability that serves `search_files`.
const CAPABILITY: &str = "file_search";
/// One walk's response budget: a slow remote walk drops the menu rather than holding it.
const SEARCH_TIMEOUT_MS: u64 = 5_000;

/// The connection `@` searches walk through. It is the search's own, opened on first use,
/// so a daemon restart that replaces the session's connection costs one reconnect here.
#[derive(Clone)]
pub struct RemoteFiles {
    socket_path: PathBuf,
    runtime: tokio::runtime::Handle,
    client: Arc<tokio::sync::Mutex<Option<DaemonClient>>>,
}

impl std::fmt::Debug for RemoteFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteFiles")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

/// A walk's answer: the daemon's resolved walk root and its entries, or the typed scope
/// missing there.
enum Walk {
    Found { root: PathBuf, entries: Vec<String> },
    ScopeMissing,
}

impl RemoteFiles {
    /// The remote search for `client`'s daemon: `None` for a local daemon (this disk is its
    /// disk), one without `file_search`, or a call outside a tokio runtime.
    #[must_use]
    pub fn for_client(client: &DaemonClient) -> Option<Self> {
        if client.host() == DaemonHost::Local || !client.supports_server_capability(CAPABILITY) {
            return None;
        }
        Some(Self {
            socket_path: client.socket_path().to_path_buf(),
            runtime: tokio::runtime::Handle::try_current().ok()?,
            client: Arc::default(),
        })
    }

    /// Start the search for the typed `@` token below `base` (a daemon-side path).
    pub(super) fn lookup(&self, base: &Path, at_prefix: String) -> SuggestionLookup {
        let files = self.clone();
        let base = base.to_path_buf();
        SuggestionLookup::Searching(spawn_search(move |cancel| {
            files.search(&base, &at_prefix, cancel)
        }))
    }

    /// The walk-thread body: walk the typed scope on the daemon — the whole query from
    /// `base` when the scope is missing there, as the local search does — and rank.
    fn search(&self, base: &Path, at_prefix: &str, cancel: &AtomicBool) -> Option<Suggestions> {
        let (raw_query, _is_at_prefix, is_quoted_prefix) = parse_path_prefix(at_prefix);
        let scoped = split_scope(&raw_query);
        let scoped_walk = match &scoped {
            Some((display_base, query)) => match self.walk(base, Some(display_base), query)? {
                Walk::Found { root, entries } => {
                    Some((root, entries, Some(display_base.as_str()), query.as_str()))
                }
                Walk::ScopeMissing => None,
            },
            None => None,
        };
        let (root, entries, display_base, query) = match scoped_walk {
            Some(found) => found,
            None => match self.walk(base, None, &raw_query)? {
                Walk::Found { root, entries } => (root, entries, None, raw_query.as_str()),
                Walk::ScopeMissing => return None,
            },
        };
        let typed = TypedQuery {
            at_prefix,
            is_quoted_prefix,
            display_base,
            query,
        };
        with_search(&typed, &root, cancel, |search| {
            suggestions(
                top_ranked(entries.iter().filter_map(|form| search.score(form))),
                search,
            )
        })
    }

    /// One `search_files` round trip; `None` when the daemon cannot answer (a dead
    /// connection is replaced once).
    fn walk(&self, base: &Path, scope: Option<&str>, query: &str) -> Option<Walk> {
        let (pattern, full_path) = fd_pattern(query);
        let command = DaemonCommand::SearchFiles {
            id: None,
            cwd: base.to_string_lossy().into_owned(),
            scope: scope.map(str::to_string),
            case_insensitive: fd_case_insensitive(&pattern),
            pattern: (!pattern.is_empty()).then_some(pattern),
            full_path,
            rest: serde_json::Map::default(),
        };
        let data = self.runtime.block_on(async {
            let mut slot = self.client.lock().await;
            for _attempt in 0..2 {
                if slot.is_none() {
                    // The search connection's events are never read: dropping the receiver
                    // only discards them.
                    let (client, _events) = DaemonClient::connect(&self.socket_path).await.ok()?;
                    *slot = Some(client);
                }
                let client = slot.as_ref()?;
                match client
                    .request_with_timeout(command.clone(), SEARCH_TIMEOUT_MS)
                    .await
                {
                    Ok(response) if response.success => return response.data,
                    Ok(_) => return None,
                    Err(_) => {
                        if let Some(dead) = slot.take() {
                            dead.close();
                        }
                    }
                }
            }
            None
        })?;
        let Some(root) = data.get("root").and_then(Value::as_str) else {
            return Some(Walk::ScopeMissing);
        };
        let entries = data
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(Walk::Found {
            root: PathBuf::from(root),
            entries,
        })
    }
}
