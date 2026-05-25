//! Shared fixtures for integration tests. Every test gets its own
//! tempdir-rooted environment with isolated:
//!  - workspace dir (containing trek.toml)
//!  - per-repo origin bare repo + non-bare clone (the "preprod" checkout)
//!  - XDG_STATE_HOME pointing inside the tempdir, so trek state is sandboxed
//!  - HOME pointing at the tempdir, so the user can't accidentally point
//!    git at their real config
//!
//! Tests interact with trek by calling `Env::trek(&["start", ...])`, which
//! shells out to the binary built by `cargo test` (path comes from
//! CARGO_BIN_EXE_trek).
//!
//! All trek invocations pass `--json --non-interactive` so output is
//! parseable and locks never block.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

#[allow(dead_code)]
pub struct Env {
    pub root: TempDir,
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub xdg_state: PathBuf,
    pub xdg_config: PathBuf,
    pub repos: Vec<RepoFixture>,
    pub binary: PathBuf,
}

#[allow(dead_code)]
pub struct RepoFixture {
    pub name: String,
    /// Bare "remote" repo — origin/<branch> tracking targets this.
    pub origin: PathBuf,
    /// Non-bare checkout — what trek treats as preprod.
    pub preprod: PathBuf,
    pub worktree_dir: PathBuf,
}

#[allow(dead_code)]
pub struct TrekRun {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[allow(dead_code)]
impl TrekRun {
    /// Parse stdout as the trek JSON envelope. Panics if not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "expected JSON on stdout, got: {}\nstderr: {}\nparse error: {e}",
                self.stdout, self.stderr
            )
        })
    }

    pub fn ok(&self) -> &Self {
        assert!(
            self.status == 0,
            "expected exit 0, got {} — stderr: {}\nstdout: {}",
            self.status,
            self.stderr,
            self.stdout
        );
        self
    }

    pub fn err(&self, exit: i32, code: &str) -> &Self {
        assert_eq!(
            self.status, exit,
            "expected exit {exit}, got {} — stderr: {}\nstdout: {}",
            self.status, self.stderr, self.stdout
        );
        let v = self.json();
        assert_eq!(v["ok"], false, "envelope should have ok=false: {v}");
        assert_eq!(
            v["error"]["code"], code,
            "expected error code {code}, got {} ({})",
            v["error"]["code"], v["error"]["message"]
        );
        self
    }
}

#[allow(dead_code)]
impl Env {
    /// Build an env with N repos. Each repo gets:
    ///   - a bare origin under <root>/origin/<name>.git
    ///   - a non-bare clone (preprod) under <root>/repos/<name>
    ///   - one seed commit (README.md) on `main`
    pub fn new(repo_names: &[&str]) -> Self {
        let root = TempDir::new().expect("tempdir");
        let workspace = root.path().join("workspace");
        let home = root.path().join("home");
        let xdg_state = root.path().join("state");
        let xdg_config = root.path().join("config");
        let worktrees_root = root.path().join("worktrees");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&xdg_state).unwrap();
        std::fs::create_dir_all(&xdg_config).unwrap();
        std::fs::create_dir_all(&worktrees_root).unwrap();

        let origin_root = root.path().join("origin");
        let repos_root = root.path().join("repos");
        std::fs::create_dir_all(&origin_root).unwrap();
        std::fs::create_dir_all(&repos_root).unwrap();

        let mut repos = Vec::new();
        for name in repo_names {
            let origin = origin_root.join(format!("{name}.git"));
            let preprod = repos_root.join(name);
            let worktree_dir = worktrees_root.join(name);
            init_repo_pair(&origin, &preprod, &worktree_dir, name);
            repos.push(RepoFixture {
                name: (*name).into(),
                origin,
                preprod,
                worktree_dir,
            });
        }

        // Generate a valid UUID for the workspace.
        let id = uuid::Uuid::new_v4();
        let mut toml = String::new();
        toml.push_str(&format!(
            "[workspace]\nname = \"test\"\nid = \"{id}\"\n\n[branch]\npattern = \"u-{{ticket}}\"\nsuffix_join = \"-\"\n\n[stage]\norphan_repos = \"baseline\"\n\n"
        ));
        for r in &repos {
            toml.push_str(&format!(
                "[[repos]]\nname = \"{}\"\npath = \"{}\"\nworktree_dir = \"{}\"\nbaseline = \"main\"\n\n",
                r.name,
                r.preprod.display(),
                r.worktree_dir.display(),
            ));
        }
        std::fs::write(workspace.join("trek.toml"), toml).unwrap();

        let binary = PathBuf::from(env!("CARGO_BIN_EXE_trek"));
        Env {
            root,
            workspace,
            home,
            xdg_state,
            xdg_config,
            repos,
            binary,
        }
    }

    /// Overwrite trek.toml. Caller passes the entire body.
    pub fn write_trek_toml(&self, body: &str) {
        std::fs::write(self.workspace.join("trek.toml"), body).unwrap();
    }

    /// Path to a repo by name. Panics if not found.
    pub fn repo(&self, name: &str) -> &RepoFixture {
        self.repos
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no repo named {name}"))
    }

    /// Run `trek <args...>` with isolated env, prefixing `--json` and
    /// `--non-interactive` so they apply globally. We put them *before* any
    /// `--` separator the args might contain, since clap will otherwise
    /// route them to a trailing var-args (e.g. `trek run -- pwd --json`).
    pub fn trek(&self, args: &[&str]) -> TrekRun {
        let mut full: Vec<&str> = vec!["--json", "--non-interactive"];
        full.extend_from_slice(args);
        let mut cmd = Command::new(&self.binary);
        cmd.args(&full)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("TREK_WORKSPACE", &self.workspace)
            .env("USER", "u")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        let out = cmd.output().expect("spawn trek");
        TrekRun {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Run `trek <args...>` *without* --json (human output mode).
    pub fn trek_human(&self, args: &[&str]) -> TrekRun {
        let mut cmd = Command::new(&self.binary);
        cmd.args(args)
            .arg("--non-interactive")
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("TREK_WORKSPACE", &self.workspace)
            .env("USER", "u");
        let out = cmd.output().expect("spawn trek");
        TrekRun {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

/// Initialize a bare origin + a non-bare preprod clone with one seed commit
/// on `main`, and configure `origin` to point at the bare repo. Sets
/// origin/HEAD to main so trek's baseline-resolver can find it.
fn init_repo_pair(origin: &Path, preprod: &Path, worktree_dir: &Path, name: &str) {
    std::fs::create_dir_all(origin).unwrap();
    std::fs::create_dir_all(worktree_dir).unwrap();
    run_git(origin.parent().unwrap(), &["init", "--bare", &format!("{name}.git")]);
    // Initial commit goes in a scratch clone, then push to origin.
    let seed = origin.parent().unwrap().join(format!("{name}-seed"));
    run_git(origin.parent().unwrap(), &["clone", origin.to_str().unwrap(), seed.to_str().unwrap()]);
    // Configure committer.
    run_git(&seed, &["config", "user.email", "test@example.com"]);
    run_git(&seed, &["config", "user.name", "Test User"]);
    run_git(&seed, &["checkout", "-b", "main"]);
    std::fs::write(seed.join("README.md"), format!("# {name}\n")).unwrap();
    run_git(&seed, &["add", "README.md"]);
    run_git(&seed, &["commit", "-m", "seed"]);
    run_git(&seed, &["push", "-u", "origin", "main"]);
    // Set origin/HEAD on the bare repo.
    run_git(origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    // Now clone preprod from origin.
    run_git(preprod.parent().unwrap(), &["clone", origin.to_str().unwrap(), name]);
    run_git(preprod, &["config", "user.email", "test@example.com"]);
    run_git(preprod, &["config", "user.name", "Test User"]);
    // Make sure origin/HEAD resolves on the preprod clone.
    let _ = run_git_capture(preprod, &["remote", "set-head", "origin", "main"]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git");
    if !out.status.success() {
        panic!(
            "git {args:?} in {} failed:\n{}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git")
}

/// Helper: run an arbitrary git command in `cwd`, panic on failure.
#[allow(dead_code)]
pub fn git(cwd: &Path, args: &[&str]) {
    run_git(cwd, args)
}

/// Helper: capture git stdout (trimmed) in `cwd`.
#[allow(dead_code)]
pub fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Helper: write a file inside a worktree and add+commit it.
#[allow(dead_code)]
pub fn commit_file(cwd: &Path, name: &str, body: &str, message: &str) {
    std::fs::write(cwd.join(name), body).unwrap();
    git(cwd, &["add", name]);
    git(cwd, &["commit", "-m", message]);
}
