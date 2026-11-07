# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "requests",
#   "httpx",
#   "pyyaml",
#   "tomli",
#   "python-dotenv",
#   "pandas",
#   "numpy",
#   "scipy",
#   "beautifulsoup4",
#   "lxml",
#   "pydantic",
#   "tyro",
# ]
# ///
#
# system.py: the Python REPL's starting state. It runs in the REPL namespace at
# every kernel start, after a saved namespace is restored. Everything it binds is
# ready to use in your first cell. The packages above are installed in the kernel.

import asyncio
import os as _prime_agent_os

_prime_agent_os.environ["NO_COLOR"] = "1"

from rlm import bash, rlm
import rlm.mcp as mcp

# bash(command: str, persistent: bool = False) -> BashHandle
#   Synchronous: returns immediately and runs the command in the background. When a
#   command finishes outside the calling python_repl block, you get a notification.
#   Each bash() call is its own process, so shell state does not persist between calls.
#   persistent=True (POSIX, saved sessions only): the shell survives kernel, worker, and
#   daemon restarts and its session's delete or archive (a deleted session's shells keep
#   running as orphans); it ends only on its own exit or an explicit kill. Use it only when
#   really required (a server or long job that must outlive a restart), never for ordinary
#   commands. After a restart, get the handles back with
#   `from rlm import persistent_shells; persistent_shells()` (a shell that finished while no
#   kernel ran is listed once, finished). Completion of a re-attached shell is polled, so its
#   result and notification lag by about a second.
#
# class BashHandle:
#     pid: int
#     running: bool
#     command: str
#     def tail(self, n: int = 50) -> str: ...
#     def output(self) -> str: ...
#     def poll(self) -> BashResult | None: ...
#     def kill(self, sig: int = SIGTERM, grace: float = 5.0) -> None: ...
#         # SIGTERM, escalating to SIGKILL; on Windows kill() uses taskkill /T, and detached
#         # or reparented descendants may survive
#     # `await handle` waits according to this session's bash completion contract; the
#     # handle remains awaitable after the command completes
#
# class BashResult:
#     exit_code: int
#     output: str
#     duration: float

# rlm: subagents. Each agent can reach its parent, its siblings, and its children.
#
# await rlm.spawn(prompt: str, *, name: str, model: str | None = None,
#                 thinking: str | None = None, cwd: str | None = None) -> RLMSpawnHandle
#   Spawns a subagent one level deeper than the caller and returns immediately. Errors when
#   the caller cannot create subagents. `model` and `thinking` default to the parent's. `cwd`
#   starts the child in that directory (relative paths resolve against the session cwd; it
#   must exist) and loads its project context. Results never arrive as the return value; they
#   arrive only via agent_message. `name` must be unique among the new agent's siblings, and
#   be meaningful and descriptive but short.
# await rlm.create_session(prompt: str, name: str | None = None, model: str | None = None,
#                          thinking: str | None = None, cwd: str | None = None) -> RLMCreateSessionHandle
#   Creates another depth-0 session. Only for depth-0 agents backed by a daemon. Returns after
#   the session is created and the first prompt sent.
# await rlm.find_models(query: str = "", limit: int = 8) -> list[RLMModel]
# await rlm.list_subagents() -> list[RLMSubagent]                 # direct children
# await rlm.delete_subagent(target: str | RLMSubagent) -> RLMSubagent
# await rlm.rename(new_name: str, *, session_id=None) -> str
#   Renames the calling session (omit session_id) or a direct child (spawn handle,
#   list_subagents() row, or session id; never the child's name). Names must be unique among
#   siblings, and the renamed session sees a transcript line for the change.
# await rlm.collect(targets=None, *, timeout_ms: int = 0) -> list[RLMChildResult]
#   Typed snapshots of direct children (status, settled flag, answer preview, error) without
#   steering anyone. timeout_ms=0 returns a non-blocking snapshot; a positive timeout blocks
#   only this call until the children settle or the deadline passes.
# await rlm.progress_note(message: str) -> dict
#   Reports brief in-flight progress to the parent (at most 512 characters, throttled to about
#   one note per 10 seconds). The parent sees notes without needing to reply.
#
# class RLMSpawnHandle:
#     rlm_child_id: str
#     name: str
#     session_dir: Path
#     model: str                    # provider/model, full resolved model name
#
# class RLMCreateSessionHandle:
#     active_session_id: str        # live daemon-instance id
#     session_id: str               # durable on-disk session identity
#     name: str
#     session_file: Path            # persisted session log
#     model: str                    # provider/model, full resolved model name
#
# class RLMSubagent:
#     rlm_child_id: str
#     active_session_id: str | None
#     session_id: str | None
#     session_name: str
#     session_dir: Path
#     status: Literal["running", "completed", "error"]
#     # rows also carry live activity detail: current activity, tool use count, duration,
#     # answer preview, latest progress note
#
# class RLMModel:
#     provider: str
#     id: str
#     name: str
#     selector: str                 # the exact string to pass as rlm.spawn(model=...)

# rlm.harness: persistent memory. These calls and rlm.get_harness_state are sync: call
# them without await. global_=True selects cross-session entries (Python reserves
# `global`, so the parameter is spelled global_).
#
# rlm.harness.create_memory(title: str, content: str, *, id: str | None = None, path: str = "general",
#                           metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.update_memory(id: str, title: str, content: str, *, path: str | None = None,
#                           metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.delete_memory(id: str, *, global_: bool = False) -> bool
# rlm.harness.create_prompt_note(title: str, content: str, *, id: str | None = None, path: str = "policy",
#                                metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.update_prompt_note(id: str, title: str, content: str, *, path: str | None = None,
#                                metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.delete_prompt_note(id: str, *, global_: bool = False) -> bool
# rlm.harness.create_skill(title: str, content: str, *, id: str | None = None, path: str = "general",
#                          reference: dict | None = None, arguments: dict | None = None,
#                          metadata: dict | None = None, global_: bool = False) -> HarnessEntry
#   reference/arguments describe the Python callable: reference requires {"type": "python"},
#   a Python import, and a callable or call pattern.
# rlm.harness.update_skill(id: str, title: str, content: str, *, path: str | None = None,
#                          reference: dict | None = None, arguments: dict | None = None,
#                          metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.delete_skill(id: str, *, global_: bool = False) -> bool
# rlm.harness.create_subagent(title: str, content: str, *, id: str | None = None, path: str = "general",
#                             metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.update_subagent(id: str, title: str, content: str, *, path: str | None = None,
#                             metadata: dict | None = None, global_: bool = False) -> HarnessEntry
# rlm.harness.delete_subagent(id: str, *, global_: bool = False) -> bool
# rlm.harness.record_refinement(trigger: str, changes: list[str], *, evidence: str = "", outcome: str = "",
#                               id: str | None = None, global_: bool = False) -> RefinementEvent
# rlm.harness.plan_refinement(observation: str, *, failing_component: str = "",
#                             next_step: str = "") -> list[str]   # a diagnose -> update -> validate plan
# rlm.harness.overview(*, max_entries_per_kind: int = 20, global_: bool = False) -> str
# rlm.harness.search(query: str, kind: str | None = None, limit: int = 10, *,
#                    global_: bool = False) -> list[HarnessEntry]    # ranked term search
# rlm.get_harness_state(state_dir: str | Path | None = None, *, global_: bool = False) -> HarnessState
#   Full memory details for the selected scope. Pass another agent's state dir to read its state.
#
# class HarnessEntry:
#     id: str
#     kind: Literal["prompt", "memory", "skill", "subagent"]
#     title: str
#     content: str
#     path: str                     # category: "general" for memories, "policy" for prompt notes
#     scope: Literal["local", "global"]
#     reference: dict[str, Any]     # skills only
#     arguments: dict[str, Any]     # skills only
#     metadata: dict
#     source: Literal["agent", "refine"]
#     created_at: str               # ISO timestamp
#     updated_at: str               # ISO timestamp
#     version: int                  # starts at 1, increments on every update
#
# class HarnessState:
#     scope: Literal["global", "local"]
#     file_path: Path
#     entries: dict[kind, dict[id, HarnessEntry]]
#     refinements: list[RefinementEvent]
#
# class RefinementEvent:
#     id: str
#     trigger: str                  # what caused the refinement
#     changes: list[str]            # applied edits; empty if none
#     evidence: str
#     outcome: str
#     created_at: str

# mcp: tools defined in the MCP format, with schemas discovered at runtime.
#
# await mcp.list_tools(server: str) -> list[dict]                 # tool schemas
# await mcp.call_tool(server: str, tool: str, arguments: dict | None = None) -> Any
# await mcp.list_plugins(connection_status: str | None = None, limit: int = 50,
#                        cursor: str | None = None) -> dict       # one page of the service catalog
# await mcp.search_plugins(query: str, limit: int = 10) -> dict   # bounded catalog search
# await mcp.list_connections() -> list[dict]                      # the user's connections (dispatch ids)
# await mcp.search_tools(query: str, connection_id: str | None = None, limit: int = 20) -> dict
#   Searches live tool names/descriptions and reports the search scope.
# await mcp.describe_tool(connection_id: str, tool: str) -> dict  # name, description, inputSchema
