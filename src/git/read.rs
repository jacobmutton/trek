//! Read-only git queries via git2. These never mutate the repo.

use std::path::Path;

use anyhow::{Context, Result};
use git2::{ErrorCode as G2Code, Repository, StatusOptions};

/// Open a repository at `path` (no discovery — `path` must be the repo root).
pub fn open(path: &Path) -> Result<Repository> {
    Repository::open(path).with_context(|| format!("open git repo {}", path.display()))
}

/// Currently checked-out branch name (short form). Returns `None` if HEAD is
/// detached.
pub fn current_branch(repo: &Repository) -> Result<Option<String>> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(e) if e.code() == G2Code::UnbornBranch => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !head.is_branch() {
        return Ok(None);
    }
    Ok(head.shorthand().map(|s| s.to_string()))
}

/// SHA at HEAD (40-char hex). Errors if HEAD is unborn.
pub fn head_sha(repo: &Repository) -> Result<String> {
    let head = repo.head().context("read HEAD")?;
    let oid = head.target().context("HEAD has no oid")?;
    Ok(oid.to_string())
}

/// Returns true if there are any uncommitted changes (staged, unstaged, or
/// untracked).
pub fn is_dirty(repo: &Repository) -> Result<bool> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(!statuses.is_empty())
}

/// Short `git status -s`-style summary, for error details.
pub fn dirty_summary(repo: &Repository, max_lines: usize) -> Result<String> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut out = String::new();
    for (i, s) in statuses.iter().enumerate() {
        if i >= max_lines {
            out.push_str("...\n");
            break;
        }
        let flag = s.status();
        let marker = if flag.is_wt_new() {
            "??"
        } else if flag.is_wt_modified() || flag.is_index_modified() {
            " M"
        } else if flag.is_wt_deleted() || flag.is_index_deleted() {
            " D"
        } else {
            " ?"
        };
        if let Some(path) = s.path() {
            out.push_str(&format!("{marker} {path}\n"));
        }
    }
    Ok(out)
}

pub fn branch_exists(repo: &Repository, name: &str) -> Result<bool> {
    match repo.find_branch(name, git2::BranchType::Local) {
        Ok(_) => Ok(true),
        Err(e) if e.code() == G2Code::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// `child` is an ancestor of `parent` if `parent` reaches it via first-parent.
/// We use this to check "is branch merged into baseline".
pub fn is_merged_into(repo: &Repository, branch: &str, into: &str) -> Result<bool> {
    let b = match repo.revparse_single(branch) {
        Ok(o) => o,
        Err(_) => return Ok(false),
    };
    let i = match repo.revparse_single(into) {
        Ok(o) => o,
        Err(_) => return Ok(false),
    };
    let res = repo.merge_base(b.id(), i.id())?;
    Ok(res == b.id())
}

/// One of "rebase" | "merge" | "cherry-pick" | "bisect" if mid-op, else None.
/// Detected by the presence of the usual files under `.git/`.
pub fn mid_operation(repo: &Repository) -> Option<&'static str> {
    let dot = repo.path(); // <repo>/.git for non-bare
    let markers: &[(&str, &str)] = &[
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("MERGE_HEAD", "merge"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("BISECT_LOG", "bisect"),
    ];
    for (file, label) in markers {
        if dot.join(file).exists() {
            return Some(label);
        }
    }
    None
}

/// Returns Some(remote-tracking-branch-name) for `branch` if configured.
pub fn upstream_of(repo: &Repository, branch: &str) -> Result<Option<String>> {
    let b = match repo.find_branch(branch, git2::BranchType::Local) {
        Ok(b) => b,
        Err(e) if e.code() == G2Code::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match b.upstream() {
        Ok(u) => Ok(u.name()?.map(|s| s.to_string())),
        Err(e) if e.code() == G2Code::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Count of commits on `branch` not reachable from its upstream (i.e. unpushed).
/// Returns 0 if no upstream configured (caller decides whether to treat that
/// as an error).
pub fn unpushed_count(repo: &Repository, branch: &str) -> Result<usize> {
    let Some(upstream) = upstream_of(repo, branch)? else {
        return Ok(0);
    };
    let local = repo.revparse_single(branch)?.id();
    let remote = repo.revparse_single(&upstream)?.id();
    let (ahead, _behind) = repo.graph_ahead_behind(local, remote)?;
    Ok(ahead)
}

/// Resolve `origin/HEAD` to a short branch name (e.g. "main").
pub fn origin_head(repo: &Repository) -> Result<Option<String>> {
    let r = match repo.find_reference("refs/remotes/origin/HEAD") {
        Ok(r) => r,
        Err(e) if e.code() == G2Code::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let target = r.symbolic_target().map(|s| s.to_string());
    // refs/remotes/origin/main -> main
    Ok(target.and_then(|t| t.strip_prefix("refs/remotes/origin/").map(|s| s.to_string())))
}

/// Convenience: open + current_branch + head_sha + is_dirty for snapshotting.
pub struct RepoState {
    pub branch: Option<String>,
    pub head_sha: String,
    pub dirty: bool,
}

pub fn state(path: &Path) -> Result<RepoState> {
    let repo = open(path)?;
    let branch = current_branch(&repo)?;
    let head_sha = head_sha(&repo)?;
    let dirty = is_dirty(&repo)?;
    Ok(RepoState {
        branch,
        head_sha,
        dirty,
    })
}
