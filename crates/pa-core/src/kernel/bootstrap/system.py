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
# Prime Agent's system setup for the Python kernel: the interpreter's initial
# state. It runs in the REPL namespace at the start of every kernel, after a
# saved namespace is restored, the way Python runs sitecustomize.
#
# - Put a system.py in the Prime Agent config directory (~/.prime/agent/) to
#   replace this file.
# - Put a user.py there to extend it; user.py runs after this setup and after
#   the Python skills are imported, the way Python runs usercustomize.
#
# The PEP 723 `dependencies` above (and those in user.py) are installed into
# the kernel venv and listed to the model as pre-installed packages.

import asyncio
import os as _prime_agent_os

_prime_agent_os.environ["NO_COLOR"] = "1"

try:
    import rlm as _prime_agent_rlm_module
    rlm = _prime_agent_rlm_module.rlm
    bash = _prime_agent_rlm_module.bash
    import rlm.mcp as mcp
except Exception as _prime_agent_rlm_error:
    _PRIME_AGENT_RLM_IMPORT_ERROR = str(_prime_agent_rlm_error)

    class _PrimeAgentMissingRlm:
        def _raise_missing(self):
            raise RuntimeError(
                "prime-agent-runtime is not installed in this kernel. "
                "Remove ~/.prime/agent/kernel-venv so prime-agent can rebuild it, or set "
                "PRIME_AGENT_KERNEL_PYTHON to a kernel environment with prime-agent-runtime installed. "
                f"Import error: {_PRIME_AGENT_RLM_IMPORT_ERROR}"
            )

        async def spawn(self, prompt, **kwargs):
            self._raise_missing()

        async def find_models(self, query="", limit=8):
            self._raise_missing()

        async def create_session(self, prompt, **kwargs):
            self._raise_missing()

        async def list_subagents(self):
            self._raise_missing()

        async def delete_subagent(self, target):
            self._raise_missing()

    rlm = _PrimeAgentMissingRlm()

    def bash(command, persistent=False):
        rlm._raise_missing()
