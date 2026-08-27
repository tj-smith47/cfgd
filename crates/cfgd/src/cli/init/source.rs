use std::path::Path;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Printer, Role};

/// Returns true if the value is a clonable source: a git URL, a `file://` URL,
/// a repository directory named `<name>.git`, or a local directory that is a
/// git work tree.
///
/// The remote half is [`cfgd_core::modules::is_git_source`], the workspace's one
/// git-URL predicate. The three extra arms exist only for `--from`, which may be
/// pointed at a repository on the same machine: a module file source naming a
/// local checkout must stay a path, so no arm may widen the shared predicate.
///
/// `file://` is answered here rather than deferred to the shared predicate,
/// which gates it behind the process-global `CFGD_ALLOW_LOCAL_SOURCES`. That
/// gate protects COMPOSED sources — a subscription that pushes files and scripts
/// onto the machine — from naming the local filesystem. `--from` is the user
/// naming their own config repo on the command line, and a `file://` URL cannot
/// be opened as a path anyway, so classifying it by an env var no `--from`
/// caller sets would only make the answer depend on whatever else the process
/// has done.
pub(super) fn is_clonable_source(value: &str) -> bool {
    if cfgd_core::modules::is_git_source(value)
        || value.starts_with("file://")
        || value.ends_with(".git")
    {
        return true;
    }
    let path = cfgd_core::expand_tilde(Path::new(value));
    path.join(".git").exists()
}

/// Resolve a --from value to a config directory path.
/// Git sources (URLs or local repos) are cloned to the target dir.
/// Plain local paths are used directly (must contain cfgd.yaml).
pub(crate) fn resolve_from(
    from: &str,
    target: Option<&Path>,
    branch: &str,
    printer: &Printer,
) -> anyhow::Result<std::path::PathBuf> {
    let from = &*cfgd_core::resolve_repo_reference(from);
    if is_clonable_source(from) {
        let dest = target
            .map(|p| p.to_path_buf())
            .unwrap_or_else(cfgd_core::default_config_dir);
        if !dest.join("cfgd.yaml").exists() {
            std::fs::create_dir_all(&dest)?;
            clone_into(&dest, from, branch, printer)?;
        } else {
            let mut row = printer.status(
                Role::Info,
                format!("Already initialized at {}", dest.posix()),
            );
            if let Some(detail) = checkout_detail(&dest) {
                row = row.detail(detail);
            }
            drop(row);
        }
        Ok(dest)
    } else {
        let path = cfgd_core::expand_tilde(Path::new(from));
        if !path.exists() {
            anyhow::bail!("Path does not exist: {}", path.posix());
        }
        if !path.join("cfgd.yaml").exists() {
            anyhow::bail!("No cfgd.yaml found in {}", path.posix());
        }
        Ok(path)
    }
}

/// The origin URL and HEAD commit a checkout is really at.
///
/// Read off the repository rather than taken from the caller's argument: the
/// rows below report on a directory somebody else may have populated, and
/// echoing the requested URL there would state as fact the one thing those rows
/// cannot know. Either half answers `None` when it cannot be read.
fn checkout_facts(dir: &Path) -> (Option<String>, Option<String>) {
    let Ok(repo) = git2::Repository::open(dir) else {
        return (None, None);
    };
    let origin = repo
        .find_remote("origin")
        .ok()
        .and_then(|remote| remote.url().ok().map(str::to_string));
    let commit = repo
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok())
        .map(|commit| commit.id().to_string())
        .map(|id| cfgd_core::short_commit(&id).to_string());
    (origin, commit)
}

/// The detail slot of any row naming a config directory `init` did not create
/// on this run: which origin, at which revision.
///
/// `init` is the surface that MINTS the checkout every later surface reports
/// on, and it is the only one that runs before there is any state to read the
/// answer back out of. A row naming only the directory cfgd picked tells the
/// reader where the files went and nothing about which repository, or which
/// revision of it, the machine is about to reconcile from — and on the
/// already-populated arms that directory may hold a different remote entirely.
///
/// `None` for a directory that is no checkout at all (a scaffolded config), so
/// a row about one renders exactly as it always has.
pub(super) fn checkout_detail(dir: &Path) -> Option<String> {
    match checkout_facts(dir) {
        (Some(url), Some(commit)) => Some(format!("{url} at {commit}")),
        (Some(url), None) => Some(url),
        (None, Some(commit)) => Some(format!("at {commit}")),
        (None, None) => None,
    }
}

/// Clone a remote repo into the target directory.
pub(super) fn clone_into(
    target_dir: &Path,
    url: &str,
    branch: &str,
    printer: &Printer,
) -> anyhow::Result<()> {
    // Named here because this is the one row the clone path prints about it.
    if target_dir.join(".git").exists() {
        let mut row = printer.status(
            Role::Info,
            format!("Skipped clone into {}", target_dir.posix()),
        );
        if let Some(detail) = checkout_detail(target_dir) {
            row = row.detail(detail);
        }
        drop(row);
        return Ok(());
    }

    cfgd_core::sources::git_clone_with_fallback(url, target_dir, printer)
        .map_err(|e| anyhow::anyhow!("Clone failed: {}", e))?;

    let mut row = printer.status(
        Role::Ok,
        format!("Cloned {url} into {}", target_dir.posix()),
    );
    if let Some(commit) = checkout_facts(target_dir).1 {
        row = row.detail(commit);
    }
    drop(row);

    // Checkout the requested branch if HEAD isn't already on it.
    // git clone checks out the remote's default branch; if the user asked for
    // a different one we need to switch.
    let repo = git2::Repository::open(target_dir)
        .map_err(|e| anyhow::anyhow!("Failed to open cloned repo: {}", e))?;
    let current_branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(String::from))
        .unwrap_or_default();
    if current_branch != branch {
        let remote_branch = format!("origin/{}", branch);
        let obj = repo
            .revparse_single(&remote_branch)
            .map_err(|_| anyhow::anyhow!("Branch '{}' not found in remote", branch))?;
        repo.checkout_tree(&obj, None)
            .map_err(|e| anyhow::anyhow!("Failed to checkout '{}': {}", branch, e))?;
        repo.set_head(&format!("refs/heads/{}", branch))
            .map_err(|e| anyhow::anyhow!("Failed to set HEAD to '{}': {}", branch, e))?;
        printer
            .status(Role::Info, "Checked out branch")
            .qualifier(branch);
    }

    Ok(())
}
