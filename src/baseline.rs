//! Resolve a repo's baseline branch.

use anyhow::Result;

use crate::config::Repo;
use crate::git::read;

/// `repo.baseline` if set, otherwise `origin/HEAD` resolved to a short branch
/// name (e.g. "main"). Returns the *branch name*, not a remote-tracking ref —
/// callers can prefix `origin/` if they want a remote-tracking ref.
pub fn resolve(repo: &Repo) -> Result<String> {
    if let Some(b) = &repo.baseline {
        return Ok(b.clone());
    }
    let r = read::open(&repo.path)?;
    if let Some(b) = read::origin_head(&r)? {
        return Ok(b);
    }
    // Last resort: try `main`, then `master`.
    for guess in ["main", "master"] {
        if read::branch_exists(&r, guess)? {
            return Ok(guess.to_string());
        }
    }
    anyhow::bail!(
        "could not determine baseline for repo `{}`: no baseline set, no origin/HEAD, no main/master",
        repo.name
    )
}
