# `trek` — Multi-Repo Branch Tool

A Rust CLI for coordinating branches, worktrees, and the preprod
checkout across the ~3 repositories that share a single unit of work
(a ticket). Built so an AI coding agent (Claude Code) can drive it:
every command is addressable, idempotent, JSON-capable, and emits
stable error codes. The agent is the brain; `trek` is consistency.

## Problem

A ticket like `FUN-1234` needs branches across ~3 repos. Dev happens
in git worktrees per ticket. Switching the *preprod* (main,
non-worktree) checkouts to a ticket's branches for integration
testing is fragile — dirty trees, forgotten repos, mismatched states,
no easy undo. Doing it by script or agent is worse: every failure
mode is a different `git` invocation with freeform stderr.

`trek` collapses the lifecycle to one command per step.

## Lifecycle

| step | action                          | command                                    |
|------|---------------------------------|--------------------------------------------|
| 1    | refresh preprod to baseline     | `trek refresh`                              |
| 2    | declare ticket + repos          | `trek start FUN-1234 --repos api,web`       |
| 3    | work in worktrees               | agent uses `trek path` / `trek run`          |
| 4    | stage for integration test      | `trek stage FUN-1234`                       |
| 5    | hand off to user                | out of scope                               |
| 6    | cleanup + reset preprod         | `trek cleanup FUN-1234`                     |

## Mental model

Three roles per repo:

- **Production branch** — what's deployed; the baseline for new work.
  Configured per repo (`baseline`), defaults to `origin/HEAD`.
- **Preprod checkout** — the repo's main on-disk checkout, used for
  cross-repo integration testing. `trek stage` puts ticket branches
  here; `trek refresh` / `trek unstage` / `trek cleanup` return it to
  baseline.
- **Worktrees** — one per ticket-branch under the repo's
  `worktree_dir`. Where dev actually happens. Never touched by stage.

Tickets:

- A ticket (e.g. `FUN-1234`) maps to **one or more branches per
  repo**. The default branch (no suffix) is named by the workspace
  pattern; an additional branch added with `--suffix migration` is
  named `{pattern}{suffix_join}migration`. Each branch gets its own
  worktree.
- Every branch-creating command (`start`, `adopt`) takes `--from
  <ref>`, recorded for audit. Defaults to baseline. Accepts a
  `@<suffix>` shorthand resolving to *this ticket's branch with that
  suffix in this repo* — that's how the stacked pattern `baseline ←
  <ticket>-migration ← <ticket>` is expressed:

  ```
  trek start FUN-1234 --repos api --suffix migration         # baseline → jm-FUN-1234-migration
  trek start FUN-1234 --repos api --from @migration          #         → jm-FUN-1234
  ```

  `trek` records `from`; it does not enforce or maintain the rebase
  relationship.

Invariants:

- **One ticket staged at a time** across the workspace. `stage` while
  another ticket is staged errors with `ALREADY_STAGED`. The agent
  must `unstage` or `cleanup` first.
- **Repos in the workspace but not declared on the ticket** are
  switched to baseline during stage. Integration testing requires
  every repo at a known state. The snapshot makes it reversible.
  `--keep-others` opts out.

## Commands

```
trek init                                          # scaffold trek.toml + workspace UUID
trek refresh [--repos …]                           # fetch + ff-pull every preprod to baseline
trek start   <ticket> --repos … [--suffix S] [--from REF]
                                                  # create branch + worktree per repo
trek adopt   <ticket> --repos … [--suffix S] [--from REF]
                                                  # reuse a pre-existing branch
trek stage   <ticket> [--suffix S] [--keep-others]
                                                  # switch preprod to ticket branches
trek unstage [--to-baseline]                       # restore preprod to snapshot or baseline
trek cleanup <ticket> [--keep-merged]              # unstage to baseline + remove worktrees + branches
trek status  [ticket]                              # state of a ticket, or the staged ticket
trek list                                          # all tickets in the workspace
trek path    <ticket> <repo> [--suffix S] [--preprod]
                                                  # absolute path; default = worktree
trek run --in {worktree|preprod} -t <ticket> [--suffix S] [--repos …] -- <cmd…>
                                                  # run a command in each repo
trek where                                         # workspace + ticket the cwd belongs to
```

Global flags: `--json`, `--non-interactive`, `--workspace <path>`,
`--quiet`, `--verbose`. `TREK_WORKSPACE` = `--workspace`.

`trek path`, `trek run`, `trek status --json`, and `--json` everywhere
are the seams an agent uses to compose without re-deriving paths or
parsing prose.

## `trek stage`

1. Acquire workspace lock (or `LOCKED` under `--non-interactive`).
2. Preflight every preprod. Refuse on dirty / detached HEAD /
   mid-rebase / mid-merge / mid-cherry-pick / mid-bisect / unpushed
   commits on the leaving branch. No `--force`; no auto-stash. Output
   offenders with `git status -s` excerpts and the exact commands to
   clear them.
3. Resolve target branches per repo. Missing branch →
   `BRANCH_NOT_FOUND`, stop before touching anything.
4. Snapshot prior state per preprod (branch + HEAD SHA + clean flag)
   into the staging record.
5. `git fetch`, then `git checkout` the targets in parallel. Per-repo
   result: `switched` / `already_on` / `failed: <code>`.
6. Commit the staging record atomically (write + rename). On any
   per-repo failure mark `partial`; exit `3`.
7. Repos in the workspace but not on the ticket are switched to
   baseline (unless `--keep-others`).

`trek unstage` reads the staging record and restores each preprod to
its snapshot branch. Refuses on dirty tree or drift (current branch
differs from the snapshot) with `DRIFT_DETECTED`. `--to-baseline`
skips snapshot comparison and goes to baseline.

`trek cleanup <ticket>` does `unstage --to-baseline` if the ticket is
currently staged, then removes all worktrees and local branches for
the ticket across declared repos. `--keep-merged` keeps branches
already merged to baseline (useful when a PR has landed).

`trek refresh` is `unstage --to-baseline` (when staged) followed by
`git fetch` + `git pull --ff-only` per preprod. Refuses dirty trees.

## State, identity, locking

- **`trek.toml`** lives in the workspace dir. `workspace.id` (UUID)
  is written at `init`; all on-disk state is keyed by it.
- **Transient state** (staging record, audit log, lock) under
  `$XDG_STATE_HOME/trek/<workspace-id>/`. Never committed; survives
  moving the workspace dir.
- **Lock**: flock on `$XDG_STATE_HOME/trek/<workspace-id>/lock`, held
  for the duration of any state-changing command. Read-only commands
  (`status`, `list`, `path`, `where`) take no lock.
- **Audit log**: one JSON line per state-changing command (timestamp,
  argv, exit code).

## Workspace discovery

1. `--workspace <path>` flag → `trek.toml` at that path.
2. `TREK_WORKSPACE` env var → same.
3. Walk up from cwd looking for `trek.toml`.
4. Otherwise `NO_WORKSPACE`.

`trek where` reports the resolved workspace and (if cwd is inside any
declared worktree) the ticket + suffix + `worktree | preprod`.

## JSON contract

`--json` produces one JSON document on stdout; human output goes to
stderr.

```json
{
  "schema": "trek.v1",
  "command": "stage",
  "ok": true,
  "data": { "...": "command-specific" },
  "error": null
}
```

On error:

```json
{
  "schema": "trek.v1",
  "command": "stage",
  "ok": false,
  "data": null,
  "error": {
    "code": "DIRTY_WORKTREE",
    "message": "Repo `api` has uncommitted changes.",
    "details": { "repo": "api", "git_status": "M src/main.rs" }
  }
}
```

Multi-repo commands report per-repo results inside `data.repos[]`
with their own `ok` / `error`. Schema is additive within `trek.v1`;
renames / removals require `trek.v2`.

Stable error codes (initial set): `NO_WORKSPACE`, `LOCKED`,
`DIRTY_WORKTREE`, `DETACHED_HEAD`, `MID_OPERATION`,
`UNPUSHED_COMMITS`, `BRANCH_NOT_FOUND`, `BRANCH_EXISTS`,
`WORKTREE_EXISTS`, `DRIFT_DETECTED`, `ALREADY_STAGED`, `NOT_STAGED`,
`TICKET_NOT_FOUND`, `SUFFIX_NOT_FOUND`, `SUFFIX_EXISTS`,
`REPO_NOT_IN_WORKSPACE`, `INVALID_TICKET_ID`, `PARTIAL_FAILURE`.

Exit codes:

| code | meaning                                                  |
|------|----------------------------------------------------------|
| 0    | ok                                                       |
| 1    | user error (bad args, missing workspace, invalid ticket) |
| 2    | precondition failed (dirty, mid-op, drift, locked)       |
| 3    | partial failure across repos                             |
| 4    | internal error                                           |

## Idempotency

- `trek start FUN-1234 --repos api`: branch + worktree already present
  → no-op. Branch present, worktree missing → create the worktree.
- `trek stage FUN-1234`: already staged for FUN-1234 → no-op. Staged
  for a different ticket → `ALREADY_STAGED`.
- `trek unstage` / `trek refresh`: no-op if already at target state.
- `trek cleanup FUN-1234`: skip repos already cleaned.

`start`, `stage`, `cleanup`, and `refresh` are safe to re-run after
partial failure to converge. An agent resuming after a crash uses
`adopt`.

## `trek run`

```
trek run --in worktree -t FUN-1234 [--suffix migration] [--repos api,web] -- pytest -x
trek run --in preprod  -t FUN-1234 [--repos api]                          -- ./scripts/it.sh
```

- Sequential by default (preserves output order); `--parallel` for
  agents that aggregate JSON.
- Without `--json`: output streamed line-prefixed with `[<repo>] `.
- With `--json`: per-repo `exit_code`, captured `stdout`, `stderr`,
  `duration_ms`.
- Aggregate exit code: `0` iff every per-repo run is `0`; else `3`.
- `--in preprod` requires the ticket to be currently staged and to
  match `-t`. Prevents running integration tests against the wrong
  branches.

## Config sketch (`trek.toml`)

```toml
[workspace]
name = "my-project"
id   = "0193b3f9-..."             # written by `trek init`

[branch]
pattern      = "{user}-{ticket}"  # {user} from $USER unless overridden
suffix_join  = "-"                # joiner between pattern and suffix
ticket_regex = "^[A-Z]+-\\d+$"    # optional; rejects typos at start time

[stage]
orphan_repos = "baseline"         # baseline | leave | fail

[[repos]]
name           = "api"
path           = "~/code/api"
worktree_dir   = "~/worktrees/api"
baseline       = "main"           # optional; defaults to origin/HEAD

[[repos]]
name           = "web"
path           = "~/code/web"
worktree_dir   = "~/worktrees/web"
branch_pattern = "feature/{ticket}"  # per-repo override
baseline       = "develop"
```

Branch name = `branch_pattern` (per-repo override → workspace
`[branch].pattern`) for the default branch; appended with
`{suffix_join}{suffix}` when `--suffix` is set. Worktree path =
`{repo.worktree_dir}/{ticket}` for default, plus the same suffix
suffix otherwise. Ticket strings are restricted to path-safe chars
and validated against `ticket_regex` if set.

`[stage]` and `[branch]` may also live in
`$XDG_CONFIG_HOME/trek/config.toml` as cross-workspace defaults;
`trek.toml` overrides per-key.

## Implementation

- **Crates**: `clap` (derive), `serde` + `toml`, `dialoguer` (skipped
  under `--non-interactive`), `anyhow` + `thiserror`, `owo-colors`,
  `fs2` (flock), `uuid`, `rayon` for per-repo parallelism, `git2` for
  read-only git queries.
- **Git access**: shell out to `git` for state-changing ops (worktree
  add/remove, checkout, fetch, pull) — its CLI handles edge cases
  libgit2 doesn't and matches what the user sees. Use `git2` for fast
  read queries (branch existence, dirtiness, current HEAD).
- **Concurrency**: read ops in parallel; state-changing commands hold
  the workspace lock and parallelize across repos where safe.

## Non-goals (v1)

- Editor / AI / tmux integration. `trek path`, `trek run`, and `--json`
  are the seams.
- PR creation, ticket-system sync.
- Hooks. Plausible later but the surface area (shell semantics, error
  propagation, untrusted input) is too much for v1.
- Submodules, sparse checkouts, monorepo subdirs.
- Cross-workspace operations.
- Auto-stash, force flags, conflict resolution UI.
- Auto-rebase between stacked branches. The agent runs `git rebase`
  in the worktree.

## v1 slice

`init`, `refresh`, `start`, `adopt`, `stage`, `unstage`, `cleanup`,
`status`, `list`, `path`, `run`, `where`. Plus workspace config +
UUID + lock + audit log + JSON contract + stable error codes.

Defer to v2: `sync` (stacked rebase), `push`, `doctor`, hooks.

## Open questions

- **Multi-user workspaces** on a shared machine: per-user state under
  `$XDG_STATE_HOME` already isolates locks and snapshots; worth
  explicitly documenting rather than implying single owner.
- **`@suffix` resolution scope**: this ticket only (current plan), or
  also allow cross-ticket references? Current plan keeps `@` scoped
  to the same ticket; full refs handle anything else.
- **Should `trek refresh` auto-unstage** if a ticket is staged, or
  refuse with `ALREADY_STAGED`? Leaning refuse — explicit `cleanup`
  preserves the safety invariant. Revisit if it adds too many agent
  steps.
