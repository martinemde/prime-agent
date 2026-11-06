//! Kernel Python environment bootstrap: the public entry point [`ensure_kernel_python`]
//! resolves (and on first run builds) the venv that runs `python -m rlm.repl`,
//! deduplicating concurrent bootstraps; the venv and version-file machinery lives in
//! [`venv`], the runtime bootstrap code in [`runtime_code`], and the agent dir's
//! `system.py`/`user.py` startup scripts in [`startup_scripts`].

pub(crate) mod dir_lock;
mod disk_memo;
mod runtime_code;
mod startup_scripts;
pub(crate) mod venv;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context};

use dir_lock::acquire_bootstrap_lock;
pub use runtime_code::{
    build_kernel_bootstrap_code, build_rlm_bootstrap_code, parse_unavailable_python_skills,
    UnavailablePythonSkills, PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER,
};
pub(crate) use startup_scripts::{requirement_name, KERNEL_AGENT_DIR_ENV};
pub use startup_scripts::{KernelStartupScripts, StartupRequirements, StartupScript};
use venv::{
    bootstrap_venv, ensure_uv, expand_home, has_prime_agent_runtime, missing_python_requirements,
    missing_python_skill_import_labels, normalize_python_skills, requirements_recorded,
    resolve_writable_kernel_venv_dir, sync_python_requirements, sync_python_skills,
    BootstrapPythonSkill,
};
pub use venv::{invalidate_runtime_probe_cache, kernel_venv_dir, kernel_venv_python};
use venv::{kernel_base_ready, kernel_ready};

/// One Python skill the kernel should import at bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelPythonSkill {
    pub name: String,
    pub import_name: String,
    pub package_path: PathBuf,
    pub pyproject_path: PathBuf,
}

/// Progress callback for the bootstrap (`ensure_kernel_python`).
pub type KernelBootstrapProgressHandler = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
pub struct EnsureKernelPythonOptions {
    pub python_skills: Vec<KernelPythonSkill>,
    /// The startup scripts whose PEP 723 dependencies the venv pre-installs.
    pub startup: KernelStartupScripts,
    pub on_progress: Option<KernelBootstrapProgressHandler>,
}

impl EnsureKernelPythonOptions {
    pub(crate) fn report(&self, message: &str) {
        match &self.on_progress {
            Some(handler) => handler(message),
            None => eprintln!("{message}"),
        }
    }
}

fn format_bootstrap_failure(error: &anyhow::Error) -> anyhow::Error {
    let mut message = format!(
        "Failed to set up the Python kernel runtime. {error:#}\n\
         First-time setup needs the bundled local runtime and internet to install uv, Python, and default Python packages; once set up, Prime Agent runs offline. \
         An interrupted runtime upgrade needs network once more, so re-run this while online. \
         Set PRIME_AGENT_KERNEL_PYTHON to a Python with a current prime-agent-runtime and default Python packages installed to skip auto-bootstrap."
    );
    // The packaged exe-adjacent sidecar is the kernel runtime source; when it is missing everywhere
    // (packaged layout and source checkout), name it.
    if venv::packaged_runtime_dir().is_none() {
        let package = venv::package_dir();
        let _ = write!(message,
            "\nThe packaged prime-agent-runtime directory was not found (looked next to the executable at {} and PI_PACKAGE_DIR); reinstall prime-agent so the kernel runtime ships beside the binary.",
            package.display()
        );
    }
    anyhow!(message)
}

/// One in-flight bootstrap per unique options set, joined by concurrent callers.
type InFlightBootstrap = Option<(
    String,
    Arc<tokio::sync::Mutex<Option<anyhow::Result<PathBuf>>>>,
)>;

static IN_FLIGHT: Mutex<InFlightBootstrap> = Mutex::new(None);

/// Resolve the Python interpreter for the kernel: the `PRIME_AGENT_KERNEL_PYTHON`
/// override when valid, else the auto-bootstrapped venv python.
///
/// # Errors
///
/// Returns an error when the override Python is invalid or the venv bootstrap fails.
///
/// # Panics
///
/// The in-flight promise stores its outcome under the same lock, so the `expect` is unreachable.
pub async fn ensure_kernel_python(options: EnsureKernelPythonOptions) -> anyhow::Result<PathBuf> {
    let python_skills = normalize_python_skills(&options.python_skills);
    let requirements = options.startup.requirements();
    for error in &requirements.errors {
        options.report(&format!("Warning: {error}"));
    }
    let requirements = requirements.requirements;
    let key = [
        std::env::var("PRIME_AGENT_KERNEL_PYTHON").unwrap_or_default(),
        std::env::var("PRIME_AGENT_KERNEL_VENV").unwrap_or_default(),
        std::env::var("HOME").unwrap_or_default(),
        std::env::var("XDG_DATA_HOME").unwrap_or_default(),
        serde_json::to_string(&python_skills).unwrap_or_default(),
        serde_json::to_string(&requirements).unwrap_or_default(),
    ]
    .join("\u{0}");

    let shared = {
        let mut in_flight = IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match in_flight.as_ref() {
            Some((existing_key, promise)) if *existing_key == key => promise.clone(),
            _ => {
                let promise = Arc::new(tokio::sync::Mutex::new(None));
                *in_flight = Some((key, promise.clone()));
                promise
            }
        }
    };
    let mut guard = shared.lock().await;
    if guard.is_none() {
        *guard = Some(ensure_kernel_python_uncached(&options, &python_skills, &requirements).await);
    }
    let outcome = guard.take().expect("outcome was just stored");
    // Drop the registry entry so a later call re-validates instead of reusing
    // a cached rejection forever.
    let mut in_flight = IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(in_flight.as_ref(), Some((_, promise)) if Arc::ptr_eq(promise, &shared)) {
        *in_flight = None;
    }
    outcome
}

async fn ensure_kernel_python_uncached(
    options: &EnsureKernelPythonOptions,
    python_skills: &[BootstrapPythonSkill],
    requirements: &[String],
) -> anyhow::Result<PathBuf> {
    if let Ok(override_python) = std::env::var("PRIME_AGENT_KERNEL_PYTHON") {
        if !override_python.is_empty() {
            let python = expand_home(&override_python);
            let python_str = python.to_string_lossy().to_string();
            let mut missing = Vec::new();
            if !has_prime_agent_runtime(&python_str) {
                missing.push(
                    "a current prime-agent-runtime with callable rlm.spawn, rlm.create_session, rlm.host_request, rlm.progress_note, rlm.rename, rlm._set_completion_notices_wired, and explicit harness CRUD methods".to_string(),
                );
            }
            if missing.is_empty() {
                let missing_extras = missing_python_requirements(&python_str, requirements);
                if !missing_extras.is_empty() {
                    missing.push(format!(
                        "the Python packages system.py/user.py declare ({})",
                        missing_extras.join(", ")
                    ));
                }
            }
            if missing.is_empty() && !options.python_skills.is_empty() {
                let missing_skills =
                    missing_python_skill_import_labels(&python_str, &options.python_skills);
                if !missing_skills.is_empty() {
                    options.report(&format!(
                        "Warning: Python skills unavailable in PRIME_AGENT_KERNEL_PYTHON and will be disabled: {}",
                        missing_skills.join(", ")
                    ));
                }
            }
            if missing.is_empty() {
                return Ok(python);
            }
            return Err(anyhow!(
                "PRIME_AGENT_KERNEL_PYTHON points to a Python missing {}: {}",
                missing.join(" and "),
                python.display()
            ));
        }
    }

    let venv = resolve_writable_kernel_venv_dir()?;
    ensure_kernel_venv(options, python_skills, requirements, venv).await
}

async fn ensure_kernel_venv(
    options: &EnsureKernelPythonOptions,
    python_skills: &[BootstrapPythonSkill],
    requirements: &[String],
    venv: PathBuf,
) -> anyhow::Result<PathBuf> {
    let python = kernel_venv_python(&venv);
    let python_str = python.to_string_lossy().to_string();
    // Validate the local source before probing any installed runtime or
    // removing an existing venv. Missing payloads must fail closed.
    let source_dir =
        venv::require_runtime_source_dir().map_err(|error| format_bootstrap_failure(&error))?;
    let runtime_identity =
        venv::hash_runtime_source(&source_dir).map_err(|error| format_bootstrap_failure(&error))?;
    let ready = || {
        kernel_ready(&python_str, &venv, &runtime_identity, python_skills)
            && requirements_recorded(&venv, requirements)
    };
    if ready() {
        return Ok(python);
    }

    let release_lock = match acquire_bootstrap_lock(&venv).await {
        Ok(release_lock) => release_lock,
        Err(error) if dir_lock::lock_creation_denied(&error) => {
            return unwritable_venv(
                options,
                &python,
                &venv,
                &runtime_identity,
                requirements,
                &error,
            );
        }
        Err(error) => return Err(error),
    };
    let result = async {
        if ready() {
            return Ok(python);
        }
        if kernel_base_ready(&python_str, &venv, &runtime_identity) {
            let uv = ensure_uv()?;
            sync_python_skills(
                &uv,
                &venv,
                &python,
                &source_dir,
                &runtime_identity,
                python_skills,
                options,
            )
            .await?;
            sync_python_requirements(&uv, &venv, &python, &source_dir, requirements, options)
                .await?;
            return Ok(python);
        }
        let had_venv = venv.exists();
        options.report("› setting up python kernel (one-time, ~30s)…");
        if had_venv {
            options.report("rebuilding kernel venv");
            std::fs::remove_dir_all(&venv)
                .with_context(|| format!("removing {}", venv.display()))?;
        }
        bootstrap_venv(
            &venv,
            &source_dir,
            &runtime_identity,
            python_skills,
            requirements,
            options,
        )
        .await?;
        Ok(python)
    }
    .await;
    drop(release_lock);
    options.report("✓ ready");
    result.map_err(|error| format_bootstrap_failure(&error))
}

/// The venv's directory denies writes to this user, so the bootstrap lock cannot be
/// created: a base-ready venv degrades the way a `PRIME_AGENT_KERNEL_PYTHON` override
/// does (uncovered skills disabled with a warning, nothing installed), and anything
/// less is a setup error naming the venv and both env overrides.
fn unwritable_venv(
    options: &EnsureKernelPythonOptions,
    python: &std::path::Path,
    venv: &std::path::Path,
    runtime_identity: &str,
    requirements: &[String],
    lock_error: &anyhow::Error,
) -> anyhow::Result<PathBuf> {
    let python_str = python.to_string_lossy().to_string();
    if !kernel_base_ready(&python_str, venv, runtime_identity) {
        return Err(anyhow!(
            "cannot set up the Python kernel runtime: the kernel venv at {} is not writable by \
             this user and does not hold a current runtime. Set PRIME_AGENT_KERNEL_VENV to a \
             writable directory, or set PRIME_AGENT_KERNEL_PYTHON to a Python with a current \
             prime-agent-runtime and the default Python packages installed. ({lock_error:#})",
            venv.display()
        ));
    }
    let missing = missing_python_skill_import_labels(&python_str, &options.python_skills);
    if !missing.is_empty() {
        options.report(&format!(
            "Warning: Python skills unavailable in {} and will be disabled: {}",
            venv.display(),
            missing.join(", ")
        ));
    }
    if !requirements_recorded(venv, requirements) {
        options.report(&format!(
            "Warning: Python packages from system.py/user.py may be missing from {}, which is not writable: {}",
            venv.display(),
            requirements.join(", ")
        ));
    }
    Ok(python.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn lock_timeout_prevents_venv_rebuild() {
        let root = tempfile::tempdir().unwrap();
        let venv = root.path().join("kernel-venv");
        std::fs::create_dir(&venv).unwrap();
        let sentinel = venv.join("owner-state");
        std::fs::write(&sentinel, "another bootstrap owns this venv").unwrap();
        let owner = acquire_bootstrap_lock(&venv).await.unwrap();
        let options = EnsureKernelPythonOptions {
            // Fail before any destructive operation or network access if the
            // caller regresses and enters setup without acquiring the lock.
            on_progress: Some(Arc::new(|message| {
                panic!("bootstrap must stop at the lock error, got progress: {message}");
            })),
            ..Default::default()
        };

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(121),
            ensure_kernel_venv(&options, &[], &[], venv),
        )
        .await
        .expect("lock acquisition must have a bounded wait")
        .expect_err("a lock timeout must propagate to the caller");

        assert!(error.to_string().contains("Timed out"), "{error}");
        assert_eq!(
            std::fs::read_to_string(sentinel).unwrap(),
            "another bootstrap owns this venv"
        );
        assert!(root.path().join("kernel-venv.bootstrap.lock").exists());
        drop(owner);
    }
}
