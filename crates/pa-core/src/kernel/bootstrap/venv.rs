//! Venv discovery, build, lock, and the shared `.bootstrap-version` cache:
//! the machine state behind [`super::ensure_kernel_python`]. The version
//! file is a cross-session cache, not a per-session manifest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use anyhow::{anyhow, Context};
use sha2::Digest;

use super::{EnsureKernelPythonOptions, KernelPythonSkill};

// The concern children (the flows + the shared record stay in the
// composition root).
mod layout;
mod probe;
mod runtime_source;
mod skills;
mod uv;
mod version;

use layout::home_dir;
pub(crate) use layout::{expand_home, resolve_writable_kernel_venv_dir};
pub use layout::{kernel_venv_dir, kernel_venv_python};
pub use probe::invalidate_runtime_probe_cache;
#[cfg(test)]
use probe::{installed_rlm_dir, lock_probe_memo, runtime_probe_key};
// The memo-clear helper and the live-probe package-dir walk exist only behind
// the unix tests (see their gates in probe.rs and tests.rs).
#[cfg(all(test, unix))]
use probe::{clear_in_process_probe_memo_for_tests, installed_package_dir};
pub(crate) use probe::{
    has_prime_agent_runtime, missing_python_requirements, missing_python_skill_import_labels,
};
use probe::{has_prime_agent_runtime_memoized, installed_runtime_identity};
use runtime_source::collect_python_files;
pub(super) use runtime_source::{hash_runtime_source, require_runtime_source_dir};
pub(super) use runtime_source::{package_dir, packaged_runtime_dir};
#[cfg(test)]
use skills::{
    file_content_hash, read_python_skill_dependency_names, read_python_skill_project_name,
};
pub(crate) use skills::{normalize_python_skills, BootstrapPythonSkill};
pub(crate) use uv::ensure_uv;
#[cfg(test)]
use uv::windows_executable_candidates;
use version::{
    bootstrap_base_version_current, bootstrap_skill_key, bootstrap_version_current,
    read_bootstrap_version, read_bootstrap_version_raw, record_python_requirements,
    write_bootstrap_version, STATE_SNAPSHOT_REQUIREMENT,
};
#[cfg(test)]
use version::{recorded_skills_cover, BOOTSTRAP_SCHEMA};

const PYTHON_VERSION: &str = "3.11";
pub(crate) const BOOTSTRAP_LOCK_NAME: &str = ".bootstrap.lock";
pub(crate) const BOOTSTRAP_LOCK_RETRY_MS: u64 = 100;
pub(crate) const BOOTSTRAP_LOCK_STALE_WITHOUT_PID_MS: u64 = 30_000;
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct BootstrapVersion {
    schema: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_uv_args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    python_skills: Option<Vec<BootstrapPythonSkill>>,
}

async fn run_async(command: &str, args: &[String]) -> anyhow::Result<()> {
    // Run on a blocking thread: the bootstrap is an IO-bound child process.
    let command = command.to_string();
    let args = args.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut child = std::process::Command::new(&command);
        child.args(&args).stdin(Stdio::null());
        // Hidden window on Windows (TS `spawnHidden`).
        crate::platform::process::set_no_window(&mut child);
        // CPython must read UTF-8 .pth files even under a Windows legacy
        // code page (TS bootstrap.ts:429).
        if cfg!(windows) {
            child.env("PYTHONUTF8", "1");
        }
        let status = child
            .status()
            .with_context(|| format!("failed to spawn {command}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "{} {} failed with exit code {}",
                command,
                args.join(" "),
                status.code().unwrap_or(-1)
            ))
        }
    })
    .await
    .map_err(|e| anyhow!("bootstrap task join failed: {e}"))?
}

pub(crate) async fn bootstrap_venv(
    venv: &Path,
    source_dir: &Path,
    runtime_identity: &str,
    python_skills: &[BootstrapPythonSkill],
    requirements: &[String],
    options: &EnsureKernelPythonOptions,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(venv.parent().unwrap_or(Path::new("/")))?;
    let uv = ensure_uv()?;
    let python = kernel_venv_python(venv);

    let venv_str = venv.to_string_lossy().to_string();
    let python_str = python.to_string_lossy().to_string();
    let mut install_args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--python".to_string(),
        python_str,
        source_dir.to_string_lossy().to_string(),
    ];
    install_args.push(STATE_SNAPSHOT_REQUIREMENT.to_string());

    run_async(
        &uv,
        &[
            "python".to_string(),
            "install".to_string(),
            PYTHON_VERSION.to_string(),
        ],
    )
    .await?;
    run_async(
        &uv,
        &[
            "venv".to_string(),
            venv_str,
            "--python".to_string(),
            PYTHON_VERSION.to_string(),
            "--seed".to_string(),
        ],
    )
    .await?;
    // One resolve installs the runtime with the startup scripts' packages. A
    // package that fails to resolve must not cost the kernel its runtime: fall
    // back to the runtime alone and leave the packages unrecorded.
    let mut with_requirements = install_args.clone();
    with_requirements.extend(requirements.iter().cloned());
    let installed_requirements = match run_async(&uv, &with_requirements).await {
        Ok(()) => requirements,
        Err(error) if !requirements.is_empty() => {
            run_async(&uv, &install_args).await?;
            options.report(&format!(
                "Warning: Python packages from system.py/user.py failed to install: {error}"
            ));
            &[]
        }
        Err(error) => return Err(error),
    };
    sync_python_skills(
        &uv,
        venv,
        &python,
        source_dir,
        runtime_identity,
        python_skills,
        options,
    )
    .await?;
    record_python_requirements(venv, installed_requirements)
}

/// True when the version file records every one of `requirements` as installed.
pub(crate) fn requirements_recorded(venv: &Path, requirements: &[String]) -> bool {
    let recorded = read_bootstrap_version(venv)
        .and_then(|version| version.extra_uv_args)
        .unwrap_or_default();
    requirements
        .iter()
        .all(|requirement| recorded.contains(requirement))
}

/// Install the startup scripts' packages the version file does not record yet, in one resolve.
/// A failure warns and leaves them unrecorded, so the next start retries.
pub(crate) async fn sync_python_requirements(
    uv: &str,
    venv: &Path,
    python: &Path,
    source_dir: &Path,
    requirements: &[String],
    options: &EnsureKernelPythonOptions,
) -> anyhow::Result<()> {
    let recorded = read_bootstrap_version(venv)
        .and_then(|version| version.extra_uv_args)
        .unwrap_or_default();
    let missing: Vec<String> = requirements
        .iter()
        .filter(|requirement| !recorded.contains(requirement))
        .cloned()
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut install_args = vec![
        "pip".to_string(),
        "install".to_string(),
        "--python".to_string(),
        python.to_string_lossy().to_string(),
        // Keep the resolver pinned to the local runtime source.
        source_dir.to_string_lossy().to_string(),
    ];
    install_args.extend(missing.iter().cloned());
    match run_async(uv, &install_args).await {
        Ok(()) => record_python_requirements(venv, &missing),
        Err(error) => {
            options.report(&format!(
                "Warning: Python packages from system.py/user.py failed to install ({}): {error}",
                missing.join(", ")
            ));
            Ok(())
        }
    }
}

/// Install/refresh the editable Python skills recorded in the version file. Only skills missing or
/// changed are installed. Per-skill failures warn and continue.
pub(crate) async fn sync_python_skills(
    uv: &str,
    venv: &Path,
    python: &Path,
    source_dir: &Path,
    runtime_identity: &str,
    python_skills: &[BootstrapPythonSkill],
    options: &EnsureKernelPythonOptions,
) -> anyhow::Result<()> {
    let version = read_bootstrap_version(venv);
    // Previously installed skills still present on disk: their records carry over so
    // sessions with different skill sets share one venv cache.
    let current_python_skills: HashMap<String, BootstrapPythonSkill> = version
        .as_ref()
        .and_then(|v| v.python_skills.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|recorded| Path::new(&recorded.package_path).is_dir())
        .map(|s| (bootstrap_skill_key(&s), s))
        .collect();
    let python_str = python.to_string_lossy().to_string();
    let mut installed: HashMap<String, BootstrapPythonSkill> = current_python_skills;
    let mut missing: Vec<&BootstrapPythonSkill> = Vec::new();
    for skill in python_skills {
        let key = bootstrap_skill_key(skill);
        if installed.get(&key).is_some_and(|existing| {
            existing.pyproject_path == skill.pyproject_path
                && existing.pyproject_hash == skill.pyproject_hash
        }) {
            continue;
        }
        missing.push(skill);
    }
    if !missing.is_empty() {
        // One uv invocation installs the whole batch of missing skills: a fresh bootstrap otherwise
        // pays one process plus build-backend startup per install. A batch failure falls back to
        // the per-skill loop.
        let mut install_args = vec![
            "pip".to_string(),
            "install".to_string(),
            "--python".to_string(),
            python_str.clone(),
            // Skills may depend on the runtime too. Keep their resolver
            // pinned to the same local source, including per-skill retries.
            source_dir.to_string_lossy().to_string(),
        ];
        for skill in &missing {
            install_args.push("--editable".to_string());
            install_args.push(skill.package_path.clone());
        }
        if run_async(uv, &install_args).await.is_ok() {
            // A changed pyproject (hash moved) replaces the stale record.
            for skill in &missing {
                installed.insert(bootstrap_skill_key(skill), (*skill).clone());
            }
        } else {
            for skill in &missing {
                let result = run_async(
                    uv,
                    &[
                        "pip".to_string(),
                        "install".to_string(),
                        "--python".to_string(),
                        python_str.clone(),
                        source_dir.to_string_lossy().to_string(),
                        "--editable".to_string(),
                        skill.package_path.clone(),
                    ],
                )
                .await;
                match result {
                    Ok(()) => {
                        installed.insert(bootstrap_skill_key(skill), (*skill).clone());
                    }
                    Err(error) => options.report(&format!(
                        "Warning: Python skill {} failed to install and will be unavailable: {error}",
                        skill.import_name
                    )),
                }
            }
        }
    }
    let mut merged: Vec<BootstrapPythonSkill> = installed.into_values().collect();
    merged.sort_by(|a, b| {
        a.package_path
            .cmp(&b.package_path)
            .then(a.import_name.cmp(&b.import_name))
    });
    write_bootstrap_version(venv, runtime_identity, &merged)
}

pub(crate) fn kernel_base_ready(python: &str, venv: &Path, runtime_identity: &str) -> bool {
    let (version, raw) = read_bootstrap_version_raw(venv);
    bootstrap_base_version_current(version, runtime_identity)
        && has_prime_agent_runtime_memoized(
            python,
            runtime_identity,
            &raw,
            &installed_runtime_identity(Path::new(python), venv),
            venv,
        )
}

pub(crate) fn kernel_ready(
    python: &str,
    venv: &Path,
    runtime_identity: &str,
    python_skills: &[BootstrapPythonSkill],
) -> bool {
    let (version, raw) = read_bootstrap_version_raw(venv);
    bootstrap_version_current(version.as_ref(), runtime_identity, python_skills)
        && has_prime_agent_runtime_memoized(
            python,
            runtime_identity,
            &raw,
            &installed_runtime_identity(Path::new(python), venv),
            venv,
        )
}

#[cfg(test)]
mod tests;
