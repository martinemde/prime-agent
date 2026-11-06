//! The kernel's startup scripts, read from the agent (config) directory:
//! `system.py` sets the interpreter's initial state and replaces the shipped
//! default when present; `user.py` runs after it. They are the REPL's
//! `sitecustomize`/`usercustomize` pair, executed in the kernel namespace like
//! `PYTHONSTARTUP`. Each script's PEP 723 inline-metadata `dependencies` are
//! the packages the kernel venv pre-installs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Kernel env key carrying the session's agent dir; the startup scripts are
/// discovered there.
pub(crate) const KERNEL_AGENT_DIR_ENV: &str = "PRIME_AGENT_CODING_AGENT_DIR";

/// File name of the system setup script in the agent dir.
const SYSTEM_SCRIPT_FILE_NAME: &str = "system.py";

/// File name of the user setup script in the agent dir.
const USER_SCRIPT_FILE_NAME: &str = "user.py";

/// The shipped system setup, used when the agent dir has no `system.py`.
pub(crate) const SHIPPED_SYSTEM_SCRIPT: &str = include_str!("system.py");

/// One startup script read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupScript {
    pub path: PathBuf,
    pub source: String,
}

/// The startup scripts one kernel runs. `Default` is the shipped system setup
/// with no user script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KernelStartupScripts {
    /// The agent dir's `system.py`; `None` runs [`SHIPPED_SYSTEM_SCRIPT`].
    pub system: Option<StartupScript>,
    /// The agent dir's `user.py`, run after the system setup.
    pub user: Option<StartupScript>,
}

/// The kernel venv's pre-installed packages, collected from the startup
/// scripts' PEP 723 `dependencies`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupRequirements {
    /// PEP 508 requirement strings, first-seen order, deduplicated.
    pub requirements: Vec<String>,
    /// One message per script whose inline metadata could not be read.
    pub errors: Vec<String>,
}

impl KernelStartupScripts {
    /// Read `system.py` and `user.py` from `agent_dir`. A missing or
    /// unreadable file is absent.
    #[must_use]
    pub fn discover(agent_dir: &Path) -> Self {
        let read = |name: &str| {
            let path = agent_dir.join(name);
            std::fs::read_to_string(&path)
                .ok()
                .map(|source| StartupScript { path, source })
        };
        Self {
            system: read(SYSTEM_SCRIPT_FILE_NAME),
            user: read(USER_SCRIPT_FILE_NAME),
        }
    }

    /// Discover from the agent dir a kernel env names ([`KERNEL_AGENT_DIR_ENV`]);
    /// an env without one runs the shipped defaults only.
    #[must_use]
    pub fn from_kernel_env(env: &HashMap<String, String>) -> Self {
        match env.get(KERNEL_AGENT_DIR_ENV) {
            Some(dir) if !dir.is_empty() => Self::discover(Path::new(dir)),
            _ => Self::default(),
        }
    }

    /// The system setup's source and display name.
    #[must_use]
    pub fn system_source(&self) -> (&str, String) {
        match &self.system {
            Some(script) => (&script.source, script.path.display().to_string()),
            None => (SHIPPED_SYSTEM_SCRIPT, SYSTEM_SCRIPT_FILE_NAME.to_string()),
        }
    }

    /// The packages to pre-install: the system setup's dependencies, then the
    /// user script's.
    #[must_use]
    pub fn requirements(&self) -> StartupRequirements {
        let (system_source, system_name) = self.system_source();
        let mut scripts = vec![(system_source, system_name)];
        if let Some(user) = &self.user {
            scripts.push((&user.source, user.path.display().to_string()));
        }
        let mut collected = StartupRequirements::default();
        for (source, name) in scripts {
            match script_dependencies(source) {
                Ok(dependencies) => {
                    for dependency in dependencies {
                        if !collected.requirements.contains(&dependency) {
                            collected.requirements.push(dependency);
                        }
                    }
                }
                Err(error) => collected
                    .errors
                    .push(format!("{name}: invalid PEP 723 script metadata: {error}")),
            }
        }
        collected
    }
}

/// The `dependencies` of a script's PEP 723 `script` metadata block; a script
/// without a block has none.
fn script_dependencies(source: &str) -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    struct ScriptMetadata {
        #[serde(default)]
        dependencies: Vec<String>,
    }
    let Some(metadata) = script_metadata_block(source)? else {
        return Ok(Vec::new());
    };
    toml::from_str::<ScriptMetadata>(&metadata)
        .map(|parsed| parsed.dependencies)
        .map_err(|error| error.message().to_string())
}

/// The TOML body of the PEP 723 `# /// script` block, with each line's `#` or
/// `# ` prefix removed. The block ends at the last `# ///` line before the
/// first line that is not a comment, per the PEP's reference regex.
fn script_metadata_block(source: &str) -> Result<Option<String>, String> {
    let mut lines = source.lines();
    if !lines.any(|line| line.trim_end() == "# /// script") {
        return Ok(None);
    }
    let mut body: Vec<&str> = Vec::new();
    let mut end = None;
    for line in lines {
        let line = line.trim_end_matches('\r');
        let content = if line == "#" {
            ""
        } else if let Some(rest) = line.strip_prefix("# ") {
            rest
        } else {
            break;
        };
        if content == "///" {
            end = Some(body.len());
        }
        body.push(content);
    }
    let Some(end) = end else {
        return Err("unclosed `# /// script` block".to_string());
    };
    Ok(Some(body[..end].join("\n")))
}

/// The distribution name a PEP 508 requirement string names
/// (`"pandas>=2; python_version > '3'"` names `pandas`).
#[must_use]
pub(crate) fn requirement_name(requirement: &str) -> &str {
    let requirement = requirement.trim_start();
    let end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(requirement.len());
    &requirement[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_system_script_lists_the_default_packages() {
        assert_eq!(
            KernelStartupScripts::default().requirements(),
            StartupRequirements {
                requirements: [
                    "requests",
                    "httpx",
                    "pyyaml",
                    "tomli",
                    "python-dotenv",
                    "pandas",
                    "numpy",
                    "scipy",
                    "beautifulsoup4",
                    "lxml",
                    "pydantic",
                    "tyro",
                ]
                .map(String::from)
                .to_vec(),
                errors: Vec::new(),
            }
        );
    }

    #[test]
    fn agent_dir_system_replaces_the_shipped_setup_and_user_adds_to_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("system.py"),
            "# /// script\n# dependencies = [\"rich\", \"httpx\"]\n# ///\nimport rich\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("user.py"),
            "# /// script\n# dependencies = [\n#   \"httpx>=0.27\",\n#   \"rich\",\n# ]\n# ///\n",
        )
        .unwrap();
        let mut env = HashMap::new();
        env.insert(
            KERNEL_AGENT_DIR_ENV.to_string(),
            dir.path().to_string_lossy().to_string(),
        );
        let scripts = KernelStartupScripts::from_kernel_env(&env);
        assert_eq!(
            scripts.system_source().0,
            std::fs::read_to_string(dir.path().join("system.py")).unwrap()
        );
        assert_eq!(
            scripts.requirements(),
            StartupRequirements {
                requirements: ["rich", "httpx", "httpx>=0.27"].map(String::from).to_vec(),
                errors: Vec::new(),
            }
        );
    }

    #[test]
    fn script_metadata_errors_name_the_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("user.py"),
            "# /// script\n# dependencies = [\n",
        )
        .unwrap();
        let requirements = KernelStartupScripts::discover(dir.path()).requirements();
        assert_eq!(
            requirements.requirements.len(),
            12,
            "the shipped list still applies"
        );
        assert_eq!(
            requirements.errors,
            vec![format!(
                "{}: invalid PEP 723 script metadata: unclosed `# /// script` block",
                dir.path().join("user.py").display()
            )]
        );
    }

    #[test]
    fn requirement_names_strip_specifiers_extras_and_markers() {
        for (requirement, name) in [
            ("pandas", "pandas"),
            ("pandas>=2", "pandas"),
            ("httpx[http2] ; python_version > '3'", "httpx"),
            ("python-dotenv==1.0", "python-dotenv"),
            ("zope.interface~=6", "zope.interface"),
        ] {
            assert_eq!(requirement_name(requirement), name);
        }
    }
}
