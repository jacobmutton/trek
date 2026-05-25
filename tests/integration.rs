//! End-to-end integration tests: drive the `trek` binary against temp git
//! repos, assert on JSON envelope shape, exit codes, and on-disk effects.

mod common;

use common::{Env, commit_file, git, git_stdout};
use serde_json::Value;

// -- init ----------------------------------------------------------------

#[test]
fn init_creates_trek_toml() {
    let env = Env::new(&[]);
    // Re-init by removing the existing trek.toml first.
    std::fs::remove_file(env.workspace.join("trek.toml")).unwrap();
    // `init` looks at cwd, so we invoke from the workspace dir specifically.
    let out = std::process::Command::new(&env.binary)
        .arg("init")
        .arg("--json")
        .current_dir(&env.workspace)
        .env("HOME", &env.home)
        .env("XDG_STATE_HOME", &env.xdg_state)
        .env("XDG_CONFIG_HOME", &env.xdg_config)
        .env("USER", "u")
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(env.workspace.join("trek.toml").is_file());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "trek.v1");
    assert_eq!(v["command"], "init");
    assert_eq!(v["ok"], true);
    assert!(v["data"]["workspace_id"].is_string());
}

#[test]
fn init_refuses_overwrite() {
    let env = Env::new(&["api"]);
    let out = std::process::Command::new(&env.binary)
        .arg("init")
        .arg("--json")
        .current_dir(&env.workspace)
        .env("HOME", &env.home)
        .env("XDG_STATE_HOME", &env.xdg_state)
        .env("XDG_CONFIG_HOME", &env.xdg_config)
        .env("USER", "u")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

// -- start / adopt -------------------------------------------------------

#[test]
fn start_creates_branch_and_worktree() {
    let env = Env::new(&["api"]);
    let r = env.trek(&["start", "FUN-1", "--repos", "api"]);
    r.ok();
    let v = r.json();
    assert_eq!(v["data"]["ticket"], "FUN-1");
    assert_eq!(v["data"]["repos"][0]["repo"], "api");
    assert_eq!(v["data"]["repos"][0]["action"], "created");

    // Worktree exists.
    let api = env.repo("api");
    assert!(api.worktree_dir.join("FUN-1").is_dir());
    // Branch exists in the main repo.
    let branches = git_stdout(&api.preprod, &["branch", "--list", "u-FUN-1"]);
    assert!(branches.contains("u-FUN-1"), "branch not found: {branches}");
}

#[test]
fn start_is_idempotent() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-2", "--repos", "api"]).ok();
    // Second call should be a no-op for the same repo+ticket.
    let r = env.trek(&["start", "FUN-2", "--repos", "api"]);
    r.ok();
    let v = r.json();
    assert_eq!(v["data"]["repos"][0]["action"], "already_present");
}

#[test]
fn start_rejects_invalid_ticket() {
    let env = Env::new(&["api"]);
    let r = env.trek(&["start", "bad/ticket", "--repos", "api"]);
    r.err(1, "INVALID_TICKET_ID");
}

#[test]
fn start_rejects_unknown_repo() {
    let env = Env::new(&["api"]);
    let r = env.trek(&["start", "FUN-3", "--repos", "nope"]);
    r.err(1, "REPO_NOT_IN_WORKSPACE");
}

#[test]
fn start_enforces_ticket_regex() {
    let env = Env::new(&["api"]);
    let id = uuid::Uuid::new_v4();
    env.write_trek_toml(&format!(
        r#"
            [workspace]
            name = "test"
            id = "{id}"
            [branch]
            pattern = "u-{{ticket}}"
            suffix_join = "-"
            ticket_regex = "^[A-Z]+-\\d+$"
            [[repos]]
            name = "api"
            path = "{}"
            worktree_dir = "{}"
            baseline = "main"
        "#,
        env.repo("api").preprod.display(),
        env.repo("api").worktree_dir.display(),
    ));
    env.trek(&["start", "lower-99", "--repos", "api"])
        .err(1, "INVALID_TICKET_ID");
    env.trek(&["start", "ABC-99", "--repos", "api"]).ok();
}

#[test]
fn adopt_reuses_existing_branch() {
    let env = Env::new(&["api"]);
    let api = env.repo("api");
    // Create a branch outside of trek.
    git(&api.preprod, &["branch", "u-ADOPTED-1"]);
    let r = env.trek(&["adopt", "ADOPTED-1", "--repos", "api"]);
    r.ok();
    assert_eq!(r.json()["data"]["repos"][0]["action"], "adopted");
    assert!(api.worktree_dir.join("ADOPTED-1").is_dir());
}

#[test]
fn adopt_errors_when_branch_missing() {
    let env = Env::new(&["api"]);
    let r = env.trek(&["adopt", "ABSENT-1", "--repos", "api"]);
    // Per-repo failure surfaces as PartialFailure (exit 3) with the per-repo
    // error code embedded.
    assert_eq!(r.status, 3);
    let v = r.json();
    assert_eq!(v["error"]["code"], "PARTIAL_FAILURE");
    assert_eq!(
        v["error"]["details"]["repos"][0]["error_code"],
        "BRANCH_NOT_FOUND"
    );
}

#[test]
fn start_with_suffix_creates_distinct_branch() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-9", "--repos", "api", "--suffix", "migration"])
        .ok();
    let api = env.repo("api");
    assert!(api.worktree_dir.join("FUN-9-migration").is_dir());
    let branches = git_stdout(&api.preprod, &["branch", "--list", "u-FUN-9-migration"]);
    assert!(branches.contains("u-FUN-9-migration"));
}

#[test]
fn start_at_suffix_creates_stacked_branch() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-10", "--repos", "api", "--suffix", "migration"])
        .ok();
    // Create the default branch stacked on top of the migration branch.
    let r = env.trek(&["start", "FUN-10", "--repos", "api", "--from", "@migration"]);
    r.ok();
    let v = r.json();
    let api = env.repo("api");
    assert!(api.worktree_dir.join("FUN-10").is_dir());
    // Check the `from` was recorded.
    let status = env.trek(&["status", "FUN-10"]);
    let s = status.json();
    let branches = &s["data"]["ticket"]["branches"];
    let default = branches
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["suffix"].is_null())
        .unwrap();
    assert_eq!(default["from"], "u-FUN-10-migration");
    let _ = v;
}

// -- stage ---------------------------------------------------------------

#[test]
fn stage_switches_preprod_to_ticket_branch() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    // Commit on the ticket branch so it diverges from main; otherwise
    // SHAs are identical and stage reports "already_on" (correct, but not
    // what we're testing here).
    let api = env.repo("api");
    commit_file(&api.worktree_dir.join("FUN-1"), "f.txt", "x", "ticket change");
    let r = env.trek(&["stage", "FUN-1"]);
    r.ok();
    let v = r.json();
    assert_eq!(v["data"]["repos"][0]["action"], "switched");
    let head = git_stdout(&api.preprod, &["rev-parse", "HEAD"]);
    let target = git_stdout(&api.preprod, &["rev-parse", "u-FUN-1"]);
    assert_eq!(head, target);
    // Preprod should be detached (the ticket branch is owned by the worktree).
    let head_state = git_stdout(&api.preprod, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head_state, "HEAD", "preprod should be detached after stage");
}

#[test]
fn second_stage_with_different_ticket_errors() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["start", "FUN-2", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["stage", "FUN-2"]).err(2, "ALREADY_STAGED");
}

#[test]
fn stage_same_ticket_twice_is_noop() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["stage", "FUN-1"]).ok(); // no-op
}

#[test]
fn stage_refuses_dirty_preprod() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let api = env.repo("api");
    std::fs::write(api.preprod.join("dirt.txt"), "x").unwrap();
    env.trek(&["stage", "FUN-1"]).err(2, "DIRTY_WORKTREE");
}

#[test]
fn stage_refuses_detached_preprod() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let api = env.repo("api");
    git(&api.preprod, &["checkout", "--detach", "HEAD"]);
    env.trek(&["stage", "FUN-1"]).err(2, "DETACHED_HEAD");
}

#[test]
fn stage_branch_not_found() {
    let env = Env::new(&["api"]);
    // No ticket record at all — TicketNotFound.
    env.trek(&["stage", "GONE-1"]).err(1, "TICKET_NOT_FOUND");
}

#[test]
fn stage_keep_others_leaves_orphans() {
    let env = Env::new(&["api", "web"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let web = env.repo("web");
    // Move web off main so we can see whether stage --keep-others touched it.
    git(&web.preprod, &["checkout", "-b", "scratch"]);
    let before = git_stdout(&web.preprod, &["symbolic-ref", "--short", "HEAD"]);
    env.trek(&["stage", "FUN-1", "--keep-others"]).ok();
    let after = git_stdout(&web.preprod, &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(before, after, "keep-others should not touch orphan repo");
}

#[test]
fn stage_default_switches_orphans_to_baseline() {
    let env = Env::new(&["api", "web"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let web = env.repo("web");
    git(&web.preprod, &["checkout", "-b", "scratch"]);
    env.trek(&["stage", "FUN-1"]).ok();
    let after = git_stdout(&web.preprod, &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(after, "main", "orphan should have been reset to baseline");
}

#[test]
fn stage_orphan_fail_mode_errors() {
    let env = Env::new(&["api", "web"]);
    let id = uuid::Uuid::new_v4();
    env.write_trek_toml(&format!(
        r#"
            [workspace]
            name = "t"
            id = "{id}"
            [branch]
            pattern = "u-{{ticket}}"
            [stage]
            orphan_repos = "fail"
            [[repos]]
            name = "api"
            path = "{}"
            worktree_dir = "{}"
            baseline = "main"
            [[repos]]
            name = "web"
            path = "{}"
            worktree_dir = "{}"
            baseline = "main"
        "#,
        env.repo("api").preprod.display(),
        env.repo("api").worktree_dir.display(),
        env.repo("web").preprod.display(),
        env.repo("web").worktree_dir.display(),
    ));
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).err(2, "BRANCH_NOT_FOUND");
}

// -- unstage -------------------------------------------------------------

#[test]
fn unstage_restores_snapshot() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let api = env.repo("api");
    let before = git_stdout(&api.preprod, &["symbolic-ref", "--short", "HEAD"]);
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["unstage"]).ok();
    let after = git_stdout(&api.preprod, &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(before, after, "unstage should restore to pre-stage branch");
}

#[test]
fn unstage_errors_when_nothing_staged() {
    let env = Env::new(&["api"]);
    env.trek(&["unstage"]).err(2, "NOT_STAGED");
}

#[test]
fn unstage_detects_drift() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    let api = env.repo("api");
    // User moves preprod to a different *named* branch — that's drift.
    git(&api.preprod, &["checkout", "-b", "user-moved"]);
    // Going via stderr details — DRIFT_DETECTED is per-repo, surfaced as
    // PartialFailure with details.
    let r = env.trek(&["unstage"]);
    assert_eq!(r.status, 3);
    let v = r.json();
    assert_eq!(
        v["error"]["details"]["repos"][0]["error_code"],
        "DRIFT_DETECTED"
    );
}

#[test]
fn unstage_to_baseline_bypasses_snapshot() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["unstage", "--to-baseline"]).ok();
    let api = env.repo("api");
    let cur = git_stdout(&api.preprod, &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(cur, "main");
}

// -- cleanup -------------------------------------------------------------

#[test]
fn cleanup_removes_worktrees_and_branches() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let api = env.repo("api");
    assert!(api.worktree_dir.join("FUN-1").exists());
    env.trek(&["cleanup", "FUN-1"]).ok();
    assert!(!api.worktree_dir.join("FUN-1").exists());
    let branches = git_stdout(&api.preprod, &["branch", "--list", "u-FUN-1"]);
    assert!(branches.is_empty(), "expected branch removed: {branches}");
}

#[test]
fn cleanup_unstages_first_when_staged() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["cleanup", "FUN-1"]).ok();
    let api = env.repo("api");
    let cur = git_stdout(&api.preprod, &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(cur, "main");
    // The staged record was cleared, so refresh should work.
    env.trek(&["refresh"]).ok();
}

#[test]
fn cleanup_keep_merged_preserves_merged_branch() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let api = env.repo("api");
    // Merge u-FUN-1 into main so it's "merged".
    git(&api.preprod, &["merge", "--ff-only", "u-FUN-1"]);
    env.trek(&["cleanup", "FUN-1", "--keep-merged"]).ok();
    let branches = git_stdout(&api.preprod, &["branch", "--list", "u-FUN-1"]);
    assert!(
        branches.contains("u-FUN-1"),
        "merged branch should be kept, got: {branches:?}"
    );
}

// -- refresh -------------------------------------------------------------

#[test]
fn refresh_pulls_from_origin() {
    let env = Env::new(&["api"]);
    let api = env.repo("api");
    // Make a new commit directly on origin (simulating upstream movement).
    let seed_clone = env.root.path().join("api-seed-2");
    git(env.root.path(), &["clone", api.origin.to_str().unwrap(), "api-seed-2"]);
    git(&seed_clone, &["config", "user.email", "t@e"]);
    git(&seed_clone, &["config", "user.name", "T E"]);
    commit_file(&seed_clone, "new.txt", "x", "upstream");
    git(&seed_clone, &["push", "origin", "main"]);

    let before = git_stdout(&api.preprod, &["rev-parse", "HEAD"]);
    let origin_sha = git_stdout(&seed_clone, &["rev-parse", "HEAD"]);
    assert_ne!(before, origin_sha);

    env.trek(&["refresh"]).ok();

    let after = git_stdout(&api.preprod, &["rev-parse", "HEAD"]);
    assert_eq!(after, origin_sha);
}

#[test]
fn refresh_refuses_while_staged() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["refresh"]).err(2, "ALREADY_STAGED");
}

#[test]
fn refresh_refuses_dirty() {
    let env = Env::new(&["api"]);
    std::fs::write(env.repo("api").preprod.join("dirt"), "x").unwrap();
    env.trek(&["refresh"]).err(2, "DIRTY_WORKTREE");
}

// -- status / list / path / where ---------------------------------------

#[test]
fn list_shows_tickets_and_staged() {
    let env = Env::new(&["api", "web"]);
    env.trek(&["start", "FUN-1", "--repos", "api,web"]).ok();
    env.trek(&["start", "FUN-2", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    let r = env.trek(&["list"]);
    r.ok();
    let v = r.json();
    assert_eq!(v["data"]["staged"], "FUN-1");
    let tickets = v["data"]["tickets"].as_array().unwrap();
    let ids: Vec<&str> = tickets.iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"FUN-1"));
    assert!(ids.contains(&"FUN-2"));
}

#[test]
fn status_reports_staged_flag() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let r = env.trek(&["status", "FUN-1"]);
    r.ok();
    assert_eq!(r.json()["data"]["staged"], false);
    env.trek(&["stage", "FUN-1"]).ok();
    let r = env.trek(&["status", "FUN-1"]);
    assert_eq!(r.json()["data"]["staged"], true);
}

#[test]
fn status_defaults_to_staged_ticket() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    env.trek(&["stage", "FUN-1"]).ok();
    let r = env.trek(&["status"]);
    r.ok();
    assert_eq!(r.json()["data"]["ticket"]["id"], "FUN-1");
}

#[test]
fn status_with_no_arg_and_nothing_staged_errors() {
    let env = Env::new(&["api"]);
    env.trek(&["status"]).err(2, "NOT_STAGED");
}

#[test]
fn path_returns_worktree_or_preprod() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    let wt = env.trek(&["path", "FUN-1", "api"]);
    wt.ok();
    let expected = env.repo("api").worktree_dir.join("FUN-1");
    assert_eq!(
        wt.json()["data"]["path"].as_str().unwrap(),
        expected.to_str().unwrap()
    );
    let pp = env.trek(&["path", "FUN-1", "api", "--preprod"]);
    pp.ok();
    assert_eq!(
        pp.json()["data"]["path"].as_str().unwrap(),
        env.repo("api").preprod.to_str().unwrap()
    );
}

// -- run -----------------------------------------------------------------

#[test]
fn run_executes_in_each_worktree() {
    let env = Env::new(&["api", "web"]);
    env.trek(&["start", "FUN-1", "--repos", "api,web"]).ok();
    let r = env.trek(&["run", "--in", "worktree", "-t", "FUN-1", "--", "pwd"]);
    r.ok();
    let v = r.json();
    let repos = v["data"]["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 2);
    for repo in repos {
        let stdout = repo["stdout"].as_str().unwrap();
        let name = repo["repo"].as_str().unwrap();
        // pwd should print a path under the worktree dir.
        let expected = env.repo(name).worktree_dir.join("FUN-1");
        assert!(
            stdout.contains(expected.file_name().unwrap().to_str().unwrap()),
            "[{name}] pwd output {stdout:?} should mention {}",
            expected.display()
        );
        assert_eq!(repo["exit_code"], 0);
    }
}

#[test]
fn run_preprod_requires_staged_match() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-1", "--repos", "api"]).ok();
    // Nothing staged.
    env.trek(&["run", "--in", "preprod", "-t", "FUN-1", "--", "true"])
        .err(2, "NOT_STAGED");
    env.trek(&["stage", "FUN-1"]).ok();
    env.trek(&["start", "FUN-2", "--repos", "api"]).ok();
    // Wrong ticket.
    env.trek(&["run", "--in", "preprod", "-t", "FUN-2", "--", "true"])
        .err(2, "NOT_STAGED");
    // Right ticket — ok.
    env.trek(&["run", "--in", "preprod", "-t", "FUN-1", "--", "true"])
        .ok();
}

#[test]
fn run_aggregates_partial_failure() {
    let env = Env::new(&["api", "web"]);
    env.trek(&["start", "FUN-1", "--repos", "api,web"]).ok();
    let r = env.trek(&["run", "--in", "worktree", "-t", "FUN-1", "--", "false"]);
    assert_eq!(r.status, 3);
    let v = r.json();
    assert_eq!(v["error"]["code"], "PARTIAL_FAILURE");
}

// -- doctor --------------------------------------------------------------

#[test]
fn doctor_reports_healthy() {
    let env = Env::new(&["api", "web"]);
    let r = env.trek(&["doctor"]);
    r.ok();
    let v = r.json();
    assert_eq!(v["data"]["ok"], true);
    assert_eq!(v["data"]["repos"].as_array().unwrap().len(), 2);
}

#[test]
fn doctor_flags_missing_repo_path() {
    let env = Env::new(&["api"]);
    let id = uuid::Uuid::new_v4();
    // Point at a path that doesn't exist.
    env.write_trek_toml(&format!(
        r#"
            [workspace]
            name = "t"
            id = "{id}"
            [[repos]]
            name = "ghost"
            path = "{}/nope"
            worktree_dir = "{}/wt"
            baseline = "main"
        "#,
        env.root.path().display(),
        env.root.path().display(),
    ));
    let r = env.trek(&["doctor"]);
    r.ok(); // doctor always exits 0; verdict is in data.ok
    let v = r.json();
    assert_eq!(v["data"]["ok"], false);
    assert_eq!(v["data"]["repos"][0]["path_exists"], false);
}

// -- sync / push ---------------------------------------------------------

#[test]
fn sync_rebases_stacked_branch_after_base_moves() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-S", "--repos", "api", "--suffix", "base"])
        .ok();
    env.trek(&["start", "FUN-S", "--repos", "api", "--from", "@base"])
        .ok();
    let api = env.repo("api");
    let base_wt = api.worktree_dir.join("FUN-S-base");
    let top_wt = api.worktree_dir.join("FUN-S");
    // Commit on base.
    commit_file(&base_wt, "b.txt", "1", "base change");
    // Commit on top of (now stale) base.
    commit_file(&top_wt, "t.txt", "1", "top change");
    // Add another commit on base after top was created — top is now stale.
    commit_file(&base_wt, "b2.txt", "2", "base change 2");

    env.trek(&["sync", "FUN-S"]).ok();

    // Top branch should include the base's latest commit as an ancestor.
    let base_sha = git_stdout(&base_wt, &["rev-parse", "HEAD"]);
    let top_log = git_stdout(&top_wt, &["log", "--format=%H", "HEAD"]);
    assert!(
        top_log.contains(&base_sha),
        "top should have base's new commit in its history after sync"
    );
}

#[test]
fn sync_refuses_dirty_worktree() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-X", "--repos", "api"]).ok();
    let api = env.repo("api");
    std::fs::write(api.worktree_dir.join("FUN-X").join("dirt"), "x").unwrap();
    env.trek(&["sync", "FUN-X"]).err(2, "DIRTY_WORKTREE");
}

#[test]
fn push_pushes_branch_to_origin() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-P", "--repos", "api"]).ok();
    let api = env.repo("api");
    commit_file(&api.worktree_dir.join("FUN-P"), "p.txt", "x", "push me");

    let r = env.trek(&["push", "FUN-P", "-u"]);
    r.ok();
    let v = r.json();
    assert_eq!(v["data"]["branches"][0]["action"], "pushed");

    // Bare origin should now have refs/heads/u-FUN-P.
    let refs = git_stdout(&api.origin, &["for-each-ref", "--format=%(refname)"]);
    assert!(refs.contains("refs/heads/u-FUN-P"), "origin should have the branch:\n{refs}");
}

#[test]
fn push_idempotent_returns_up_to_date() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-P", "--repos", "api"]).ok();
    let api = env.repo("api");
    commit_file(&api.worktree_dir.join("FUN-P"), "p.txt", "x", "first");
    env.trek(&["push", "FUN-P", "-u"]).ok();
    let r = env.trek(&["push", "FUN-P"]);
    r.ok();
    assert_eq!(r.json()["data"]["branches"][0]["action"], "up_to_date");
}

// -- hooks ---------------------------------------------------------------

#[test]
fn post_start_hook_runs_with_env() {
    let env = Env::new(&["api"]);
    let id = uuid::Uuid::new_v4();
    let marker = env.root.path().join("hook-fired.txt");
    env.write_trek_toml(&format!(
        r#"
            [workspace]
            name = "t"
            id = "{id}"
            [branch]
            pattern = "u-{{ticket}}"
            [stage]
            orphan_repos = "baseline"
            [hooks]
            post_start = "echo $TREK_TICKET-$TREK_EXIT_CODE > {}"
            [[repos]]
            name = "api"
            path = "{}"
            worktree_dir = "{}"
            baseline = "main"
        "#,
        marker.display(),
        env.repo("api").preprod.display(),
        env.repo("api").worktree_dir.display(),
    ));
    env.trek(&["start", "FUN-H", "--repos", "api"]).ok();
    let body = std::fs::read_to_string(&marker).expect("hook marker should exist");
    assert!(body.trim() == "FUN-H-0", "hook body: {body:?}");
}

// -- where ---------------------------------------------------------------

#[test]
fn where_detects_worktree_with_suffix() {
    let env = Env::new(&["api"]);
    env.trek(&["start", "FUN-W", "--repos", "api", "--suffix", "migration"])
        .ok();
    let wt_dir = env.repo("api").worktree_dir.join("FUN-W-migration");
    // Invoke trek where with cwd inside the worktree.
    let mut cmd = std::process::Command::new(&env.binary);
    cmd.arg("where")
        .arg("--json")
        .arg("--non-interactive")
        .current_dir(&wt_dir)
        .env("HOME", &env.home)
        .env("XDG_STATE_HOME", &env.xdg_state)
        .env("XDG_CONFIG_HOME", &env.xdg_config)
        .env("TREK_WORKSPACE", &env.workspace)
        .env("USER", "u");
    let out = cmd.output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["ticket"], "FUN-W");
    assert_eq!(v["data"]["suffix"], "migration");
    assert_eq!(v["data"]["repo"], "api");
    assert_eq!(v["data"]["location"], "worktree");
}
