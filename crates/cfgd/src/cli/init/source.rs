use std::path::{Path, PathBuf};

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{HintCommands, Printer, Role};

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

/// The directory a `--from` run materialises into, read off the `--config` the
/// caller gave: `None` when that is the default config directory, which
/// [`resolve_from`] resolves for itself and then guards through
/// [`refuse_occupied_default_destination`].
///
/// The path is absolutized first, so a relative `--config cfgd.yaml` names the
/// working directory rather than an empty parent. Whether the file already
/// exists is deliberately not part of the answer: reading an existing
/// `--config` as "no destination given" is what sent a run pointed at a
/// scratch directory into the invoking user's own config directory instead.
pub(crate) fn from_destination(config: &Path) -> Option<PathBuf> {
    let config = cfgd_core::absolutize_path(config);
    let dir = config.parent()?;
    (dir != cfgd_core::default_config_dir()).then(|| dir.to_path_buf())
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
        let dest = match target {
            Some(path) => path.to_path_buf(),
            None => cfgd_core::default_config_dir(),
        };
        refuse_occupied_default_destination(&dest)?;
        if !dest.join(cfgd_core::config::CONFIG_FILENAME).exists() {
            std::fs::create_dir_all(&dest)?;
            clone_into(&dest, from, branch, printer)?;
        } else {
            let mut row = printer.status(
                Role::Info,
                format!(
                    "Already initialized at {}",
                    cfgd_core::fold_home_in_text(&dest.display_posix())
                ),
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
        if !path.join(cfgd_core::config::CONFIG_FILENAME).exists() {
            anyhow::bail!("No cfgd.yaml found in {}", path.posix());
        }
        Ok(path)
    }
}

/// What the default config directory was found to hold, worded for the refusal
/// below, or `None` when it is free for this run to write into.
///
/// The three findings are ordered by what the reader has to act on first: a
/// symlink is reported as a symlink even when it points at a config repository,
/// because the directory that would be written is not the one the path names.
fn occupied_default_destination(dest: &Path) -> Option<&'static str> {
    if std::fs::symlink_metadata(dest).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Some("it is a symlink");
    }
    if dest.join(cfgd_core::config::CONFIG_FILENAME).exists() {
        return Some("it already holds a cfgd.yaml");
    }
    match std::fs::read_dir(dest) {
        Ok(mut entries) => entries.next().map(|_| "it is not empty"),
        // An unreadable directory is a directory this run cannot prove is
        // free, and the whole point of the check is that the cost of being
        // wrong is somebody's config repository.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some("it could not be read"),
    }
}

/// Refuse to write a `--from` source into the default config directory when
/// `dest` names it and it is already somebody's.
///
/// A verb that materialises a config from `--from` resolves a missing
/// destination to [`cfgd_core::default_config_dir`], and the only guard there
/// used to be a `cfgd.yaml` at the top of it: a directory holding a git
/// checkout, a symlink into one, or anything else at all was cloned straight
/// over. The `cfgd.yaml` arm was no guard either — it skipped the clone and
/// handed the directory back, so `apply --from` went on to apply whatever
/// config it found against the real machine.
///
/// The question is asked about the DIRECTORY, not about the string
/// [`from_destination`] compared: [`cfgd_core::absolutize_path`] leaves `..` as
/// a literal component, so `--config ~/.config/cfgd/../cfgd/cfgd.yaml` named
/// the default directory under a spelling no string comparison matches, and a
/// `--config` pointed at whatever the default directory is a symlink to named
/// it under another.
///
/// Two answers, in this order. [`cfgd_core::lexically_normalized`] folds both
/// spellings first, because a `..` walking back through a component that does
/// not exist (`<default>/absent/../cfgd.yaml`) stats nothing, and
/// [`cfgd_core::is_same_inode`] can only say "different" about a path it
/// cannot open. The inode question then catches what the fold cannot: two
/// genuinely different spellings of one directory, reached through a symlink.
/// The occupancy probe reads the folded path for the same reason; the message
/// still names the path the caller wrote.
fn refuse_occupied_default_destination(dest: &Path) -> anyhow::Result<()> {
    let default = cfgd_core::default_config_dir();
    let folded = cfgd_core::lexically_normalized(dest);
    if folded != cfgd_core::lexically_normalized(&default)
        && !cfgd_core::is_same_inode(dest, &default)
    {
        return Ok(());
    }
    let Some(finding) = occupied_default_destination(&folded) else {
        return Ok(());
    };
    let shown = cfgd_core::fold_home_in_text(&dest.display_posix());
    Err(crate::cli::cli_error_with_hints(
        shown.clone(),
        "config_dir_occupied",
        format!("Refusing to write into the default config directory {shown}: {finding}."),
        serde_json::json!({ "destination": shown, "finding": finding }),
        vec![HintCommands::new(
            "Name a destination, or point --config at the config you want this run to use:",
            [
                "cfgd init <dir> --from <source>",
                "cfgd apply --from <source> --config <dir>/cfgd.yaml",
            ],
        )],
    ))
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
        (Some(url), Some(commit)) => Some(format!("{url} {}", commit_detail(&commit))),
        (Some(url), None) => Some(url),
        (None, Some(commit)) => Some(commit_detail(&commit)),
        (None, None) => None,
    }
}

/// The revision half of a checkout detail, spelled once: `at <commit>`.
///
/// `short_commit` owns a commit's display FORM; nothing owned its SPELLING,
/// and the gap let the clone row put a bare `27d1d2046c0a` in the same slot,
/// after the same em dash, in the same dim style as every other detail the run
/// prints — each of which names its unit (`3 vars, 3 aliases`, `5 already
/// deployed`, `4 already installed`). Nothing there names the hex token a commit, and a
/// second `cfgd init` reports the same fact about the same directory in the
/// other shape.
///
/// A commit rendered into a named table COLUMN takes neither: the header is
/// its noun.
pub(super) fn commit_detail(commit: &str) -> String {
    format!("at {commit}")
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
            format!(
                "Skipped clone into {}",
                cfgd_core::fold_home_in_text(&target_dir.posix().to_string())
            ),
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
        format!(
            "Cloned {url} into {}",
            cfgd_core::fold_home_in_text(&target_dir.posix().to_string())
        ),
    );
    // The subject already carries the URL, so this arm cannot take
    // `checkout_detail` wholesale — only its revision spelling.
    if let Some(commit) = checkout_facts(target_dir).1 {
        row = row.detail(commit_detail(&commit));
    }
    drop(row);

    // Checkout the requested branch if HEAD isn't already on it.
    // git clone checks out the remote's default branch; switch when the user
    // asked for a different one.
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
