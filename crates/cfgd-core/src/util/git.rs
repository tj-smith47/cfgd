use super::constants::GIT_NETWORK_TIMEOUT;
use super::paths::home_dir_var;
use super::process::{
    command_output_with_timeout, command_path, exit_status_reason, stderr_lossy_trimmed,
    stdout_lossy_trimmed,
};
use crate::config;

/// Resolve the `git` program both factories below spawn.
///
/// `command_available("git")` answers from `$PATH` *plus* the directories of a
/// package manager cfgd bootstrapped this run, but a bare `Command::new("git")`
/// only walks `$PATH`. Resolving through the same lookup keeps availability and
/// spawn from disagreeing — otherwise a git installed by a manager bootstrapped
/// moments ago reports as present and then fails to spawn. Falls back to the
/// bare name so the OS still performs its own lookup when resolution misses.
fn git_program() -> std::ffi::OsString {
    command_path("git")
        .map(std::path::PathBuf::into_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("git"))
}

/// Prepare a `git` CLI command with SSH hang protection.
///
/// Sets `GIT_TERMINAL_PROMPT=0` to prevent interactive prompts and, for SSH URLs,
/// sets `GIT_SSH_COMMAND` with `BatchMode=yes` and configurable `StrictHostKeyChecking`
/// to prevent hangs in non-interactive contexts (piped install scripts, daemons).
///
/// The user's git config is honored, so `url.<base>.insteadOf` rewrite rules,
/// `http.proxy`, and similar settings apply to every remote operation. Only the
/// credential-helper list is cleared (see below).
///
/// The `ssh_policy` parameter controls the `StrictHostKeyChecking` value:
/// - `None` uses the default (`accept-new`)
/// - `Some(policy)` uses the specified policy
pub fn git_cmd_safe(
    url: Option<&str>,
    ssh_policy: Option<config::SshHostKeyPolicy>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(git_program());
    // git spawns credential-helper grandchildren (osxkeychain on macOS,
    // git-credential-manager-core on Windows) that inherit the child's stderr
    // pipe and outlive the watchdog's SIGKILL of the immediate `git`, so the
    // captured stderr this function's callers report on is whatever the pipe
    // readers managed to drain before they were abandoned. `-c credential.helper=`
    // resets the accumulated helper list (system + global + local) to empty so no
    // such grandchild launches — without discarding the rest of the user's git
    // config the way nulling GIT_CONFIG_GLOBAL/GIT_CONFIG_NOSYSTEM would, which
    // also threw away `url.insteadOf` rewrites and proxy settings the remote op
    // depends on. Prompt-free auth is still guaranteed by the askpass/terminal
    // env below, so the helper is the only interactive surface left to suppress.
    cmd.arg("-c").arg("credential.helper=");
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("SSH_ASKPASS", "true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if url.is_some_and(|u| u.starts_with("git@") || u.starts_with("ssh://")) {
        let policy = ssh_policy.unwrap_or_default();
        cmd.env(
            "GIT_SSH_COMMAND",
            format!(
                "ssh -o BatchMode=yes -o StrictHostKeyChecking={}",
                policy.as_ssh_option()
            ),
        );
    }
    cmd
}

/// Build a `Command` for git suitable for LOCAL operations (config get/set,
/// tag verify, add, commit, log). Sets `GIT_TERMINAL_PROMPT=0` to prevent
/// any prompt-driven hang, but does NOT set `GIT_SSH_COMMAND` because no
/// network is involved. Use [`git_cmd_safe`] for any operation that talks to
/// a remote.
pub fn git_cmd_local() -> std::process::Command {
    let mut cmd = std::process::Command::new(git_program());
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// Refuse a revision operand that git would read as an OPTION, naming the value.
///
/// The documented guard for this is `--end-of-options`, and it is the one guard
/// `git reset` and `git checkout` did not honour on the gits still in the field:
/// both REFUSE the whole invocation when it appears (`fatal: option
/// '--end-of-options' must come before non-option arguments`; `fatal: git
/// checkout: --detach does not take a path argument '--end-of-options'`).
/// Probed: refused by 2.34.1, 2.39.5 and 2.43.0 (upstream and the
/// `1:2.43.0-1ubuntu7.3` Ubuntu 24.04 LTS ships alike), accepted from 2.43.7 on
/// — the fix landed in the 2.43.x maintenance line, and no release note names
/// it. So on the commonest Linux host every such invocation failed before it
/// ran, which cost the source cache its verify-then-publish rollback, the one
/// thing standing between a refused fetch and a discarded checkout. A revision
/// argv therefore carries a TRAILING `--` (which separates a revision from a
/// pathspec on every git) and this refusal. The refusal is narrower than the
/// option it replaces — `--end-of-options` would have PERMITTED a ref literally
/// named `-foo`, which no git will mint through `tag` or `branch` — and that is
/// the whole of the difference. `clone`, `fetch` and `ls-remote` accept the
/// option on every probed git and keep it.
pub fn refuse_option_like_revision(revision: &str) -> std::result::Result<(), String> {
    if revision.starts_with('-') {
        return Err(format!(
            "refusing to run git against revision '{revision}': a revision must not begin with '-'"
        ));
    }
    Ok(())
}

/// Try a git CLI command via [`git_cmd_safe`], returning `true` on success.
/// On failure, logs the stderr via `tracing::debug` and returns `false`.
pub fn try_git_cmd(
    url: Option<&str>,
    args: &[&str],
    label: &str,
    ssh_policy: Option<config::SshHostKeyPolicy>,
) -> bool {
    // Hold the PATH read-lock across resolution + spawn: a concurrent test
    // emptying `PATH` is a data race on `environ` that surfaces here as a git
    // child that mysteriously fails to run. Compiled out of release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _path_guard = crate::test_helpers::path_env_read_guard();

    let mut cmd = git_cmd_safe(url, ssh_policy);
    cmd.args(args);
    match command_output_with_timeout(&mut cmd, GIT_NETWORK_TIMEOUT) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            tracing::debug!(
                "git {} CLI failed ({}): {}",
                label,
                exit_status_reason(&output.status),
                stderr_lossy_trimmed(&output),
            );
            false
        }
        Err(e) => {
            tracing::debug!("git {} CLI unavailable: {e}", label);
            false
        }
    }
}

/// Env-var seam name for the cosign binary path. See [`crate::tool_binary_name`].
pub const COSIGN_BIN_ENV: &str = "CFGD_COSIGN_BIN";

/// Build a base `cosign` `Command` — the shared factory for signature / attestation
/// operations across `oci.rs`, `cli/module.rs`, and `upgrade.rs`.
///
/// Rationale: cosign is cfgd's controlled shell-out for Sigstore signature
/// verification, the same architectural category as [`git_cmd_safe`] for git.
/// Centralising the factory keeps invocation-site assumptions (stderr capture,
/// future env / timeout hardening) uniform and lets the module-boundary audit
/// point at one place instead of tracking every caller.
///
/// The binary name honors `CFGD_COSIGN_BIN` for tests via [`crate::tool_cmd`].
///
/// Callers add their own subcommand (`sign`, `verify-blob`, `verify-attestation`,
/// `attest`, etc.) and any additional flags.
pub fn cosign_cmd() -> std::process::Command {
    super::process::tool_cmd(COSIGN_BIN_ENV, "cosign")
}

/// Verify cosign is available, honoring the `CFGD_COSIGN_BIN` test seam.
/// Delegates to [`crate::require_tool_with_seam`] to share the env-var-override logic
/// with every other shimmable tool in cfgd-core.
pub fn require_cosign() -> std::result::Result<(), String> {
    super::process::require_tool_with_seam(COSIGN_BIN_ENV, "cosign", None)
}

/// Best-effort detection of a local git repo's default branch.
///
/// Tries (in order) `origin/HEAD` symbolic-ref (the remote-tracking default),
/// then the local `HEAD` symbolic-ref. Returns `None` when the directory is not
/// a git repo, both refs are missing, or the `git` binary is unavailable.
///
/// Callers should supply their own fallback (cfgd convention: `"master"`).
pub fn detect_default_branch(repo_dir: &std::path::Path) -> Option<String> {
    // Hold the PATH read-lock across resolution + spawn: a concurrent test
    // emptying `PATH` is a data race on `environ` that surfaces here as a git
    // child that mysteriously fails to run. Compiled out of release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _path_guard = crate::test_helpers::path_env_read_guard();

    let dir = repo_dir.display().to_string();

    let mut cmd = git_cmd_safe(None, None);
    cmd.args([
        "-C",
        &dir,
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .stdout(std::process::Stdio::piped());
    if let Ok(output) = cmd.output()
        && output.status.success()
    {
        let raw = stdout_lossy_trimmed(&output);
        let stripped = raw.strip_prefix("origin/").unwrap_or(&raw);
        if !stripped.is_empty() {
            return Some(stripped.to_string());
        }
    }

    let mut cmd = git_cmd_safe(None, None);
    cmd.args(["-C", &dir, "symbolic-ref", "--short", "HEAD"])
        .stdout(std::process::Stdio::piped());
    if let Ok(output) = cmd.output()
        && output.status.success()
    {
        let branch = stdout_lossy_trimmed(&output);
        if !branch.is_empty() {
            return Some(branch);
        }
    }

    None
}

/// Run a local git command in the current working directory and return its
/// trimmed stdout, or `None` if git is missing or the command exits non-zero.
fn git_output_cwd(args: &[&str]) -> Option<String> {
    // Hold the PATH read-lock across resolution + spawn: a concurrent test
    // emptying `PATH` is a data race on `environ` that surfaces here as a git
    // child that mysteriously fails to run. Compiled out of release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    let _path_guard = crate::test_helpers::path_env_read_guard();

    let output = git_cmd_local().args(args).output().ok()?;
    if output.status.success() {
        Some(stdout_lossy_trimmed(&output))
    } else {
        None
    }
}

/// Detect the `origin` remote URL of the git repository containing the current
/// working directory. Returns `None` outside a repo or when no `origin` is set.
///
/// Used to stamp provenance (source repo) into pushed artifacts.
pub fn detect_git_remote() -> Option<String> {
    git_output_cwd(&["remote", "get-url", "origin"])
}

/// Detect the `HEAD` commit SHA of the git repository containing the current
/// working directory. Returns `None` outside a repo or in a repo with no commits.
///
/// Used to stamp provenance (source commit) into pushed artifacts.
pub fn detect_git_head() -> Option<String> {
    git_output_cwd(&["rev-parse", "HEAD"])
}

/// Resolve a user-written repository reference into the value git should be
/// handed: an existing local path wins, and anything else routes through
/// [`expand_github_shorthand`].
///
/// `acme/config` is simultaneously a valid GitHub shorthand and a valid
/// relative path, and only the filesystem can say which one the user meant.
/// Every entry point that takes a repository reference from the user resolves
/// it here, so a value naming something on disk is never silently turned into a
/// network fetch of a same-named GitHub repository on one surface and left
/// alone on another.
///
/// Presence is judged with `symlink_metadata`, not `Path::exists`: a dangling
/// symlink is still an entry the user created under that name, and expanding it
/// to a GitHub URL would answer a local name with a remote repository.
///
/// Resolution is idempotent — an expanded URL is neither a path nor a
/// shorthand — so a caller may resolve a value that has already been resolved.
pub fn resolve_repo_reference(value: &str) -> std::borrow::Cow<'_, str> {
    let path = super::paths::expand_tilde(std::path::Path::new(value));
    if std::fs::symlink_metadata(&path).is_ok() {
        return std::borrow::Cow::Borrowed(value);
    }
    expand_github_shorthand(value)
}

/// Expand a GitHub `owner/repo` shorthand into a full HTTPS clone URL.
///
/// `acme/config` becomes `https://github.com/acme/config.git`. Every other
/// shape is returned untouched, so a value that already names a repository —
/// on any host, over any transport — reaches git exactly as it was written.
///
/// Pass-through classes:
/// - explicit schemes: `https://`, `http://`, `ssh://`, `git://`, `file://`
/// - SCP-style remotes (`git@github.com:acme/config.git`)
/// - dot-carrying hosts: `gitlab.com/config`, `git.example.com/acme/config`
/// - path-shaped values: `/etc/cfgd`, `./config`, `~/config`, `C:\src\config`
/// - anything that is not exactly two `/`-separated segments
///
/// The host test is a dot anywhere in the FIRST segment. GitHub logins admit
/// only ASCII alphanumerics and hyphens, so an owner can never carry a dot; a
/// dotted first segment is therefore a hostname, never an owner. That keeps a
/// self-hosted `gitlab.example.com/acme/config` pointed at its own server
/// instead of being silently redirected to github.com. Repository names may
/// contain dots (`acme/acme.github.io`), so the test is confined to the first
/// segment. A `.git` suffix on the second segment is accepted and not doubled.
///
/// A DOTLESS host is indistinguishable from an owner by grammar alone, so
/// `localhost/config` and `gitserver/config` do expand to github.com. Name such
/// a host with a scheme (`http://gitserver/config`) to reach it. This function
/// answers from the string alone; the filesystem question — "does the user
/// already have something by this name?" — belongs to
/// [`resolve_repo_reference`], which every user-facing entry point calls
/// instead of this one.
pub fn expand_github_shorthand(value: &str) -> std::borrow::Cow<'_, str> {
    match split_github_shorthand(value) {
        Some((owner, repo)) => {
            std::borrow::Cow::Owned(format!("https://github.com/{owner}/{repo}.git"))
        }
        None => std::borrow::Cow::Borrowed(value),
    }
}

/// Split a value into `(owner, repo)` when — and only when — it is a GitHub
/// shorthand. The returned repo has any `.git` suffix removed.
fn split_github_shorthand(value: &str) -> Option<(&str, &str)> {
    let (owner, repo) = value.split_once('/')?;
    // A second separator means three or more segments, which no shorthand has.
    if repo.contains('/') {
        return None;
    }
    // The owner charset excludes `.` (host-shaped), `:` (scheme, port, SCP-style
    // remote, Windows drive) and `~` (home-relative path), so every one of those
    // shapes leaves the first segment and is returned to the caller untouched.
    let owner_ok =
        !owner.is_empty() && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !owner_ok {
        return None;
    }
    let stem = repo.strip_suffix(".git").unwrap_or(repo);
    // A stem of nothing but dots (`.`, `..`) names a directory relative to the
    // owner segment, never a repository — `acme/..` is a path, not a shorthand.
    let repo_ok = !stem.is_empty()
        && stem.chars().any(|c| c != '.')
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !repo_ok {
        return None;
    }
    Some((owner, stem))
}

/// Git credential callback for git2 — handles SSH and HTTPS authentication.
/// Used by sources/, modules/, and daemon/ for all git operations.
///
/// Tries in order:
/// 1. SSH agent (for SSH URLs)
/// 2. SSH key files: `~/.ssh/id_ed25519`, `~/.ssh/id_rsa` (for SSH URLs)
/// 3. Git credential helper / GIT_ASKPASS (for HTTPS URLs)
/// 4. Default system credentials
pub fn git_ssh_credentials(
    _url: &str,
    username_from_url: Option<&str>,
    allowed_types: git2::CredentialType,
) -> std::result::Result<git2::Cred, git2::Error> {
    let username = username_from_url.unwrap_or("git");

    if allowed_types.contains(git2::CredentialType::SSH_KEY) {
        if let Ok(cred) = git2::Cred::ssh_key_from_agent(username) {
            return Ok(cred);
        }
        let home = home_dir_var().unwrap_or_default();
        for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
            let key_path = std::path::Path::new(&home).join(".ssh").join(key_name);
            if key_path.exists()
                && let Ok(cred) = git2::Cred::ssh_key(username, None, &key_path, None)
            {
                return Ok(cred);
            }
        }
    }

    if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        return git2::Cred::credential_helper(
            &git2::Config::open_default()
                .map_err(|e| git2::Error::from_str(&format!("cannot open git config: {e}")))?,
            _url,
            username_from_url,
        );
    }

    if allowed_types.contains(git2::CredentialType::DEFAULT) {
        return git2::Cred::default();
    }

    Err(git2::Error::from_str("no suitable credentials found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    /// A `git reset` / `git checkout` argv ends its revision with a trailing
    /// `--`.
    ///
    /// The two verbs did not honour `--end-of-options` on the gits still in the
    /// field: refused by 2.34.1, 2.39.5 and 2.43.0 (the git Ubuntu 24.04 LTS
    /// ships), accepted only from 2.43.7 on. On the commonest Linux host every
    /// such argv failed before it ran — which is how a refused source fetch lost
    /// its verify-then-publish rollback (`reset_checkout_to`) and every
    /// `pinVersion` lost its detached checkout. `clone`, `fetch` and `ls-remote`
    /// accept the option on every probed git and keep it.
    ///
    /// Pinned as the INVARIANT rather than the retired spelling: an argv naming
    /// either verb must carry a `--` element, so a revision written without one
    /// trips the walk whether or not it reaches for the option that failed.
    #[test]
    fn no_revision_verb_argv_spells_end_of_options() {
        let mut offenders = Vec::new();
        let mut seen = 0usize;
        for path in workspace_rust_files() {
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            // Prose says the words on purpose — the rule is documented where it
            // is enforced, and a comment spawns no process.
            let code = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for (argv, verb) in revision_verb_argvs(&code) {
                seen += 1;
                if !argv.contains("\"--\"") {
                    offenders.push(format!(
                        "{}: git {verb} argv carries no `\"--\"` element",
                        path.display()
                    ));
                }
                if argv.contains("\"--end-of-options\"") {
                    offenders.push(format!(
                        "{}: git {verb} argv names --end-of-options",
                        path.display()
                    ));
                }
            }
        }
        assert!(
            seen > 0,
            "no reset/checkout argv found: the walk stopped seeing its population"
        );
        assert!(
            offenders.is_empty(),
            "a git reset/checkout revision argv ends its revision with a trailing \
             `--`, and never names --end-of-options, which those verbs refuse on \
             git 2.43.0 and older (accepted only from 2.43.7; Ubuntu 24.04 LTS \
             ships 2.43.0). Guard an attacker-influenced revision with \
             refuse_option_like_revision instead:\n{}",
            offenders.join("\n")
        );
    }

    /// The guard refuses exactly a revision git would read as an option.
    #[test]
    fn refuse_option_like_revision_refuses_only_an_option_shaped_revision() {
        for ok in ["HEAD", "v1.2.3", "4e0c5e63", "feature/-dash", "main"] {
            assert!(
                refuse_option_like_revision(ok).is_ok(),
                "{ok} is a revision"
            );
        }
        let refused = refuse_option_like_revision("--upload-pack=touch /tmp/pwn")
            .expect_err("an option-shaped revision is refused");
        assert!(
            refused.contains("--upload-pack=touch /tmp/pwn"),
            "the refusal names the value it refused: {refused}"
        );
    }

    /// Every `reset` / `checkout` argv in `code`, with the verb it names — the
    /// bracketed list form and the `Command` builder chain alike.
    ///
    /// A verb literal that is neither is a word, not an argv — an enum arm
    /// rendering the stage name of a libgit2 pull spells `"checkout"` and runs
    /// nothing — so the chain arm is gated on `.arg(` / `.args(`, the only way
    /// a statement with no list can still hand git a revision. Both forms are
    /// bounded by their own statement, so a distant `[` cannot be read as the
    /// argv's opening and a neighbouring chain cannot lend this one its `--`.
    fn revision_verb_argvs(code: &str) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        for verb in ["\"reset\"", "\"checkout\""] {
            let mut rest = code;
            while let Some(at) = rest.find(verb) {
                let before = &rest[..at];
                let stmt_start = before.rfind([';', '{', '}']).map_or(0, |o| o + 1);
                let stmt_end = rest[at..].find(';').map_or(rest.len(), |o| at + o);
                match before[stmt_start..].rfind('[') {
                    Some(open) => {
                        let from = stmt_start + open;
                        if let Some(close) = rest[from..].find(']') {
                            out.push((&rest[from..from + close], verb));
                        }
                    }
                    None => {
                        let stmt = &rest[stmt_start..stmt_end];
                        if stmt.contains(".arg(") || stmt.contains(".args(") {
                            out.push((stmt, verb));
                        }
                    }
                }
                rest = &rest[at + verb.len()..];
            }
        }
        out
    }

    /// The walk judges a builder chain, and still leaves a bare verb WORD alone.
    ///
    /// Read for bracketed lists only, a `Command::new("git").arg("reset")…`
    /// chain was invisible: the very shape a caller reaches for when the argv
    /// is built conditionally, and the one no reviewer would think to spell as
    /// an array.
    #[test]
    fn the_argv_walk_judges_a_builder_chain_with_no_array() {
        let chain = revision_verb_argvs(
            "let mut cmd = git_cmd_local();\ncmd.arg(\"reset\").arg(\"--hard\").arg(rev);",
        );
        assert_eq!(chain.len(), 1, "the chain is one argv: {chain:?}");
        assert!(
            !chain[0].0.contains("\"--\""),
            "and it carries no trailing separator, which is what the walk fails on: {chain:?}"
        );
        assert_eq!(
            revision_verb_argvs("cmd.args([\"reset\", \"--hard\", \"--\", rev]);").len(),
            1,
            "the bracketed form still reads as one argv"
        );
        assert!(
            revision_verb_argvs("PullStage::Checkout => \"checkout\",").is_empty(),
            "a verb WORD spawns nothing and is left alone"
        );
    }

    /// Every `.rs` file under every crate's `src/`.
    fn workspace_rust_files() -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("crates"),
        ];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        assert!(!out.is_empty(), "found no sources under crates/");
        out
    }

    /// Saves and restores the `CFGD_COSIGN_BIN` env var so tests stay isolated
    /// even when one panics. Pairs with `serial_test::serial` since env-var
    /// mutation is process-global.
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn capture(key: &'static str) -> Self {
            Self {
                key,
                prior: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: serial_test::serial gates execution; no concurrent reader.
            unsafe {
                match self.prior.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn git_cmd_local_sets_terminal_prompt_zero_and_no_ssh_env() {
        let cmd = git_cmd_local();
        // `file_stem`, not `file_name`: the program is a resolved absolute path
        // whenever `git` is on PATH, and carries `.exe` on Windows.
        let prog = std::path::Path::new(cmd.get_program())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert_eq!(prog, "git", "program must resolve to `git`");

        let envs: std::collections::HashMap<&std::ffi::OsStr, Option<&std::ffi::OsStr>> =
            cmd.get_envs().collect();
        let term = envs
            .get(std::ffi::OsStr::new("GIT_TERMINAL_PROMPT"))
            .and_then(|v| v.as_deref())
            .and_then(|s| s.to_str());
        assert_eq!(
            term,
            Some("0"),
            "GIT_TERMINAL_PROMPT must be set to 0 to prevent prompt-driven hangs"
        );
        assert!(
            !envs.contains_key(std::ffi::OsStr::new("GIT_SSH_COMMAND")),
            "git_cmd_local is for local-only ops and must not configure GIT_SSH_COMMAND"
        );
    }

    #[test]
    fn both_factories_spawn_the_program_command_path_resolves() {
        // `command_available("git")` answers from `$PATH` plus the directories of
        // a manager cfgd bootstrapped this run, but a bare `Command::new("git")`
        // walks only `$PATH` — so a git installed by a just-bootstrapped manager
        // reports present and then fails to spawn. Both factories must therefore
        // carry whatever `command_path` resolved, never the bare name.
        //
        // Deliberately reads the ambient `$PATH` rather than emptying it: `PATH`
        // is process-global, and the git-spawning tests in this workspace are not
        // serialized, so an empty-`PATH` window here breaks them under
        // `cargo test`'s thread-per-test model (it is invisible under nextest's
        // process-per-test model). The registry-fallback half of `command_path`
        // is pinned by `util::process::tests` against a tool name nothing else
        // spawns.
        //
        // One read guard brackets every resolution below, so the baseline and
        // the two factory resolutions all observe the same `PATH`; without it a
        // writer landing between them flips the answer mid-comparison. Safe to
        // hold: `command_path` and the factories read `PATH` directly and never
        // re-take this lock, and nothing here spawns.
        let _path = crate::test_helpers::path_env_read_guard();

        let Some(resolved) = command_path("git") else {
            // No git on this host: the factories must still hand the OS the bare
            // name so it can perform its own lookup.
            for cmd in [git_cmd_local(), git_cmd_safe(None, None)] {
                assert_eq!(
                    cmd.get_program(),
                    std::ffi::OsStr::new("git"),
                    "with no resolution the factories must fall back to the bare name"
                );
            }
            return;
        };
        for cmd in [git_cmd_local(), git_cmd_safe(None, None)] {
            assert_eq!(
                std::path::Path::new(cmd.get_program()),
                resolved.as_path(),
                "factory must spawn the git that command_path resolved, not the bare name"
            );
        }
    }

    #[test]
    fn git_cmd_safe_clears_credential_helper_but_keeps_user_config() {
        let cmd = git_cmd_safe(None, None);

        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The accumulated credential-helper list must be reset to empty so no
        // osxkeychain / GCM grandchild can launch and outlive the watchdog.
        let helper_pos = args.iter().position(|a| a == "credential.helper=");
        assert!(
            helper_pos.is_some_and(|p| p > 0 && args[p - 1] == "-c"),
            "git_cmd_safe must pass `-c credential.helper=`; got args {args:?}"
        );

        // It must NOT discard the user's git config — honoring url.insteadOf /
        // proxy settings is the whole point of the surgical credential-only reset.
        let envs: std::collections::HashMap<&std::ffi::OsStr, Option<&std::ffi::OsStr>> =
            cmd.get_envs().collect();
        assert!(
            !envs.contains_key(std::ffi::OsStr::new("GIT_CONFIG_GLOBAL")),
            "git_cmd_safe must not null GIT_CONFIG_GLOBAL (would drop url.insteadOf)"
        );
        assert!(
            !envs.contains_key(std::ffi::OsStr::new("GIT_CONFIG_NOSYSTEM")),
            "git_cmd_safe must not set GIT_CONFIG_NOSYSTEM (would drop system config)"
        );
        // Prompt-free auth must still be guaranteed.
        for key in ["GIT_TERMINAL_PROMPT", "GIT_ASKPASS", "SSH_ASKPASS"] {
            assert!(
                envs.contains_key(std::ffi::OsStr::new(key)),
                "git_cmd_safe must still set {key} to stay non-interactive"
            );
        }
    }

    #[test]
    #[serial]
    fn require_cosign_with_env_var_pointing_to_real_file_succeeds() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bin = tmp.path().join("anything");
        fs::write(&bin, "").expect("write");

        let _guard = EnvVarGuard::capture("CFGD_COSIGN_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", &bin);
        }
        require_cosign().expect("env-var pointing to existing file → Ok");
    }

    #[test]
    #[serial]
    fn require_cosign_with_env_var_pointing_to_missing_file_errors_out() {
        let _guard = EnvVarGuard::capture("CFGD_COSIGN_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", "/no/such/file/at/all");
        }
        let err = require_cosign().expect_err("missing file → Err");
        assert!(
            err.contains("CFGD_COSIGN_BIN") && err.contains("not a file"),
            "error must call out env-var + missing-file: {err}"
        );
    }

    #[test]
    fn detect_default_branch_resolves_origin_head_on_detached_checkout() {
        // Reproduces the CI checkout shape (actions/checkout = detached HEAD)
        // and proves origin/HEAD still resolves the remote default branch. The
        // local-HEAD fallback is covered by detect_default_branch_on_fresh_init_repo.
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("upstream");
        let work = tmp.path().join("work");
        // Anchor every git child to this test's own tempdir. Without an
        // explicit cwd the child inherits the process cwd, which concurrent
        // #[serial] tests (CwdGuard users) point at short-lived tempdirs;
        // a `git clone` spawned in that window dies with "this operation
        // must be run in a work tree" once the inherited cwd is deleted.
        let anchor = tmp.path().to_path_buf();
        let git = move |args: &[&str]| {
            // `git` resolves through `PATH` at spawn time, so this child must not
            // overlap a concurrent test's empty-`PATH` window.
            let _spawn = crate::test_helpers::path_env_read_guard();
            let ok = super::git_cmd_local()
                .current_dir(&anchor)
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} must succeed");
        };
        let up = upstream.to_str().unwrap();
        let wk = work.to_str().unwrap();
        git(&["init", "-b", "trunk", up]);
        git(&[
            "-C",
            up,
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]);
        git(&["clone", up, wk]);
        git(&["-C", wk, "checkout", "--detach", "HEAD", "--"]);
        assert_eq!(
            detect_default_branch(&work).as_deref(),
            Some("trunk"),
            "origin/HEAD must resolve the remote default branch on a detached checkout"
        );
    }

    #[test]
    fn detect_default_branch_returns_none_for_non_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = detect_default_branch(tmp.path());
        assert!(result.is_none(), "non-git directory must return None");
    }

    #[test]
    fn detect_default_branch_on_fresh_init_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let result = detect_default_branch(tmp.path());
        assert!(result.is_some());
    }

    #[test]
    fn try_git_cmd_succeeds_on_version() {
        let ok = try_git_cmd(None, &["--version"], "version-check", None);
        assert!(ok, "git --version should succeed");
    }

    #[test]
    fn try_git_cmd_fails_on_invalid_subcommand() {
        let ok = try_git_cmd(None, &["not-a-real-subcommand-xyz"], "invalid-cmd", None);
        assert!(!ok, "invalid git subcommand should return false");
    }

    mod cwd_provenance {
        use serial_test::serial;

        use super::{detect_git_head, detect_git_remote};
        use crate::test_helpers::CwdGuard;

        fn git(dir: &std::path::Path, args: &[&str]) {
            let _spawn = crate::test_helpers::path_env_read_guard();
            let status = super::super::git_cmd_local()
                .args(args)
                .current_dir(dir)
                .status()
                .expect("git command");
            assert!(status.success(), "git {args:?} failed");
        }

        #[test]
        #[serial]
        fn detect_git_remote_returns_url_when_origin_configured() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            git(
                dir.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.test/owner/repo.git",
                ],
            );
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            assert_eq!(
                detect_git_remote().as_deref(),
                Some("https://example.test/owner/repo.git"),
                "should echo configured remote URL"
            );
        }

        #[test]
        #[serial]
        fn detect_git_remote_returns_none_in_fresh_repo() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            assert!(
                detect_git_remote().is_none(),
                "fresh repo with no remote must return None"
            );
        }

        #[test]
        #[serial]
        fn detect_git_head_returns_sha_after_initial_commit() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            git(dir.path(), &["config", "user.email", "test@example.com"]);
            git(dir.path(), &["config", "user.name", "Test"]);
            std::fs::write(dir.path().join("f.txt"), b"hello").expect("write file");
            git(dir.path(), &["add", "."]);
            git(dir.path(), &["commit", "-m", "init"]);

            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            let sha = detect_git_head().expect("HEAD must be Some after initial commit");
            assert_eq!(sha.len(), 40, "HEAD SHA must be 40 hex chars: {sha}");
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit()),
                "HEAD SHA must be hex: {sha}"
            );
        }

        #[test]
        #[serial]
        fn detect_git_head_returns_none_in_empty_repo() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            assert!(
                detect_git_head().is_none(),
                "empty repo has no HEAD, must return None"
            );
        }
    }

    mod github_shorthand {
        use super::*;

        #[test]
        fn expands_owner_repo() {
            assert_eq!(
                expand_github_shorthand("acme/config"),
                "https://github.com/acme/config.git"
            );
        }

        #[test]
        fn expands_owner_repo_with_hyphens_and_digits() {
            assert_eq!(
                expand_github_shorthand("acme-corp7/dev-config2"),
                "https://github.com/acme-corp7/dev-config2.git"
            );
        }

        #[test]
        fn expands_repo_carrying_dots_without_doubling_git_suffix() {
            assert_eq!(
                expand_github_shorthand("acme/acme.github.io"),
                "https://github.com/acme/acme.github.io.git"
            );
            assert_eq!(
                expand_github_shorthand("acme/config.git"),
                "https://github.com/acme/config.git"
            );
            assert_eq!(
                expand_github_shorthand("acme/my_config"),
                "https://github.com/acme/my_config.git"
            );
        }

        #[test]
        fn passes_through_explicit_schemes() {
            for value in [
                "https://github.com/acme/config",
                "https://github.com/acme/config.git",
                "http://internal.host/acme/config",
                "ssh://git@github.com/acme/config.git",
                "git://github.com/acme/config",
                "file:///srv/git/config.git",
            ] {
                assert_eq!(expand_github_shorthand(value), value, "scheme: {value}");
            }
        }

        #[test]
        fn passes_through_scp_style_remotes() {
            for value in [
                "git@github.com:acme/config.git",
                "git@gitlab.example.com:acme/config.git",
                "deploy@10.0.0.5:srv/config.git",
            ] {
                assert_eq!(expand_github_shorthand(value), value, "scp: {value}");
            }
        }

        #[test]
        fn passes_through_host_shaped_values() {
            for value in [
                "gitlab.com/acme/config",
                "gitlab.com/config",
                "git.example.com/acme/config.git",
                "codeberg.org/acme/config",
                "localhost:3000/acme/config",
            ] {
                assert_eq!(expand_github_shorthand(value), value, "host: {value}");
            }
        }

        #[test]
        fn passes_through_path_shaped_values() {
            for value in [
                "/srv/config",
                "/srv/config.git",
                "./config",
                "../config",
                "~/config",
                "~/dev/config",
                r"C:\src\config",
                r"C:/src/config",
                r".\config",
                "config",
                "",
            ] {
                assert_eq!(expand_github_shorthand(value), value, "path: {value}");
            }
        }

        #[test]
        fn passes_through_values_that_are_not_two_segments() {
            for value in [
                "acme/config/extra",
                "acme//config",
                "acme/",
                "/config",
                "acme/config//modules/tmux",
            ] {
                assert_eq!(expand_github_shorthand(value), value, "segments: {value}");
            }
        }

        #[test]
        fn passes_through_dot_only_repo_stems() {
            for value in ["acme/..", "acme/.", "acme/...", "acme/..git"] {
                assert_eq!(expand_github_shorthand(value), value, "stem: {value}");
            }
        }

        #[test]
        fn passes_through_ref_and_query_suffixes() {
            for value in [
                "acme/config@v1.2.0",
                "acme/config?ref=dev",
                "acme/config#main",
                "acme/con fig",
            ] {
                assert_eq!(expand_github_shorthand(value), value, "suffix: {value}");
            }
        }

        #[test]
        fn expansion_is_idempotent() {
            let once = expand_github_shorthand("acme/config").into_owned();
            assert_eq!(expand_github_shorthand(&once), once);
        }

        #[test]
        fn pass_through_does_not_allocate() {
            assert!(matches!(
                expand_github_shorthand("https://github.com/acme/config.git"),
                std::borrow::Cow::Borrowed(_)
            ));
        }
    }

    mod repo_reference {
        use super::*;
        use crate::test_helpers::CwdGuard;

        #[test]
        #[serial]
        fn expands_a_shorthand_that_names_nothing_on_disk() {
            let dir = tempfile::tempdir().expect("tempdir");
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            assert_eq!(
                resolve_repo_reference("acme/config"),
                "https://github.com/acme/config.git"
            );
        }

        #[test]
        #[serial]
        fn existing_relative_path_wins_over_shorthand() {
            let dir = tempfile::tempdir().expect("tempdir");
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            fs::create_dir_all(dir.path().join("acme").join("config")).expect("create nested dir");
            assert_eq!(
                resolve_repo_reference("acme/config"),
                "acme/config",
                "a relative path that exists must never become a GitHub URL"
            );
        }

        #[test]
        fn existing_absolute_path_wins_over_shorthand() {
            let dir = tempfile::tempdir().expect("tempdir");
            let nested = dir.path().join("acme").join("config");
            fs::create_dir_all(&nested).expect("create nested dir");
            let as_written = nested.to_string_lossy().into_owned();
            assert_eq!(resolve_repo_reference(&as_written), as_written);
        }

        #[cfg(unix)]
        #[test]
        #[serial]
        fn dangling_symlink_wins_over_shorthand() {
            let dir = tempfile::tempdir().expect("tempdir");
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            fs::create_dir_all(dir.path().join("acme")).expect("create owner dir");
            std::os::unix::fs::symlink("nowhere", dir.path().join("acme").join("config"))
                .expect("plant dangling symlink");
            assert_eq!(
                resolve_repo_reference("acme/config"),
                "acme/config",
                "a broken link is still an entry the user named; expanding it answers a \
                 local name with a remote repository"
            );
        }

        #[test]
        fn passes_through_full_urls() {
            for value in [
                "https://gitlab.example.com/acme/config.git",
                "git@github.com:acme/config.git",
                "gitlab.com/acme/config",
            ] {
                assert_eq!(resolve_repo_reference(value), value, "value: {value}");
            }
        }

        #[test]
        #[serial]
        fn resolution_is_idempotent() {
            let dir = tempfile::tempdir().expect("tempdir");
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            let once = resolve_repo_reference("acme/config").into_owned();
            assert_eq!(resolve_repo_reference(&once), once);
        }

        #[test]
        fn pass_through_does_not_allocate() {
            assert!(matches!(
                resolve_repo_reference("https://github.com/acme/config.git"),
                std::borrow::Cow::Borrowed(_)
            ));
        }
    }
}
