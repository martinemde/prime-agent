//! The runtime bootstrap code injected into each kernel right after start or
//! restore: runs the system setup (which binds the `rlm`, `bash`, and MCP
//! surfaces), pre-imports every Python skill, then runs the user setup.

use super::startup_scripts::{KernelStartupScripts, StartupScript, SHIPPED_SYSTEM_SCRIPT};
use super::KernelPythonSkill;

/// Line the runtime bootstrap prints when one or more pre-imported Python skills failed to import.
/// The host scans the bootstrap cell's stdout for this marker.
pub const PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER: &str =
    "__PRIME_AGENT_PYTHON_SKILL_IMPORT_ERRORS__";

/// Failed skill imports reported by a bootstrap cell: `(import name, import error)` pairs in import
/// order (TS `UnavailablePythonSkills = Record<string, string>`).
pub type UnavailablePythonSkills = Vec<(String, String)>;

/// Extract the unavailable-skill report a bootstrap cell printed, or `None` when it printed none:
/// the marker must be followed by a JSON object with at least one non-empty string value.
#[must_use]
pub fn parse_unavailable_python_skills(stdout: &str) -> Option<UnavailablePythonSkills> {
    let at = stdout.find(PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER)?;
    let raw = stdout[at + PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER.len()..].trim();
    let parsed = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw).ok()?;
    let errors = parsed
        .into_iter()
        .filter_map(|(name, error)| {
            let error = error.as_str()?;
            (!error.is_empty()).then(|| (name, error.to_string()))
        })
        .collect::<UnavailablePythonSkills>();
    (!errors.is_empty()).then_some(errors)
}

/// Host wiring the runtime needs whatever the system setup does, emitted only
/// when the host registered the [`crate::kernel::shared::BASH_COMPLETED_REQUEST_TYPE`]
/// handler: turns the runtime's bounded `await bash()` on. Without the notice
/// nothing wakes a cell that ended its turn on a degraded command, so
/// daemonless sessions keep the unbounded await.
const RLM_BOOTSTRAP_BASH_NOTICES_CODE: &str = "try:
    import rlm as _prime_agent_rlm_module
    _prime_agent_rlm_module._set_completion_notices_wired(True)
except Exception:
    pass";

/// Runs one startup script from the agent dir in the REPL namespace, the way
/// the interpreter runs `PYTHONSTARTUP`: top-level `await` works as in a cell,
/// and an exception prints its traceback to stderr instead of failing the
/// kernel (like a broken `sitecustomize`).
const RUN_STARTUP_SCRIPT_CODE: &str = r#"async def _prime_agent_run_startup_script(_prime_agent_path, _prime_agent_source):
    import ast as _prime_agent_ast
    import inspect as _prime_agent_inspect
    import sys as _prime_agent_sys
    import traceback as _prime_agent_traceback
    try:
        _prime_agent_result = eval(
            compile(
                _prime_agent_source,
                _prime_agent_path,
                "exec",
                flags=_prime_agent_ast.PyCF_ALLOW_TOP_LEVEL_AWAIT,
            ),
            globals(),
        )
        if _prime_agent_inspect.iscoroutine(_prime_agent_result):
            await _prime_agent_result
    except Exception:
        print(f"Error in {_prime_agent_path}:", file=_prime_agent_sys.stderr)
        _prime_agent_traceback.print_exc()"#;

fn run_startup_script_line(script: &StartupScript) -> String {
    let path = script.path.display().to_string();
    // JSON string literals are valid Python string literals.
    let path_literal = serde_json::to_string(&path).unwrap_or_else(|_| "\"\"".to_string());
    let source_literal =
        serde_json::to_string(&script.source).unwrap_or_else(|_| "\"\"".to_string());
    format!("await _prime_agent_run_startup_script({path_literal}, {source_literal})")
}

/// The code the session injects right after kernel start/restore, with the shipped system setup
/// and no user script. See [`build_kernel_bootstrap_code`].
#[must_use]
pub fn build_rlm_bootstrap_code(
    python_skills: &[KernelPythonSkill],
    bash_notices_wired: bool,
) -> String {
    build_kernel_bootstrap_code(
        &KernelStartupScripts::default(),
        python_skills,
        bash_notices_wired,
    )
}

/// The code the session injects right after kernel start/restore, in order: the host's bash
/// notice wiring (only when `bash_notices_wired`), the system setup (the agent dir's `system.py`,
/// else the shipped one, which binds the `rlm`, `bash`, and MCP surfaces), every Python skill
/// import, then the agent dir's `user.py`.
#[must_use]
pub fn build_kernel_bootstrap_code(
    startup: &KernelStartupScripts,
    python_skills: &[KernelPythonSkill],
    bash_notices_wired: bool,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    if bash_notices_wired {
        sections.push(RLM_BOOTSTRAP_BASH_NOTICES_CODE.to_string());
    }
    if startup.system.is_some() || startup.user.is_some() {
        sections.push(RUN_STARTUP_SCRIPT_CODE.to_string());
    }
    match &startup.system {
        Some(script) => sections.push(run_startup_script_line(script)),
        None => sections.push(SHIPPED_SYSTEM_SCRIPT.trim().to_string()),
    }
    // Python skills pre-import between the system and user setup.
    // TS: `[...new Set(pythonSkills.map(...))]` — first-seen order, so the pre-import loop follows
    // the session's skill discovery order.
    let mut import_names: Vec<&str> = Vec::new();
    for skill in python_skills {
        let import_name = skill.import_name.as_str();
        if !import_names.contains(&import_name) {
            import_names.push(import_name);
        }
    }
    if !import_names.is_empty() {
        let imports_json =
            serde_json::to_string(&import_names).unwrap_or_else(|_| "[]".to_string());
        let code = format!(
            r#"
import importlib as _prime_agent_importlib
import inspect as _prime_agent_inspect
import sys as _prime_agent_sys
import types as _prime_agent_types

class _PrimeAgentCallableSkillModule(_prime_agent_types.ModuleType):
    async def __call__(self, *args, **kwargs):
        result = self.run(*args, **kwargs)
        if _prime_agent_inspect.isawaitable(result):
            return await result
        return result

class _PrimeAgentUnavailableSkill:
    def __init__(self, name, error):
        self.__name__ = name
        self._prime_agent_import_error = error
        self.__doc__ = f"Python skill {{name}} is unavailable: {{error}}"

    async def run(self, *args, **kwargs):
        raise RuntimeError(
            f"Python skill {{self.__name__}} is unavailable in this kernel. "
            f"Import error: {{self._prime_agent_import_error}}"
        )

    async def __call__(self, *args, **kwargs):
        return await self.run()

    def __repr__(self):
        return f"<unavailable Python skill {{self.__name__!r}}: {{self._prime_agent_import_error}}>"

def _prime_agent_wrap_skill_module(module):
    run = getattr(module, "run", None)
    if not callable(run):
        return module
    if isinstance(module, _PrimeAgentCallableSkillModule):
        return module
    wrapped = _PrimeAgentCallableSkillModule(module.__name__)
    wrapped.__dict__.update(module.__dict__)
    try:
        wrapped.__signature__ = _prime_agent_inspect.signature(run)
    except Exception:
        pass
    doc = getattr(run, "__doc__", None)
    if doc:
        wrapped.__doc__ = doc
    _prime_agent_sys.modules[module.__name__] = wrapped
    return wrapped

_PRIME_AGENT_SKILL_IMPORT_ERRORS = {{}}

for _prime_agent_skill_name in {imports_json}:
    try:
        globals()[_prime_agent_skill_name] = _prime_agent_wrap_skill_module(
            _prime_agent_importlib.import_module(_prime_agent_skill_name)
        )
    except Exception as _prime_agent_skill_error:
        # An empty exception message would otherwise be dropped by the
        # host-side parser; fall back to the exception type name.
        _prime_agent_skill_error_text = (
            str(_prime_agent_skill_error) or type(_prime_agent_skill_error).__name__
        )
        _PRIME_AGENT_SKILL_IMPORT_ERRORS[_prime_agent_skill_name] = _prime_agent_skill_error_text
        globals()[_prime_agent_skill_name] = _PrimeAgentUnavailableSkill(
            _prime_agent_skill_name,
            _prime_agent_skill_error_text,
        )

if _PRIME_AGENT_SKILL_IMPORT_ERRORS:
    import json as _prime_agent_json
    print(
        "{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}"
        + _prime_agent_json.dumps(_PRIME_AGENT_SKILL_IMPORT_ERRORS)
    )
"#
        );
        sections.push(code.trim().to_string());
    }
    if let Some(script) = &startup.user {
        sections.push(run_startup_script_line(script));
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bootstrap_code_without_skills_binds_rlm() {
        let code = build_rlm_bootstrap_code(&[], false);
        assert!(code.contains("from rlm import bash, rlm"));
        assert!(!code.contains("_PrimeAgentUnavailableSkill"));
    }

    #[test]
    fn bootstrap_turns_the_bounded_bash_default_on_only_with_notices() {
        let daemonless = build_rlm_bootstrap_code(&[], false);
        let noticed = build_rlm_bootstrap_code(&[], true);
        assert!(
            noticed.contains("_set_completion_notices_wired(True)"),
            "the wired bootstrap must enable the bounded bash default: {noticed}"
        );
        assert!(
            !daemonless.contains("_set_completion_notices_wired"),
            "the daemonless bootstrap must not emit the toggle: {daemonless}"
        );
    }

    #[test]
    fn bootstrap_code_imports_skills() {
        let skills = vec![KernelPythonSkill {
            name: "edit".into(),
            import_name: "edit".into(),
            package_path: PathBuf::from("/pkg/edit"),
            pyproject_path: PathBuf::from("/pkg/edit/pyproject.toml"),
        }];
        let code = build_rlm_bootstrap_code(&skills, false);
        assert!(code.contains(r#"for _prime_agent_skill_name in ["edit"]"#));
        assert!(code.contains("_PrimeAgentUnavailableSkill"));
    }

    #[test]
    fn bootstrap_code_runs_system_then_skills_then_user() {
        let skills = vec![KernelPythonSkill {
            name: "edit".into(),
            import_name: "edit".into(),
            package_path: PathBuf::from("/pkg/edit"),
            pyproject_path: PathBuf::from("/pkg/edit/pyproject.toml"),
        }];
        let script = |name: &str, source: &str| StartupScript {
            path: PathBuf::from(format!("/agent/{name}")),
            source: source.to_string(),
        };
        let startup = KernelStartupScripts {
            system: Some(script("system.py", "x = \"system\"\n")),
            user: Some(script("user.py", "y = x\n")),
        };
        let code = build_kernel_bootstrap_code(&startup, &skills, true);
        let at = |needle: &str| {
            code.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}: {code}"))
        };
        let order = [
            at("_set_completion_notices_wired(True)"),
            at(r#"await _prime_agent_run_startup_script("/agent/system.py", "x = \"system\"\n")"#),
            at(r#"for _prime_agent_skill_name in ["edit"]"#),
            at(r#"await _prime_agent_run_startup_script("/agent/user.py", "y = x\n")"#),
        ];
        assert!(order.is_sorted(), "{code}");
        assert!(
            !code.contains("import rlm.mcp as mcp"),
            "a system.py replaces the shipped setup: {code}"
        );
    }

    /// The TS #2381 parser table: marker+JSON, noise-prefixed marker, no marker, non-JSON payload
    /// all land where the host-side report decides notice vs silence.
    #[test]
    fn parse_unavailable_python_skills_table() {
        let cases: Vec<(String, Option<UnavailablePythonSkills>)> = vec![
            (
                format!(
                    "{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"websearch\":\"No module named 'websearch'\"}}\n"
                ),
                Some(vec![("websearch".into(), "No module named 'websearch'".into())]),
            ),
            (
                format!("noise\n{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"edit\":\"boom\"}}"),
                Some(vec![("edit".into(), "boom".into())]),
            ),
            ("some unrelated kernel output".to_string(), None),
            (format!("{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}not json"), None),
            (format!("{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{}}"), None),
        ];
        for (stdout, expected) in cases {
            assert_eq!(
                parse_unavailable_python_skills(&stdout),
                expected,
                "{stdout}"
            );
        }
    }

    #[test]
    fn parse_unavailable_python_skills_drops_non_string_and_empty_values() {
        // Non-string values and empty messages drop (TS keeps only
        // non-empty string entries); ordering follows the marker's JSON.
        let stdout = format!(
            "{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"edit\":\"boom\",\"null_skill\":null,\"list_skill\":[1],\"empty_skill\":\"\",\"websearch\":\"No module named 'websearch'\"}}"
        );
        assert_eq!(
            parse_unavailable_python_skills(&stdout),
            Some(vec![
                ("edit".into(), "boom".into()),
                ("websearch".into(), "No module named 'websearch'".into()),
            ])
        );
        // A report whose every value drops stays silent.
        let stdout = format!("{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"empty_skill\":\"\"}}");
        assert_eq!(parse_unavailable_python_skills(&stdout), None);
    }
}
