//! Mutating git ops, shelled out to the `git` CLI. We capture stderr so we
//! can return it inside structured errors / JSON `details`.

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use thiserror::Error;

use crate::error::ErrorCode;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git {0} failed (exit {1}): {2}")]
    Failed(String, i32, String),
    #[error("io running git: {0}")]
    Io(#[from] std::io::Error),
}

impl GitError {
    /// Map a git failure to a stable trek error code. The mapping is
    /// best-effort — we look at stderr strings.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Io(_) => ErrorCode::Internal,
            Self::Failed(_, _, stderr) => {
                let s = stderr.to_ascii_lowercase();
                if s.contains("not a valid object name")
                    || s.contains("did not match any") // e.g. "did not match any file(s) known to git"
                    || s.contains("unknown revision")
                {
                    ErrorCode::BranchNotFound
                } else if s.contains("already exists") {
                    ErrorCode::BranchExists
                } else if s.contains("would be overwritten") || s.contains("uncommitted") {
                    ErrorCode::DirtyWorktree
                } else {
                    ErrorCode::Internal
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GitOutcome {
    pub stdout: String,
    pub stderr: String,
}

fn run(cwd: &Path, args: &[&str]) -> Result<GitOutcome, GitError> {
    let out = Command::new("git").current_dir(cwd).args(args).output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(GitError::Failed(
            args.join(" "),
            out.status.code().unwrap_or(-1),
            stderr,
        ));
    }
    Ok(GitOutcome { stdout, stderr })
}

pub fn fetch(cwd: &Path) -> Result<GitOutcome, GitError> {
    run(cwd, &["fetch", "--prune"])
}

pub fn fetch_remote(cwd: &Path, remote: &str) -> Result<GitOutcome, GitError> {
    run(cwd, &["fetch", "--prune", remote])
}

pub fn checkout(cwd: &Path, branch: &str) -> Result<GitOutcome, GitError> {
    run(cwd, &["checkout", branch])
}

pub fn checkout_detach(cwd: &Path, rev: &str) -> Result<GitOutcome, GitError> {
    run(cwd, &["checkout", "--detach", rev])
}

pub fn pull_ff_only(cwd: &Path) -> Result<GitOutcome, GitError> {
    run(cwd, &["pull", "--ff-only"])
}

pub fn create_branch(cwd: &Path, name: &str, from: &str) -> Result<GitOutcome, GitError> {
    run(cwd, &["branch", name, from])
}

pub fn delete_branch(cwd: &Path, name: &str, force: bool) -> Result<GitOutcome, GitError> {
    let flag = if force { "-D" } else { "-d" };
    run(cwd, &["branch", flag, name])
}

pub fn worktree_add(
    cwd: &Path,
    path: &Path,
    branch: &str,
) -> Result<GitOutcome, GitError> {
    let path_s = path
        .to_str()
        .ok_or_else(|| GitError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 worktree path")))?;
    run(cwd, &["worktree", "add", path_s, branch])
}

pub fn worktree_remove(cwd: &Path, path: &Path, force: bool) -> Result<GitOutcome, GitError> {
    let path_s = path
        .to_str()
        .ok_or_else(|| GitError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 worktree path")))?;
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path_s);
    run(cwd, &args)
}

pub fn worktree_prune(cwd: &Path) -> Result<GitOutcome, GitError> {
    run(cwd, &["worktree", "prune"])
}

/// `git rev-parse --verify <rev>` — returns sha if `rev` resolves.
pub fn rev_parse(cwd: &Path, rev: &str) -> Result<String, GitError> {
    let out = run(cwd, &["rev-parse", "--verify", rev])?;
    Ok(out.stdout.trim().to_string())
}
