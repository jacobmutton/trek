# trek

Multi-repo branch + worktree coordinator for ticket-driven work.

`trek` collapses the cross-repo branch/worktree/preprod-checkout dance
into a handful of idempotent commands. It's designed so an AI coding
agent can drive it: every command emits structured JSON with stable
error codes, never prompts under `--non-interactive`, and is safe to
re-run after a partial failure.

The full design document lives in [`trek-design.md`](trek-design.md).

## Install

Requires Rust 1.86+ (edition 2024) and a working `git` on PATH.

```sh
cargo install --path .
```

This builds an optimized binary under `~/.cargo/bin/trek`.

## Quick start

```sh
# In your workspace dir (typically the parent of your repos)
trek init

# Edit trek.toml: declare your repos under [[repos]]

# Start work on a ticket — creates a branch + worktree in every named repo
trek start FUN-1234 --repos api,web

# Do work in the worktrees
trek path FUN-1234 api          # absolute path to the api worktree
cd $(trek path FUN-1234 api)
# ... edit, commit, etc ...

# Switch preprod (the main on-disk checkouts) to this ticket's branches
trek stage FUN-1234

# Run integration tests across every repo's preprod
trek run --in preprod -t FUN-1234 -- ./scripts/integration-test.sh

# Push the branches up for review
trek push FUN-1234 -u

# When the PRs land, tear it all down
trek cleanup FUN-1234 --keep-merged
```

## Lifecycle

| step | action                          | command                                    |
|------|---------------------------------|--------------------------------------------|
| 1    | refresh preprod to baseline     | `trek refresh`                              |
| 2    | declare ticket + repos          | `trek start FUN-1234 --repos api,web`       |
| 3    | work in worktrees               | use `trek path` / `trek run`                |
| 4    | stage for integration test      | `trek stage FUN-1234`                       |
| 5    | (push, hand off, review…)       | `trek push FUN-1234 -u`                     |
| 6    | cleanup + reset preprod         | `trek cleanup FUN-1234`                     |

## Commands

```
trek init                                          # scaffold trek.toml + workspace UUID
trek refresh [--repos …]                           # fetch + ff-pull every preprod to baseline
trek start   <ticket> --repos … [--suffix S] [--from REF]
trek adopt   <ticket> --repos … [--suffix S] [--from REF]
trek stage   <ticket> [--suffix S] [--keep-others]
trek unstage [--to-baseline]
trek cleanup <ticket> [--keep-merged]
trek status  [ticket]
trek list
trek path    <ticket> <repo> [--suffix S] [--preprod]
trek run --in {worktree|preprod} -t <ticket> [--suffix S] [--repos …] [--parallel] -- <cmd…>
trek where
trek doctor
trek sync    <ticket> [--repos …]
trek push    <ticket> [--suffix S] [--repos …] [--remote origin] [-u]
```

Global flags: `--json`, `--non-interactive`, `--workspace <path>`,
`--quiet`, `--verbose`. The env var `TREK_WORKSPACE` is equivalent to
`--workspace`.

### `trek init`

Writes `trek.toml` in the current directory with a fresh workspace
UUID. Refuses to overwrite an existing file.

### `trek start` / `trek adopt`

`start` creates a new branch off `--from` (default: baseline) and a
worktree under each repo's `worktree_dir`. `adopt` reuses an
already-existing branch.

The `--from @<suffix>` shorthand resolves to *this ticket's branch
with that suffix in this repo*, enabling stacks:

```sh
trek start FUN-1234 --repos api --suffix migration         # baseline → u-FUN-1234-migration
trek start FUN-1234 --repos api --from @migration          #         → u-FUN-1234 (stacked)
```

### `trek stage` / `trek unstage`

`stage` is the integration-test pivot: it snapshots every preprod
checkout, then detaches each to the ticket branch's commit. (Detaching
sidesteps git's refusal to check out a branch that's owned by a
worktree.)

Repos in the workspace but not declared on the ticket are switched to
baseline. `--keep-others` opts out. The `[stage].orphan_repos` config
key (`baseline` | `leave` | `fail`) is the default.

`unstage` restores each preprod to its snapshot branch. Refuses if a
preprod has drifted to a different *named* branch. `--to-baseline`
skips snapshot comparison and goes straight to baseline.

### `trek cleanup`

`unstage --to-baseline` (if staged) → remove every worktree → delete
every local branch for the ticket. `--keep-merged` preserves branches
already merged into baseline (useful when a PR has landed).

### `trek refresh`

Fetch + `pull --ff-only` every preprod back to baseline. Refuses if
anything is staged — run `trek cleanup` or `trek unstage` first.

### `trek run`

```sh
trek run --in worktree -t FUN-1234 [--suffix S] [--repos r1,r2] -- pytest -x
trek run --in preprod  -t FUN-1234                              -- ./integration.sh
```

Sequential by default with output line-prefixed by `[<repo>] `. Use
`--parallel` for agents that aggregate JSON. `--in preprod` requires
the ticket to be currently staged and to match `-t`.

### `trek path` / `trek where` / `trek list` / `trek status`

Read-only inspection commands; no lock taken, safe under concurrency.
`trek path` is JSON-friendly so an agent can compose paths without
re-deriving them:

```sh
cd $(trek path FUN-1234 api)
```

### `trek doctor`

One-shot diagnostic: per-repo path exists / is a git repo / baseline
resolves, plus a scan for stale ticket entries (referencing repos no
longer in `trek.toml`). Always exits 0; the JSON `data.ok` field is the
machine-readable verdict.

### `trek sync`

For each branch of a ticket, run `git rebase <recorded-from>` in its
worktree. Stacked branches are walked deps-first. Conservative:
refuses any dirty worktree, and on rebase failure leaves the worktree
mid-rebase for you (or the agent) to resolve.

### `trek push`

`git push [-u] <remote> <branch>` per branch. Idempotent — an already-
up-to-date branch is reported as `up_to_date` not `pushed`.

## Configuration

`trek.toml` (in the workspace dir):

```toml
[workspace]
name = "my-project"
id   = "0193b3f9-..."           # written by `trek init`

[branch]
pattern      = "{user}-{ticket}" # {user} from $USER unless overridden
suffix_join  = "-"
ticket_regex = "^[A-Z]+-\\d+$"   # optional; rejects typos at start time

[stage]
orphan_repos = "baseline"        # baseline | leave | fail

[hooks]
post_start   = "echo started $TREK_TICKET"
post_stage   = "..."
# Hooks run via `sh -c`; available env:
# TREK_COMMAND, TREK_TICKET, TREK_SUFFIX, TREK_EXIT_CODE,
# TREK_WORKSPACE, TREK_WORKSPACE_ID. Failures are logged but never
# change the command's exit code.

[[repos]]
name           = "api"
path           = "~/code/api"
worktree_dir   = "~/worktrees/api"
baseline       = "main"          # optional; defaults to origin/HEAD

[[repos]]
name           = "web"
path           = "~/code/web"
worktree_dir   = "~/worktrees/web"
branch_pattern = "feature/{ticket}"  # per-repo override
baseline       = "develop"
```

Workspace-wide defaults can live in
`$XDG_CONFIG_HOME/trek/config.toml` (only `[branch]` and `[stage]`
tables are honored there); per-key overrides in `trek.toml` take
precedence.

## JSON contract

Every command, with `--json`, emits one JSON document on stdout (human
output goes to stderr):

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
    "message": "repo `api` has uncommitted changes.",
    "details": { "repo": "api", "git_status": " M src/main.rs" }
  }
}
```

Multi-repo commands include per-repo results inside `data.repos[]`,
each with its own `ok` and `error_code`.

### Error codes

`NO_WORKSPACE`, `LOCKED`, `DIRTY_WORKTREE`, `DETACHED_HEAD`,
`MID_OPERATION`, `UNPUSHED_COMMITS`, `BRANCH_NOT_FOUND`,
`BRANCH_EXISTS`, `WORKTREE_EXISTS`, `DRIFT_DETECTED`, `ALREADY_STAGED`,
`NOT_STAGED`, `TICKET_NOT_FOUND`, `SUFFIX_NOT_FOUND`, `SUFFIX_EXISTS`,
`REPO_NOT_IN_WORKSPACE`, `INVALID_TICKET_ID`, `PARTIAL_FAILURE`,
`NOT_IMPLEMENTED`, `INTERNAL`.

### Exit codes

| code | meaning                                                  |
|------|----------------------------------------------------------|
| 0    | ok                                                       |
| 1    | user error (bad args, missing workspace, invalid ticket) |
| 2    | precondition failed (dirty, mid-op, drift, locked)       |
| 3    | partial failure across repos                             |
| 4    | internal error                                           |

## State

Per-workspace state lives under `$XDG_STATE_HOME/trek/<workspace-id>/`
(default `~/.local/state/trek/<workspace-id>/`):

- `lock` — exclusive flock held by any state-changing command
- `staged.json` — the currently-staged ticket + per-repo snapshot
- `tickets/<id>.json` — one file per tracked ticket
- `audit.jsonl` — one JSON line per state-changing command

Nothing under here is ever committed to your repos.

## Concurrency

State-changing commands hold an exclusive flock on the workspace lock
file. Read-only commands (`status`, `list`, `path`, `where`, `doctor`)
take no lock and are safe to run concurrently with anything else.

## Testing

```sh
cargo test
```

The suite is 32 unit tests + 46 integration tests; integration tests
spin up isolated temp git repos and drive the binary end-to-end.

## License
