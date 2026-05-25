//! Branch-name and worktree-path resolution from the config + ticket + suffix.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::config::{Config, Repo};

pub struct Names {
    pub branch: String,
    pub worktree: PathBuf,
}

/// Resolve the branch name and worktree path for a (repo, ticket, suffix).
pub fn resolve(cfg: &Config, repo: &Repo, ticket: &str, suffix: Option<&str>) -> Result<Names> {
    let pattern = cfg.branch_pattern_for(repo);
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let mut branch = expand_pattern(pattern, &user, ticket)?;
    if let Some(s) = suffix {
        branch.push_str(&cfg.branch.suffix_join);
        branch.push_str(s);
    }
    let mut wt_leaf = ticket.to_string();
    if let Some(s) = suffix {
        wt_leaf.push_str(&cfg.branch.suffix_join);
        wt_leaf.push_str(s);
    }
    let worktree = repo.worktree_dir.join(&wt_leaf);
    Ok(Names { branch, worktree })
}

fn expand_pattern(pattern: &str, user: &str, ticket: &str) -> Result<String> {
    // Tiny templater: only {user} and {ticket} are recognized. An unknown
    // placeholder is an error rather than silently passed through.
    let mut out = String::with_capacity(pattern.len() + ticket.len());
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = match after.find('}') {
            Some(e) => e,
            None => bail!("unterminated `{{` in pattern {pattern:?}"),
        };
        let key = &after[..end];
        match key {
            "user" => out.push_str(user),
            "ticket" => out.push_str(ticket),
            other => bail!("unknown placeholder {{{other}}} in pattern {pattern:?}"),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `@suffix` shorthand: if `from` starts with `@`, expand to the branch name
/// for this ticket with that suffix in this repo. Otherwise return `from` as
/// given (or the baseline if `from` is None).
pub fn resolve_from(
    cfg: &Config,
    repo: &Repo,
    ticket: &str,
    from: Option<&str>,
    baseline: &str,
) -> Result<String> {
    match from {
        None => Ok(baseline.to_string()),
        Some(f) if f.starts_with('@') => {
            let suffix = &f[1..];
            let suffix_opt = if suffix.is_empty() { None } else { Some(suffix) };
            Ok(resolve(cfg, repo, ticket, suffix_opt)?.branch)
        }
        Some(f) => Ok(f.to_string()),
    }
}

/// Validate a ticket id: path-safe chars + optional `ticket_regex`.
pub fn validate_ticket(cfg: &Config, ticket: &str) -> Result<(), TicketError> {
    if ticket.is_empty() {
        return Err(TicketError::Empty);
    }
    for c in ticket.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.';
        if !ok {
            return Err(TicketError::BadChar(c));
        }
    }
    if ticket.starts_with('.') {
        return Err(TicketError::BadChar('.'));
    }
    if let Some(pat) = &cfg.branch.ticket_regex {
        let re = regex::Regex::new(pat).map_err(|e| TicketError::BadRegex(e.to_string()))?;
        if !re.is_match(ticket) {
            return Err(TicketError::RegexMismatch(pat.clone()));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("ticket id is empty")]
    Empty,
    #[error("ticket id contains disallowed character {0:?}")]
    BadChar(char),
    #[error("ticket_regex {0:?} is not a valid regex: {0}")]
    BadRegex(String),
    #[error("ticket id does not match ticket_regex {0:?}")]
    RegexMismatch(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BranchCfg, Repo, StageCfg, Workspace};
    use uuid::Uuid;

    fn cfg() -> Config {
        Config {
            workspace: Workspace {
                name: "t".into(),
                id: Uuid::nil(),
            },
            branch: BranchCfg::default(),
            stage: StageCfg::default(),
            repos: vec![Repo {
                name: "api".into(),
                path: PathBuf::from("/tmp/api"),
                worktree_dir: PathBuf::from("/tmp/wt/api"),
                baseline: None,
                branch_pattern: None,
            }],
        }
    }

    #[test]
    fn pattern_expands_user_and_ticket() {
        unsafe { std::env::set_var("USER", "jm") };
        let c = cfg();
        let r = &c.repos[0];
        let names = resolve(&c, r, "FUN-1234", None).unwrap();
        assert_eq!(names.branch, "jm-FUN-1234");
        assert_eq!(names.worktree, PathBuf::from("/tmp/wt/api/FUN-1234"));
    }

    #[test]
    fn suffix_joins_branch_and_path() {
        unsafe { std::env::set_var("USER", "jm") };
        let c = cfg();
        let r = &c.repos[0];
        let names = resolve(&c, r, "FUN-1234", Some("migration")).unwrap();
        assert_eq!(names.branch, "jm-FUN-1234-migration");
        assert_eq!(names.worktree, PathBuf::from("/tmp/wt/api/FUN-1234-migration"));
    }

    #[test]
    fn from_at_suffix_resolves_to_sibling_branch() {
        unsafe { std::env::set_var("USER", "jm") };
        let c = cfg();
        let r = &c.repos[0];
        let from = resolve_from(&c, r, "FUN-1234", Some("@migration"), "main").unwrap();
        assert_eq!(from, "jm-FUN-1234-migration");
    }

    #[test]
    fn from_baseline_when_none() {
        let c = cfg();
        let r = &c.repos[0];
        let from = resolve_from(&c, r, "FUN-1234", None, "develop").unwrap();
        assert_eq!(from, "develop");
    }

    #[test]
    fn rejects_bad_ticket_chars() {
        let c = cfg();
        assert!(validate_ticket(&c, "FUN/1234").is_err());
        assert!(validate_ticket(&c, "").is_err());
        assert!(validate_ticket(&c, "FUN-1234").is_ok());
    }

    #[test]
    fn enforces_ticket_regex() {
        let mut c = cfg();
        c.branch.ticket_regex = Some("^[A-Z]+-\\d+$".to_string());
        assert!(validate_ticket(&c, "FUN-1234").is_ok());
        assert!(validate_ticket(&c, "fun-1234").is_err()); // lowercase
        assert!(validate_ticket(&c, "FUN-abc").is_err()); // non-digit
    }

    #[test]
    fn rejects_invalid_ticket_regex() {
        let mut c = cfg();
        c.branch.ticket_regex = Some("[unterminated".to_string());
        assert!(matches!(
            validate_ticket(&c, "FUN-1").unwrap_err(),
            TicketError::BadRegex(_)
        ));
    }

    #[test]
    fn at_resolves_to_default_branch_when_suffix_empty() {
        unsafe { std::env::set_var("USER", "jm") };
        let c = cfg();
        let r = &c.repos[0];
        // `@` with no suffix is the default branch.
        let from = resolve_from(&c, r, "FUN-1234", Some("@"), "main").unwrap();
        assert_eq!(from, "jm-FUN-1234");
    }

    #[test]
    fn per_repo_branch_pattern_overrides_workspace() {
        unsafe { std::env::set_var("USER", "jm") };
        let mut c = cfg();
        c.repos[0].branch_pattern = Some("feature/{ticket}".to_string());
        let names = resolve(&c, &c.repos[0], "FUN-1234", None).unwrap();
        assert_eq!(names.branch, "feature/FUN-1234");
    }
}
