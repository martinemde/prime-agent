# Python kernel startup scripts

Prime Agent's Python REPL starts every kernel by running Python startup
scripts in the REPL namespace. Configuring the agent's runtime means editing
those scripts: whatever they import, define, or install is the agent's
starting state.

| File | Role | Python analogue |
| --- | --- | --- |
| `system.py` | The interpreter's initial state. Prime Agent ships a default; a `system.py` in the config directory replaces it. | `sitecustomize` |
| `user.py` | Your additions. Runs after the system setup and after Python skills are imported. | `usercustomize` |

Both files live in the Prime Agent config directory: `~/.prime/agent/`, or
`$PRIME_AGENT_CODING_AGENT_DIR` when that is set.

## When they run

At every kernel start, after a saved namespace is restored, in this order:

1. `system.py` (yours, or the shipped default)
2. Python skill imports
3. `user.py`

Like `PYTHONSTARTUP`, each script runs in the REPL's `__main__` namespace, so
its top-level names are visible to the model's cells. The model sees both
scripts verbatim as the first `python_repl` cell of its conversation, so it
knows what already ran; keep secrets out of them. Top-level `await` works
as it does in a cell. If a script raises, its traceback is printed to the
kernel's stderr and startup continues. A change takes effect at the next
kernel start, for example a new session or `/reload`.

## Pre-installed packages

Each script can declare packages with [PEP 723] inline script metadata. The
kernel venv installs the `dependencies` of both scripts, and the system prompt
lists them to the model as pre-installed packages:

```python
# /// script
# dependencies = [
#   "rich",
#   "polars>=1",
# ]
# ///
import polars as pl
from rich import print
```

Packages are installed into the shared kernel venv the first time a kernel
starts with them, and stay installed when you remove them from a script. A
package that fails to install prints a warning and is retried at the next
start. With `PRIME_AGENT_KERNEL_PYTHON`, nothing is installed: that interpreter
must already have every declared package.

## The shipped `system.py`

The default system setup binds `rlm`, `bash`, and `mcp`, plus `asyncio` and
`NO_COLOR=1`, and declares the default packages (`requests`, `httpx`,
`pyyaml`, `pandas`, `numpy`, and others). Its comments are the model's API
reference for those names, read from the first cell. Its source is
[`crates/pa-core/src/kernel/bootstrap/system.py`](../crates/pa-core/src/kernel/bootstrap/system.py).

Prefer `user.py` for additions. Replace `system.py` only when you want a
different base: copy the shipped file and edit it. The first cell shows your
replacement instead of the shipped file, so keep the API comments for
whatever it binds.

[PEP 723]: https://peps.python.org/pep-0723/
