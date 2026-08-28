use super::*;

/// A fast-forward a pull performed: the commit the local branch left, and the
/// one it landed on.
///
/// Reported rather than collapsed to a bool because a sync that says only "new
/// changes" leaves the reader with no way to tell WHICH changes arrived, and
/// both ids are already in hand at the moment the ref moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefMovement {
    pub from: String,
    pub to: String,
}

pub(crate) fn git_pull(repo_path: &Path) -> std::result::Result<Option<RefMovement>, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {}", e))?;

    let head = repo.head().map_err(|e| format!("get HEAD: {}", e))?;
    let old_commit = head.target().map(|oid| oid.to_string());
    let branch_name = head
        .shorthand()
        .ok()
        .ok_or_else(|| "cannot determine branch name".to_string())?;

    // Try git CLI first with SSH hang protection.
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(String::from));
    let repo_dir = &repo_path.display().to_string();
    let cli_ok = crate::try_git_cmd(
        remote_url.as_deref(),
        &["-C", repo_dir, "fetch", "origin", branch_name],
        "fetch",
        None,
    );

    if !cli_ok {
        // Fall back to libgit2
        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| format!("find remote: {}", e))?;
        let mut fetch_opts = git2::FetchOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        fetch_opts.remote_callbacks(callbacks);
        remote
            .fetch(&[branch_name], Some(&mut fetch_opts), None)
            .map_err(|e| format!("fetch: {}", e))?;
    }

    // Check if we need to fast-forward
    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .map_err(|e| format!("find FETCH_HEAD: {}", e))?;
    let fetch_commit = repo
        .reference_to_annotated_commit(&fetch_head)
        .map_err(|e| format!("resolve FETCH_HEAD: {}", e))?;

    let (analysis, _) = repo
        .merge_analysis(&[&fetch_commit])
        .map_err(|e| format!("merge analysis: {}", e))?;

    if analysis.is_up_to_date() {
        return Ok(None);
    }

    if analysis.is_fast_forward() {
        let refname = format!("refs/heads/{}", branch_name);
        let mut reference = repo
            .find_reference(&refname)
            .map_err(|e| format!("find ref: {}", e))?;
        reference
            .set_target(fetch_commit.id(), "cfgd: fast-forward pull")
            .map_err(|e| format!("set target: {}", e))?;
        repo.set_head(&refname)
            .map_err(|e| format!("set HEAD: {}", e))?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .map_err(|e| format!("checkout: {}", e))?;
        return Ok(Some(RefMovement {
            // A branch with no target is unborn, which cannot fast-forward, so
            // the fallback is unreachable rather than a stand-in for a real id.
            from: old_commit.unwrap_or_default(),
            to: fetch_commit.id().to_string(),
        }));
    }

    Err("cannot fast-forward — remote has diverged".to_string())
}

pub(crate) fn git_auto_commit_push(repo_path: &Path) -> std::result::Result<bool, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| format!("open repo: {}", e))?;

    // Check for changes
    let mut index = repo.index().map_err(|e| format!("get index: {}", e))?;
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| format!("stage changes: {}", e))?;
    index.write().map_err(|e| format!("write index: {}", e))?;

    let diff = repo
        .diff_index_to_workdir(Some(&index), None)
        .map_err(|e| format!("diff: {}", e))?;

    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());

    let staged_diff = if let Some(ref tree) = head_tree {
        repo.diff_tree_to_index(Some(tree), Some(&index), None)
            .map_err(|e| format!("staged diff: {}", e))?
    } else {
        // No HEAD yet, everything in index is new
        repo.diff_tree_to_index(None, Some(&index), None)
            .map_err(|e| format!("staged diff: {}", e))?
    };

    if diff.stats().map(|s| s.files_changed()).unwrap_or(0) == 0
        && staged_diff.stats().map(|s| s.files_changed()).unwrap_or(0) == 0
    {
        return Ok(false);
    }

    // Create commit
    let tree_oid = index
        .write_tree()
        .map_err(|e| format!("write tree: {}", e))?;
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| format!("find tree: {}", e))?;

    let signature = repo
        .signature()
        .map_err(|e| format!("get signature: {}", e))?;

    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());

    let parents: Vec<&git2::Commit> = parent.as_ref().map(|p| vec![p]).unwrap_or_default();

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "cfgd: auto-commit configuration changes",
        &tree,
        &parents,
    )
    .map_err(|e| format!("commit: {}", e))?;

    // Push — try git CLI first with SSH hang protection.
    let head = repo.head().map_err(|e| format!("get HEAD: {}", e))?;
    let branch_name = head
        .shorthand()
        .ok()
        .ok_or_else(|| "cannot determine branch name".to_string())?;

    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(String::from));

    let repo_dir = &repo_path.display().to_string();
    let cli_ok = crate::try_git_cmd(
        remote_url.as_deref(),
        &["-C", repo_dir, "push", "origin", branch_name],
        "push",
        None,
    );

    if !cli_ok {
        // Fall back to libgit2.
        let mut remote = repo
            .find_remote("origin")
            .map_err(|e| format!("find remote: {}", e))?;

        let mut push_opts = git2::PushOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        push_opts.remote_callbacks(callbacks);

        let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
        remote
            .push(&[&refspec], Some(&mut push_opts))
            .map_err(|e| format!("push: {}", e))?;
    }

    Ok(true)
}
// --- Public sync functions for CLI commands ---

/// What a pull over a config directory came to.
///
/// The ONE verdict `cfgd pull` and `cfgd sync`'s local-repo leg both render
/// and exit from. Two commands running one operation answered differently
/// while each classified `git_pull`'s bare `Err` for itself: a directory under
/// no version control read `⚠ Pull failed` on one and nothing at all on the
/// other, and a real failure exited 1 on one and 0 on the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    /// The directory is under no version control, so there is nothing to pull.
    /// Not a failure: `git_pull` answers a bare `Err` here, which read as one.
    NotARepository,
    UpToDate,
    Moved(RefMovement),
    /// The pull was attempted and refused. Carries the message `git_pull`
    /// composed, libgit2 tail and all; `pull_failure_summary` is the display
    /// fold and `-o json` keeps this string.
    Failed(String),
}

/// Whether a pull over this directory can do anything at all.
///
/// Asked BEFORE the pull by a caller that opens a live region for it, so a
/// directory under no version control opens no section to animate; the pull
/// itself asks again, so a caller that does not probe still gets the right
/// verdict.
///
/// A gitlink FILE (a worktree or a submodule) is a repository too, so this
/// asks whether the entry EXISTS rather than whether it is a directory.
pub fn is_git_repository(repo_path: &Path) -> bool {
    repo_path.join(".git").exists()
}

pub fn git_pull_sync(repo_path: &Path) -> PullOutcome {
    if !is_git_repository(repo_path) {
        return PullOutcome::NotARepository;
    }
    match git_pull(repo_path) {
        Ok(Some(movement)) => PullOutcome::Moved(movement),
        Ok(None) => PullOutcome::UpToDate,
        Err(e) => PullOutcome::Failed(e),
    }
}

/// A pull failure worded for a person: libgit2's `; class=…; code=…` tail
/// belongs in a bug report, not in a result line under a red glyph.
///
/// The stored message and the `-o json` payload keep the full string, so
/// nothing diagnostic is lost — this is the DISPLAY fold alone.
pub fn pull_failure_summary(message: &str) -> &str {
    message
        .split_once("; class=")
        .map_or(message, |(head, _)| head)
}
