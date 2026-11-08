---
name: goal
description: Manage the persistent thread goal from the Python REPL. Use to read goal status and budget usage, to start a goal when the user explicitly asks for one, to record a user or external blocker, to resume after the dependency is satisfied, or to mark the active goal complete once its objective is fully achieved.
---

# Goal

The thread goal is a persistent objective the harness keeps re-prompting you to
pursue while it is runnable. A blocked goal retains the objective without
automatic goal wake-ups until explicitly resumed. Goal state (status, token
budget, usage accounting, blockers) lives in the host; this skill is the
kernel-side interface to it. Call it directly from the Python REPL:

```python
await goal.get()
await goal.create("ship the release notes")
# Only pass token_budget when the user explicitly asks for one:
# await goal.create("ship the release notes", token_budget=200000)
# Required work is waiting for a dependency, not complete:
await goal.block(
    "Release approval is missing",
    waiting_on="user",
    resume_when="The user approves the release",
)
# Only after the recorded dependency is explicitly satisfied:
await goal.resume()
await goal.complete()
```

## API

- `await goal.get()` — current goal as a dict: `goal` (or `None` when no goal
  is set), `remaining_tokens`, and `completion_budget_report`. The `goal` dict
  carries `objective`, `status`, `token_budget`, `tokens_used`,
  `time_used_seconds`, timestamps, and optional `blocker` details.
- `await goal.create(objective, token_budget=None)` — start a new active goal.
  Fails while a goal is still pending (active, blocked, paused, or budget-limited); a
  completed or errored goal is replaced by the new one. Only create a goal when
  the user or system/developer instructions explicitly ask for a persistent
  long-running goal; do not infer goals from ordinary tasks. Set `token_budget`
  only when an explicit token budget is requested.
- `await goal.block(reason, waiting_on, resume_when, independent_work_remaining=False)`
  — record a required dependency and mark the goal blocked without claiming
  completion. `waiting_on` is `"user"` or `"external"`; `reason` explains what
  prevents progress, and `resume_when` states the concrete condition that
  unblocks it. If independent work remains, do it before blocking: the host
  rejects `independent_work_remaining=True`. Do not repeatedly retry the
  blocked work.
- `await goal.resume()` — resume a blocked goal after explicit evidence that
  its recorded dependency is satisfied. External completion messages let you
  reassess the dependency, then explicitly call this API. Unrelated user
  input and elapsed time do not automatically resume a goal.
- `await goal.complete()` — mark the existing goal achieved. Use only when the
  objective has actually been achieved and no required work remains; do not
  call it merely because the budget is nearly exhausted or because you are
  stopping work. When the result includes a `completion_budget_report`, report
  that final usage to the user.

## Rules

- Pause, clear, and budget-limiting are controlled by the user and the host.
- A blocked goal remains incomplete. Do not call `goal.complete()` to stop
  reminders, and do not resume until the recorded dependency is satisfied.
- The host may also block after consecutive turns produce no output. This
  empty-output safeguard is not a semantic progress evaluator: it cannot
  detect repeated attempts that produce output but do not advance the goal.
  Reassess the recorded blocker before explicitly resuming.
- When an active goal is actually complete, call `await goal.complete()`; do
  not merely say it is done — the harness keeps continuing the goal until the
  completion call arrives.
