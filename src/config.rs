use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const DEFAULT_BRANCH_PATTERN: &str = "{user}-{ticket}";
const DEFAULT_SUFFIX_JOIN: &str = "-";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub workspace: Workspace,
    #[serde(default)]
    pub branch: BranchCfg,
    #[serde(default)]
    pub stage: StageCfg,
    #[serde(default, rename = "repos")]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub hooks: Hooks,
}

/// Optional shell commands to run after specific trek operations. Each value
/// is a shell command line; trek runs it via `sh -c` with these env vars
/// available: TREK_COMMAND, TREK_TICKET, TREK_EXIT_CODE, TREK_WORKSPACE,
/// TREK_WORKSPACE_ID. Hook failures are reported as warnings but do not
/// change the command's exit code.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    #[serde(default)]
    pub post_start: Option<String>,
    #[serde(default)]
    pub post_adopt: Option<String>,
    #[serde(default)]
    pub post_stage: Option<String>,
    #[serde(default)]
    pub post_unstage: Option<String>,
    #[serde(default)]
    pub post_cleanup: Option<String>,
    #[serde(default)]
    pub post_refresh: Option<String>,
    #[serde(default)]
    pub post_sync: Option<String>,
    #[serde(default)]
    pub post_push: Option<String>,
}

impl Hooks {
    pub fn for_command(&self, cmd: &str) -> Option<&str> {
        match cmd {
            "start" => self.post_start.as_deref(),
            "adopt" => self.post_adopt.as_deref(),
            "stage" => self.post_stage.as_deref(),
            "unstage" => self.post_unstage.as_deref(),
            "cleanup" => self.post_cleanup.as_deref(),
            "refresh" => self.post_refresh.as_deref(),
            "sync" => self.post_sync.as_deref(),
            "push" => self.post_push.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchCfg {
    #[serde(default = "default_branch_pattern")]
    pub pattern: String,
    #[serde(default = "default_suffix_join")]
    pub suffix_join: String,
    #[serde(default)]
    pub ticket_regex: Option<String>,
}

impl Default for BranchCfg {
    fn default() -> Self {
        Self {
            pattern: DEFAULT_BRANCH_PATTERN.to_string(),
            suffix_join: DEFAULT_SUFFIX_JOIN.to_string(),
            ticket_regex: None,
        }
    }
}

fn default_branch_pattern() -> String {
    DEFAULT_BRANCH_PATTERN.to_string()
}
fn default_suffix_join() -> String {
    DEFAULT_SUFFIX_JOIN.to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrphanRepos {
    Baseline,
    Leave,
    Fail,
}

impl Default for OrphanRepos {
    fn default() -> Self {
        Self::Baseline
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageCfg {
    #[serde(default)]
    pub orphan_repos: OrphanRepos,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub worktree_dir: PathBuf,
    #[serde(default)]
    pub baseline: Option<String>,
    #[serde(default)]
    pub branch_pattern: Option<String>,
}

impl Config {
    pub fn load(workspace_toml: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(workspace_toml)
            .with_context(|| format!("read {}", workspace_toml.display()))?;
        let mut cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parse {}", workspace_toml.display()))?;
        cfg.apply_xdg_defaults()?;
        cfg.expand_paths()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Overlay `$XDG_CONFIG_HOME/trek/config.toml` defaults for keys that
    /// `trek.toml` did not set explicitly. The TOML file may contain
    /// `[branch]` and/or `[stage]` tables.
    fn apply_xdg_defaults(&mut self) -> Result<()> {
        let Some(path) = xdg_config_path() else {
            return Ok(());
        };
        if !path.is_file() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;

        #[derive(Deserialize, Default)]
        struct XdgDefaults {
            #[serde(default)]
            branch: Option<BranchPartial>,
            #[serde(default)]
            stage: Option<StagePartial>,
        }
        #[derive(Deserialize, Default)]
        struct BranchPartial {
            pattern: Option<String>,
            suffix_join: Option<String>,
            ticket_regex: Option<String>,
        }
        #[derive(Deserialize, Default)]
        struct StagePartial {
            orphan_repos: Option<OrphanRepos>,
        }

        let defaults: XdgDefaults = toml::from_str(&raw)
            .with_context(|| format!("parse {}", path.display()))?;

        if let Some(b) = defaults.branch {
            // Only fill values that match the built-in defaults (i.e. weren't
            // explicitly set in trek.toml). serde gives us no easy "was set"
            // signal, so we approximate by comparing to defaults.
            if self.branch.pattern == DEFAULT_BRANCH_PATTERN {
                if let Some(v) = b.pattern {
                    self.branch.pattern = v;
                }
            }
            if self.branch.suffix_join == DEFAULT_SUFFIX_JOIN {
                if let Some(v) = b.suffix_join {
                    self.branch.suffix_join = v;
                }
            }
            if self.branch.ticket_regex.is_none() {
                self.branch.ticket_regex = b.ticket_regex;
            }
        }
        if let Some(s) = defaults.stage {
            if self.stage.orphan_repos == OrphanRepos::default() {
                if let Some(v) = s.orphan_repos {
                    self.stage.orphan_repos = v;
                }
            }
        }
        Ok(())
    }

    fn expand_paths(&mut self) -> Result<()> {
        for r in &mut self.repos {
            r.path = expand_tilde(&r.path)?;
            r.worktree_dir = expand_tilde(&r.worktree_dir)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for r in &self.repos {
            if !seen.insert(&r.name) {
                bail!("duplicate repo name `{}` in trek.toml", r.name);
            }
        }
        if let Some(re) = &self.branch.ticket_regex {
            // We don't want a `regex` dep just for validation; defer to
            // command-time. Cheap sanity check: not empty.
            if re.is_empty() {
                bail!("ticket_regex must be a non-empty string");
            }
        }
        Ok(())
    }

    pub fn repo(&self, name: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.name == name)
    }

    pub fn branch_pattern_for<'a>(&'a self, repo: &'a Repo) -> &'a str {
        repo.branch_pattern
            .as_deref()
            .unwrap_or(&self.branch.pattern)
    }
}

fn xdg_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(p).join("trek").join("config.toml"));
    }
    dirs::config_dir().map(|p| p.join("trek").join("config.toml"))
}

pub fn expand_tilde(p: &Path) -> Result<PathBuf> {
    let s = p
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 path: {}", p.display()))?;
    if let Some(rest) = s.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve $HOME"))?;
        Ok(home.join(rest))
    } else if s == "~" {
        dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve $HOME"))
    } else {
        Ok(p.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    /// Tests in this module mutate XDG_CONFIG_HOME, which is process-global.
    /// Serialize them so they don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_toml(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn defaults_apply_when_branch_and_stage_omitted() {
        let _lk = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let toml = write_toml(
            dir.path(),
            "trek.toml",
            r#"
                [workspace]
                name = "t"
                id = "00000000-0000-0000-0000-000000000001"
            "#,
        );
        // Disable XDG overlay so the host env can't leak into the test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let cfg = Config::load(&toml).unwrap();
        assert_eq!(cfg.branch.pattern, "{user}-{ticket}");
        assert_eq!(cfg.branch.suffix_join, "-");
        assert!(cfg.branch.ticket_regex.is_none());
        assert_eq!(cfg.stage.orphan_repos, OrphanRepos::Baseline);
        assert!(cfg.repos.is_empty());
        assert!(cfg.hooks.post_start.is_none());
    }

    #[test]
    fn rejects_duplicate_repo_names() {
        let _lk = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let toml = write_toml(
            dir.path(),
            "trek.toml",
            r#"
                [workspace]
                name = "t"
                id = "00000000-0000-0000-0000-000000000001"
                [[repos]]
                name = "api"
                path = "/tmp/a"
                worktree_dir = "/tmp/wta"
                [[repos]]
                name = "api"
                path = "/tmp/b"
                worktree_dir = "/tmp/wtb"
            "#,
        );
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let err = Config::load(&toml).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("duplicate"));
    }

    #[test]
    fn xdg_overlay_fills_only_unset_fields() {
        let _lk = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("trek");
        std::fs::create_dir_all(&xdg).unwrap();
        write_toml(
            &xdg,
            "config.toml",
            r#"
                [branch]
                pattern = "xdg-{ticket}"
                suffix_join = "_"
                ticket_regex = "^FOO-\\d+$"
                [stage]
                orphan_repos = "leave"
            "#,
        );
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        // trek.toml leaves branch.pattern at default → XDG should fill it.
        let toml = write_toml(
            dir.path(),
            "trek.toml",
            r#"
                [workspace]
                name = "t"
                id = "00000000-0000-0000-0000-000000000001"
            "#,
        );
        let cfg = Config::load(&toml).unwrap();
        assert_eq!(cfg.branch.pattern, "xdg-{ticket}");
        assert_eq!(cfg.branch.suffix_join, "_");
        assert_eq!(cfg.branch.ticket_regex.as_deref(), Some("^FOO-\\d+$"));
        assert_eq!(cfg.stage.orphan_repos, OrphanRepos::Leave);
    }

    #[test]
    fn xdg_overlay_does_not_override_explicit_value() {
        let _lk = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("trek");
        std::fs::create_dir_all(&xdg).unwrap();
        write_toml(
            &xdg,
            "config.toml",
            r#"
                [branch]
                pattern = "xdg-{ticket}"
            "#,
        );
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        // trek.toml explicitly sets pattern → XDG should NOT overwrite.
        let toml = write_toml(
            dir.path(),
            "trek.toml",
            r#"
                [workspace]
                name = "t"
                id = "00000000-0000-0000-0000-000000000001"
                [branch]
                pattern = "explicit-{ticket}"
            "#,
        );
        let cfg = Config::load(&toml).unwrap();
        assert_eq!(cfg.branch.pattern, "explicit-{ticket}");
    }

    #[test]
    fn tilde_expansion() {
        let home = dirs::home_dir().unwrap();
        let p = expand_tilde(Path::new("~/foo")).unwrap();
        assert_eq!(p, home.join("foo"));
        let p = expand_tilde(Path::new("~")).unwrap();
        assert_eq!(p, home);
        let p = expand_tilde(Path::new("/abs")).unwrap();
        assert_eq!(p, PathBuf::from("/abs"));
        let p = expand_tilde(Path::new("rel")).unwrap();
        assert_eq!(p, PathBuf::from("rel"));
    }

    #[test]
    fn repo_lookup_and_pattern_override() {
        let _lk = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let toml = write_toml(
            dir.path(),
            "trek.toml",
            r#"
                [workspace]
                name = "t"
                id = "00000000-0000-0000-0000-000000000001"
                [[repos]]
                name = "api"
                path = "/tmp/api"
                worktree_dir = "/tmp/wt/api"
                [[repos]]
                name = "web"
                path = "/tmp/web"
                worktree_dir = "/tmp/wt/web"
                branch_pattern = "feature/{ticket}"
            "#,
        );
        let cfg = Config::load(&toml).unwrap();
        assert_eq!(cfg.repo("api").unwrap().name, "api");
        assert!(cfg.repo("missing").is_none());
        assert_eq!(cfg.branch_pattern_for(cfg.repo("api").unwrap()), "{user}-{ticket}");
        assert_eq!(cfg.branch_pattern_for(cfg.repo("web").unwrap()), "feature/{ticket}");
    }

    #[test]
    fn hooks_for_command_mapping() {
        let mut h = Hooks::default();
        h.post_start = Some("a".into());
        h.post_stage = Some("b".into());
        assert_eq!(h.for_command("start"), Some("a"));
        assert_eq!(h.for_command("stage"), Some("b"));
        assert_eq!(h.for_command("unstage"), None);
        assert_eq!(h.for_command("nonexistent"), None);
    }
}
