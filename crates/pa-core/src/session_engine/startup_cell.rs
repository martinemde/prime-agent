//! The kernel's startup as the conversation's first cell. Every provider request starts with a
//! `python_repl` call whose code is the startup scripts the kernel runs, so the model meets its
//! namespace as code that already ran rather than as prompt prose. The cell is rebuilt from the
//! scripts, never written to the session log, and identical across requests (cache-stable, and
//! untouched by compaction).

use std::sync::Arc;

use pa_agent::agent_loop::ConvertToLlmFn;
use pa_agent::types::Message;

use crate::kernel::bootstrap::{KernelPythonSkill, KernelStartupScripts};

const STARTUP_CELL_TOOL_CALL_ID: &str = "call_kernel_startup";

/// The cell's code: the system setup, the Python skill imports, then `user.py`, in run order.
#[must_use]
pub fn startup_cell_code(
    scripts: &KernelStartupScripts,
    python_skills: &[KernelPythonSkill],
) -> String {
    let (system_source, system_name) = scripts.system_source();
    let mut sections = vec![format!("# {system_name}\n{}", system_source.trim_end())];
    let mut imports: Vec<&str> = Vec::new();
    for skill in python_skills {
        if !imports.contains(&skill.import_name.as_str()) {
            imports.push(&skill.import_name);
        }
    }
    if !imports.is_empty() {
        sections.push(format!("# Python skills imported: {}", imports.join(", ")));
    }
    if let Some(user) = scripts
        .user
        .as_ref()
        .filter(|user| !user.source.trim().is_empty())
    {
        sections.push(format!(
            "# {}\n{}",
            user.path.display(),
            user.source.trim_end()
        ));
    }
    sections.join("\n\n")
}

/// The harness notice, the `python_repl` call running `code`, and its (empty) result.
#[must_use]
pub fn startup_cell_messages(code: &str) -> Vec<Message> {
    let wire = serde_json::json!([
        {
            "role": "user",
            "content": "[kernel-start]\nThe Python kernel started and ran its startup scripts.",
            "timestamp": 0
        },
        {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": STARTUP_CELL_TOOL_CALL_ID,
                "name": pa_types::ai::PYTHON_TOOL_NAME,
                "arguments": { "code": code }
            }],
            "api": "prime-agent",
            "provider": "prime-agent",
            "model": "kernel-startup",
            "usage": {},
            "stopReason": "toolUse",
            "timestamp": 0
        },
        {
            "role": "toolResult",
            "toolCallId": STARTUP_CELL_TOOL_CALL_ID,
            "toolName": pa_types::ai::PYTHON_TOOL_NAME,
            "content": [{ "type": "text", "text": "" }],
            "isError": false,
            "timestamp": 0
        }
    ]);
    serde_json::from_value(wire).expect("the startup cell matches the message wire shape")
}

/// `inner`, with the startup cell in front of every request's messages.
#[must_use]
pub fn with_startup_cell(inner: ConvertToLlmFn, cell: Vec<Message>) -> ConvertToLlmFn {
    let cell = Arc::new(cell);
    Arc::new(move |messages| {
        let inner = Arc::clone(&inner);
        let cell = Arc::clone(&cell);
        Box::pin(async move {
            let mut out = cell.as_ref().clone();
            out.extend(inner(messages).await?);
            Ok(out)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::StartupScript;
    use std::path::PathBuf;

    fn skill(import_name: &str) -> KernelPythonSkill {
        KernelPythonSkill {
            name: import_name.to_string(),
            import_name: import_name.to_string(),
            package_path: PathBuf::from("/skills"),
            pyproject_path: PathBuf::from("/skills/pyproject.toml"),
        }
    }

    #[test]
    fn cell_code_is_the_scripts_in_run_order() {
        let scripts = KernelStartupScripts {
            system: None,
            user: Some(StartupScript {
                path: PathBuf::from("/agent/user.py"),
                source: "USER_PY_LOADED = True\n".to_string(),
            }),
        };
        let code = startup_cell_code(&scripts, &[skill("edit"), skill("goal"), skill("edit")]);
        let system = format!(
            "# system.py\n{}",
            crate::kernel::bootstrap::SHIPPED_SYSTEM_SCRIPT.trim_end()
        );
        assert_eq!(
            code,
            format!(
                "{system}\n\n# Python skills imported: edit, goal\n\n# /agent/user.py\nUSER_PY_LOADED = True"
            )
        );
    }

    #[tokio::test]
    async fn every_request_starts_with_the_cell() {
        let inner: ConvertToLlmFn = Arc::new(|_messages| {
            Box::pin(async {
                Ok(serde_json::from_value(serde_json::json!([
                    { "role": "user", "content": "hi", "timestamp": 1 }
                ]))?)
            })
        });
        let convert = with_startup_cell(inner, startup_cell_messages("x = 1"));
        let out = convert(Vec::new()).await.unwrap();
        assert_eq!(out.len(), 4);
        let Message::Assistant(call) = &out[1] else {
            panic!("the cell's second message is the python_repl call");
        };
        assert_eq!(call.tool_calls()[0].arguments["code"], "x = 1");
        assert!(
            matches!(&out[2], Message::ToolResult(result) if result.tool_call_id == STARTUP_CELL_TOOL_CALL_ID)
        );
        assert!(matches!(&out[3], Message::User(_)));
    }
}
