//! The daemon-ensure concern: the socket probe, the stale-daemon shutdown, the
//! detached supervisor spawn, and the startup poll window.

use anyhow::Context as _;

use super::{anyhow, Command, Duration, Instant, Path, Result, Stdio};

const DAEMON_STARTUP_TIMEOUT_MS: u64 = 30_000;
const DAEMON_SHUTDOWN_WAIT_MS: u64 = 5_000;
/// Pause between daemon-startup probes (TS `ensureDaemonRunning` polls at
/// 25ms). A cold supervisor binds its socket ~39ms after the spawn, so the
/// poll interval quantizes every cold launch: the bind lands 0-interval
/// before the next probe, a pure wait on a floor of microseconds. The 1ms
/// interval caps that overshoot at <=1ms for the cost of ~40 failed
/// connects per cold launch (each ~us; bounded by the same 30s startup
/// budget, so a hung boot adds at most ~1k connects/s of syscall work,
/// not a spin). Overshoot tables: probe-grid record 20261001-034500 on the
/// bench repo. Timing-only: the probe itself, the 30s startup budget, and
/// the timeout error are unchanged.
const DAEMON_PROBE_INTERVAL_MS: u64 = 1;

enum DaemonProbe {
    Absent,
    /// A current supervisor answered; it runs at the carried placement.
    Current(pa_tui::daemon_client::DaemonHost),
    /// A supervisor answered with a different protocol/schema. The client rides
    /// boxed: its size is platform-dependent, so the box keeps the other arms small.
    Stale(Box<pa_tui::daemon_client::DaemonClient>),
}

/// Probe the socket once: connect, read the hello, and classify it.
async fn probe_daemon(socket_path: &Path) -> DaemonProbe {
    let Ok((client, _events)) = pa_tui::daemon_client::DaemonClient::connect(socket_path).await
    else {
        return DaemonProbe::Absent;
    };
    let hello = client.hello();
    let current = hello.get("protocol").and_then(|p| p.get("version"))
        == Some(&serde_json::json!(
            pa_types::daemon::DAEMON_PROTOCOL_VERSION
        ))
        && hello.get("schemaId") == Some(&serde_json::json!(pa_types::daemon::DAEMON_SCHEMA_ID));
    if current {
        let host = client.host();
        client.close();
        DaemonProbe::Current(host)
    } else {
        DaemonProbe::Stale(Box::new(client))
    }
}

/// Ensure a current daemon is listening on `socket_path`, spawning this
/// executable in `--mode daemon` when it is not (a stale idle daemon is
/// shut down, a busy one refuses replacement), and report where it runs.
///
/// # Errors
/// Returns an error when the executable path cannot be resolved, a stale
/// daemon refuses replacement, the supervisor cannot be spawned, or no
/// current daemon starts before the startup timeout.
pub async fn ensure_daemon_running(
    socket_path: &Path,
    spawn_cwd: &Path,
) -> Result<pa_tui::daemon_client::DaemonHost> {
    match probe_daemon(socket_path).await {
        DaemonProbe::Current(host) => return Ok(host),
        DaemonProbe::Stale(client) => shutdown_stale_daemon(*client, socket_path).await?,
        DaemonProbe::Absent => {}
    }
    let exe = std::env::current_exe().context("resolve the prime-agent executable")?;
    ensure_daemon_running_with(&exe, socket_path, spawn_cwd).await?;
    // This process spawned it (or lost the race to a local launcher).
    Ok(pa_tui::daemon_client::DaemonHost::Local)
}

/// [`ensure_daemon_running`] with an explicit supervisor executable (the
/// product path uses this process's own binary).
///
/// # Errors
/// Returns an error when the supervisor cannot be spawned or no current
/// daemon starts before the startup timeout.
pub async fn ensure_daemon_running_with(
    exe: &Path,
    socket_path: &Path,
    spawn_cwd: &Path,
) -> Result<()> {
    match probe_daemon(socket_path).await {
        DaemonProbe::Current(_) => return Ok(()),
        DaemonProbe::Stale(client) => {
            // A stale daemon appeared between the caller's check and here:
            // fall through to the spawn path after refusing busy ones.
            client.close();
        }
        DaemonProbe::Absent => {}
    }
    spawn_supervisor_detached(socket_path, spawn_cwd, exe)?;
    let deadline = Instant::now() + Duration::from_millis(DAEMON_STARTUP_TIMEOUT_MS);
    loop {
        match probe_daemon(socket_path).await {
            DaemonProbe::Current(_) => return Ok(()),
            DaemonProbe::Stale(client) => {
                // A concurrent launcher won the socket with a build whose protocol matches at
                // connect time but failed the schema check: re-probe before deciding.
                client.close();
            }
            DaemonProbe::Absent => {}
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "Timed out waiting for the Prime Agent daemon to start on {}. Run: prime-agent shutdown --force, then retry the original command.",
                socket_path.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(DAEMON_PROBE_INTERVAL_MS)).await;
    }
}

/// Shut a stale daemon down when no session is busy; a busy one refuses
/// replacement. A remote daemon is never replaced from here: the spawn that
/// follows a shutdown would start a local daemon on the forwarded path.
async fn shutdown_stale_daemon(
    client: pa_tui::daemon_client::DaemonClient,
    socket_path: &Path,
) -> Result<()> {
    if client.host() != pa_tui::daemon_client::DaemonHost::Local {
        let version = client
            .hello()
            .get("appVersion")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        client.close();
        return Err(anyhow!(
            "The remote Prime Agent daemon on {} runs an incompatible version ({version}). Update Prime Agent on the daemon's machine and restart its daemon, then retry.",
            socket_path.display()
        ));
    }
    let sessions = client
        .request_ok(pa_types::daemon::DaemonCommand::List {
            id: None,
            all: None,
            cwd: None,
            session_dir: None,
            include_client_owned: None,
            rest: serde_json::Map::default(),
        })
        .await;
    let busy = sessions.map_or(true, |data| {
        data.get("sessions")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|rows| {
                rows.iter()
                    .any(|row| row.get("isSessionActive") == Some(&serde_json::json!(true)))
            })
    });
    client.close();
    if busy {
        return Err(anyhow!(
            "An incompatible Prime Agent daemon is running on {}.\n\nRun:\n  prime-agent shutdown --force\n\nThen retry the original command (the running daemon has active work).",
            socket_path.display()
        ));
    }
    if let Ok((client, _)) = pa_tui::daemon_client::DaemonClient::connect(socket_path).await {
        let _ = client
            .request_ok(pa_types::daemon::DaemonCommand::Shutdown {
                id: None,
                force: None,
                rest: serde_json::Map::default(),
            })
            .await;
        client.close();
    }
    wait_for_socket_gone(socket_path).await;
    Ok(())
}

/// Wait until nothing accepts connections on the socket (bounded).
async fn wait_for_socket_gone(socket_path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_millis(DAEMON_SHUTDOWN_WAIT_MS);
    while Instant::now() < deadline {
        if !pa_daemon::socket::can_connect(socket_path, Duration::from_millis(250)).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Spawn a detached supervisor on `socket_path` (the child outlives this
/// CLI).
fn spawn_supervisor_detached(socket_path: &Path, spawn_cwd: &Path, exe: &Path) -> Result<()> {
    let mut command = Command::new(exe);
    command
        .args(["--mode", "daemon", "--daemon-socket"])
        .arg(socket_path)
        .current_dir(spawn_cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The binary entry strips any inherited worker handshake env, so the
    // spawned supervisor never starts in worker mode. A daemon must also not
    // share the launching TUI's terminal session: a session-wide terminal
    // cleanup could hang it up after the TUI exits; on Unix, setsid also
    // creates its own process group.
    #[cfg(unix)]
    pa_core::platform::process::set_new_session(&mut command);
    #[cfg(not(unix))]
    pa_core::platform::process::set_new_process_group(&mut command);
    command
        .spawn()
        .with_context(|| format!("spawn the Prime Agent daemon on {}", socket_path.display()))?;
    Ok(())
}
