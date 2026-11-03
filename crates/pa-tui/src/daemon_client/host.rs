//! Where the daemon behind a client runs: on this machine, or behind a
//! forwarded socket (an SSH stream-local forward, a bind-mounted socket in a
//! container). Paths the client sends — its cwd, its session directory, its
//! environment — only mean something to a daemon that shares its filesystem.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The daemon's placement relative to this client process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonHost {
    /// The daemon shares this process's filesystem: client paths are valid there.
    Local,
    /// The daemon's filesystem is not this one: the daemon owns the working
    /// directory, session directory, and environment of what it creates.
    /// `cwd` is the daemon's default working directory, when it reports one.
    Remote { cwd: Option<PathBuf> },
}

/// Classify the daemon from the socket this client connected to and the
/// socket path the daemon's hello reports binding. A daemon that reports no
/// path (an older build) counts as local, as does a socket this process
/// cannot stat (nothing to compare against).
#[must_use]
pub fn daemon_host(connected: &Path, hello: &Value) -> DaemonHost {
    let Some(bound) = hello
        .get("socketPath")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return DaemonHost::Local;
    };
    if Path::new(bound) == connected || same_file(connected, Path::new(bound)) {
        return DaemonHost::Local;
    }
    DaemonHost::Remote {
        cwd: hello
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from),
    }
}

/// Whether both paths name the same file. A `connected` path that cannot be
/// read answers `true` (no evidence of a separate filesystem).
#[cfg(unix)]
fn same_file(connected: &Path, bound: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(connected) = std::fs::metadata(connected) else {
        return true;
    };
    std::fs::metadata(bound)
        .is_ok_and(|bound| bound.dev() == connected.dev() && bound.ino() == connected.ino())
}

/// Named pipes have no filesystem identity to compare: every daemon is local.
#[cfg(not(unix))]
fn same_file(_connected: &Path, _bound: &Path) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_socket_that_is_not_the_daemons_own_is_remote() {
        let dir = tempfile::tempdir().unwrap();
        let bound = dir.path().join("daemon.sock");
        let link = dir.path().join("link.sock");
        let forwarded = dir.path().join("forwarded.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&bound).unwrap();
        let _forward = std::os::unix::net::UnixListener::bind(&forwarded).unwrap();
        std::os::unix::fs::symlink(&bound, &link).unwrap();
        let hello = json!({"socketPath": bound, "cwd": "/home/coder/repo"});
        let hosts = [&bound, &link, &forwarded].map(|socket| daemon_host(socket, &hello));
        assert_eq!(
            hosts,
            [
                DaemonHost::Local,
                DaemonHost::Local,
                DaemonHost::Remote {
                    cwd: Some(PathBuf::from("/home/coder/repo"))
                },
            ]
        );
    }
}
