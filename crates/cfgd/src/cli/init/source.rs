use std::path::Path;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Printer, Role};

/// Returns true if the value is a clonable git source (URL or local git repo).
pub(super) fn is_git_source(value: &str) -> bool {
    // Remote URLs
    if value.starts_with("https://")
        || value.starts_with("http://")
        || value.starts_with("ssh://")
        || value.starts_with("git://")
        || value.starts_with("git@")
        || value.ends_with(".git")
    {
        return true;
    }
    // Local git repos
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
    if is_git_source(from) {
        let dest = target
            .map(|p| p.to_path_buf())
            .unwrap_or_else(cfgd_core::default_config_dir);
        if !dest.join("cfgd.yaml").exists() {
            std::fs::create_dir_all(&dest)?;
            clone_into(&dest, from, branch, printer)?;
        } else {
            printer.status_simple(
                Role::Info,
                format!("Already initialized at {}", dest.posix()),
            );
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

/// Clone a remote repo into the target directory.
pub(super) fn clone_into(
    target_dir: &Path,
    url: &str,
    branch: &str,
    printer: &Printer,
) -> anyhow::Result<()> {
    // If target already has .git, it's already cloned — nothing to do.
    if target_dir.join(".git").exists() {
        printer.status_simple(Role::Info, "Repository already exists, skipping clone");
        return Ok(());
    }

    cfgd_core::sources::git_clone_with_fallback(url, target_dir, printer)
        .map_err(|e| anyhow::anyhow!("Clone failed: {}", e))?;

    printer.status_simple(Role::Ok, format!("Cloned to {}", target_dir.posix()));

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
        printer.status_simple(Role::Info, format!("Checked out branch: {}", branch));
    }

    Ok(())
}
