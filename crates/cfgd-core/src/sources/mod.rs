// Sources — multi-source config management
// Manages fetching, caching, and tracking external config sources (git repos),
// and is the single composition entry point (`SourceManager::compose`) shared by
// every command's desired-state resolution.
// Dependency rules: depends only on config/, output/, errors/, composition/,
// modules/. Must NOT import files/, packages/, secrets/, reconciler/, providers/.

pub mod lockfile;

pub use lockfile::{
    load_sources_lockfile, remove_source_lock_entry, save_sources_lockfile,
    update_source_lock_entry,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{FetchOptions, RemoteCallbacks, Repository};
use semver::{Version, VersionReq};

use crate::config::{
    ConfigSourceDocument, OriginSpec, OriginType, ProfileDocument, ResolvedProfile, SourceSpec,
    parse_config_source,
};
use crate::errors::{Result, SourceError};
use crate::output::{Printer, Role};

pub(crate) const SOURCE_MANIFEST_FILE: &str = "cfgd-source.yaml";
const PROFILES_DIR: &str = "profiles";

/// Whether git would treat this origin URL as a remote rather than a path on
/// this machine. Applies git's own rule: a `scheme://` URL is remote, and so is
/// the scp-style `[user@]host:path` form — recognised, as git recognises it, by
/// a colon appearing before the first slash. Everything else (absolute,
/// relative, UNC and drive-letter paths) is a location on this host.
///
/// Deliberately NOT [`crate::modules::is_git_source`], which is a strict
/// scheme-and-`git@` allow-list: it answers "is this a git URL" for module file
/// sources, where a bare `files/nvim` must stay a path, and rejects the legal
/// `deploy@host:acme/config.git` remote that a source origin may name. This
/// predicate answers the narrower question the source-origin guard asks — "does
/// resolving this reach the network or the local filesystem" — so widening
/// either one to serve the other would break the caller it was not written for.
fn is_remote_origin_url(url: &str) -> bool {
    let url = url.trim();
    if url.to_lowercase().starts_with("file://") {
        return false;
    }
    if url.contains("://") {
        return true;
    }
    match url.split_once(':') {
        Some((host, _)) => {
            // A one-letter "host" is a Windows drive letter (`C:/repo`), which
            // git resolves as a path on the host it runs on.
            !host.is_empty()
                && !host.contains('/')
                && !host.contains('\\')
                && !(host.len() == 1 && host.starts_with(|c: char| c.is_ascii_alphabetic()))
        }
        None => false,
    }
}

/// Cached state for a single config source.
#[derive(Debug, Clone)]
pub struct CachedSource {
    pub name: String,
    pub origin_url: String,
    pub origin_branch: String,
    pub local_path: PathBuf,
    pub manifest: ConfigSourceDocument,
    pub last_commit: Option<String>,
    pub last_fetched: Option<String>,
    /// Resolved tag name when the source was loaded with a `pinVersion` semver
    /// range or exact tag. `None` for HEAD-tracking sources and commit-SHA pins.
    pub resolved_ref: Option<String>,
}

/// Manager for multiple config sources — handles fetching, caching, version checking.
pub struct SourceManager {
    cache_dir: PathBuf,
    sources: HashMap<String, CachedSource>,
    /// When true, skip signature verification even if a source requires it.
    allow_unsigned: bool,
    /// Every advisory this manager has emitted, in the order it emitted them.
    /// Recorded as well as printed so a caller that REUSES a composition can
    /// re-state a condition that still holds without re-composing to hear it
    /// again — a persistent warning that stops appearing reads as resolved. See
    /// [`Self::take_advisories`].
    advisories: Vec<SourceAdvisory>,
}

/// One thing a composition said out loud, carried WITH the channel it was said
/// on.
///
/// A re-stating caller must not pick the channel: a bypass announced through
/// `alert` (which survives `Verbosity::Quiet` and structured output) and
/// re-stated through `status_simple(Role::Warn, …)` is filtered away on exactly
/// the runs it was written for — the daemon's quiet ticks — so the operator
/// hears it once and then hears an unchanged machine go silent. The channel
/// travels with the sentence so the first statement and every restatement are
/// the same statement.
#[derive(Clone, Debug)]
pub(crate) struct SourceAdvisory {
    message: String,
    channel: AdvisoryChannel,
}

/// The two channels a source advisory is ever said on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdvisoryChannel {
    /// An ordinary warning, correctly suppressed at `Verbosity::Quiet` and
    /// under `-o json`: the source was skipped and the composition went on.
    Status,
    /// Always visible, stderr only: the run acted against a security policy the
    /// user declared.
    Alert,
}

impl std::fmt::Display for SourceAdvisory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl SourceAdvisory {
    /// A skip advisory built without a composition, for a test that drives a
    /// holder's restatement rather than the composition that produced one.
    #[cfg(test)]
    pub(crate) fn skipped(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            channel: AdvisoryChannel::Status,
        }
    }

    /// Say it again, on the channel it was first said on.
    pub(crate) fn restate(&self, printer: &Printer) {
        match self.channel {
            AdvisoryChannel::Status => printer.status_simple(Role::Warn, &self.message),
            AdvisoryChannel::Alert => printer.alert(&self.message),
        }
    }
}

impl SourceManager {
    /// Create a new SourceManager using the given cache directory.
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            cache_dir: cache_dir.to_path_buf(),
            sources: HashMap::new(),
            allow_unsigned: false,
            advisories: Vec::new(),
        }
    }

    /// Set whether to allow unsigned source content (bypasses signature verification).
    pub fn set_allow_unsigned(&mut self, allow: bool) {
        self.allow_unsigned = allow;
    }

    /// Say — and remember — that a source was skipped and the composition went
    /// on without it.
    ///
    /// The ONE emission point for a skip advisory, so the sentence a caller
    /// re-states later is byte-identical to the one it first printed rather than
    /// a second copy that can drift from it.
    fn skip_advisory(&mut self, printer: &Printer, message: String) {
        let advisory = SourceAdvisory {
            message,
            channel: AdvisoryChannel::Status,
        };
        advisory.restate(printer);
        self.advisories.push(advisory);
    }

    /// Say — and remember — that a policy this source declared was BYPASSED and
    /// the composition went on anyway.
    ///
    /// `alert`, not `skip_advisory`'s `status_simple(Role::Warn, …)`: a bypass
    /// is a statement about what the run actually did with the user's own
    /// security setting, and every path that loads a source hands `load_source`
    /// a `Verbosity::Quiet` printer (`cfgd sync`, `cfgd compliance`, the daemon)
    /// under which a Warn status is filtered away. `alert` survives Quiet AND
    /// structured output, which is the whole reason it exists. It is remembered
    /// for the same reason a skip is: a holder that caches a composition across
    /// ticks must re-state it, or a bypass that goes quiet reads as one that
    /// stopped happening. `SourceAdvisory` carries that choice with the
    /// sentence, so a re-stating caller cannot undo it.
    fn bypass_advisory(&mut self, printer: &Printer, message: String) {
        let advisory = SourceAdvisory {
            message,
            channel: AdvisoryChannel::Alert,
        };
        advisory.restate(printer);
        self.advisories.push(advisory);
    }

    /// Take the advisories emitted since the last take.
    ///
    /// For a caller that holds a composition across more than one unit of work:
    /// the conditions these describe (a source never synced, a checkout cloned
    /// from an origin the spec no longer names, a signature check bypassed)
    /// persist until someone runs `cfgd sync` or drops `--allow-unsigned`, so a
    /// holder re-states them rather than letting them fall silent behind a cache
    /// hit.
    pub(crate) fn take_advisories(&mut self) -> Vec<SourceAdvisory> {
        std::mem::take(&mut self.advisories)
    }

    /// Default source cache directory: `<cache-root>/sources` under the single
    /// unified cfgd cache root (Linux `~/.cache/cfgd/sources`, macOS
    /// `~/Library/Caches/cfgd/sources`, Windows `%LOCALAPPDATA%\cfgd\sources`).
    pub fn default_cache_dir() -> Result<PathBuf> {
        Self::default_cache_dir_for(crate::Scope::User)
    }

    /// Default source cache directory (`<cache-root>/sources`) for the given
    /// [`crate::Scope`]: per-user under [`crate::Scope::User`], the FHS /
    /// platform system cache root under [`crate::Scope::System`].
    pub fn default_cache_dir_for(scope: crate::Scope) -> Result<PathBuf> {
        Ok(crate::default_cache_dir_for(scope)
            .map_err(|e| SourceError::CacheError {
                message: e.to_string(),
            })?
            .join("sources"))
    }

    /// Load all sources from config, fetching if needed.
    ///
    /// A source marked `sync.required: true` is fail-closed: any load failure
    /// propagates immediately (naming the source), so a security or team
    /// baseline that cannot be fetched aborts apply/plan rather than being
    /// silently composed out. Non-required sources keep best-effort behaviour —
    /// a per-source failure is warn-logged and skipped, and an error is only
    /// returned when sources were specified but every one of them failed.
    pub fn load_sources(&mut self, sources: &[SourceSpec], printer: &Printer) -> Result<()> {
        let mut loaded = 0;
        for spec in sources {
            match self.load_source(spec, printer) {
                Ok(()) => loaded += 1,
                Err(e) if spec.sync.required => {
                    return Err(SourceError::FetchFailed {
                        name: spec.name.clone(),
                        message: format!("required source failed to load: {e}"),
                    }
                    .into());
                }
                Err(e) => {
                    printer.status_simple(
                        Role::Warn,
                        format!("Failed to load source '{}': {}", spec.name, e),
                    );
                }
            }
        }
        if !sources.is_empty() && loaded == 0 {
            return Err(SourceError::GitError {
                name: "all".to_string(),
                message: "all sources failed to load".to_string(),
            }
            .into());
        }
        Ok(())
    }

    /// Load all sources from their existing on-disk cache WITHOUT touching the
    /// network. A source whose cache directory does not yet exist (never synced)
    /// is warned about and skipped, leaving local-only state intact. This is the
    /// read-path loader: `diff`/`status`/`verify`/`compliance`/`checkin` and the
    /// daemon reconcile loop compose the desired state offline and fast, relying
    /// on the daemon's repo-sync (or an explicit `cfgd sync`) for fetch cadence.
    ///
    /// Unlike [`load_sources`](Self::load_sources), a cache miss is not a fatal
    /// "all sources failed" condition — it degrades to local-only for that
    /// source. A source that IS cached but whose manifest is malformed or whose
    /// signature fails still surfaces as an error (a broken desired-state config
    /// must be reported, not silently dropped).
    pub fn load_sources_cached(&mut self, sources: &[SourceSpec], printer: &Printer) -> Result<()> {
        for spec in sources {
            self.load_source_cached(spec, printer)?;
        }
        Ok(())
    }

    /// Load a single source from its on-disk cache without fetching. A
    /// never-synced source (no cache dir) is warned about and skipped; a cached
    /// source with a broken manifest or failed signature is a hard error.
    pub fn load_source_cached(&mut self, spec: &SourceSpec, printer: &Printer) -> Result<()> {
        validate_source_name(&spec.name)?;

        let source_dir = self.cache_dir.join(&spec.name);
        // The checkout DIRECTORY is an input of its own: a source that has never
        // synced contributes nothing, and the arrival of its cache is what
        // changes that answer — a fact no manifest read can report, because on
        // this arm there is no manifest to read.
        crate::record_config_input(&source_dir);
        if !source_dir.exists() {
            self.skip_advisory(
                printer,
                format!(
                    "Source '{}' has no local cache yet — run 'cfgd sync' to fetch it; using local state only",
                    spec.name
                ),
            );
            return Ok(());
        }

        // A read path never fetches, but it must not COMPOSE a checkout that was
        // cloned from a repository the spec no longer names — that is another
        // source's content wearing this one's name. Degrade exactly like the
        // cache-miss above: the sync path is what discards and re-clones. Only
        // a RECORDED origin that disagrees takes this arm — a cache that is not
        // a repository falls through to the parse/signature checks below, whose
        // hard errors are the fail-closed contract for a corrupt cache.
        if Self::cached_recorded_origin(&source_dir)
            .is_some_and(|recorded| recorded != spec.origin.url)
        {
            self.skip_advisory(
                printer,
                format!(
                    "Source '{}': cached checkout was cloned from a different origin — run 'cfgd sync' to re-fetch it; skipped without verifying its signature, using local state only",
                    spec.name
                ),
            );
            return Ok(());
        }

        let manifest = self.parse_manifest(&spec.name, &source_dir)?;

        // A cached source still gets its signature verified — a tampered cache
        // must not silently feed a read path.
        self.verify_commit_signature(
            spec,
            &source_dir,
            &manifest.spec.policy.constraints,
            printer,
        )?;

        let last_commit = Self::head_commit(&source_dir);

        let cached = CachedSource {
            name: spec.name.clone(),
            origin_url: spec.origin.url.clone(),
            origin_branch: spec.origin.branch.clone(),
            local_path: source_dir,
            manifest,
            last_commit,
            last_fetched: None,
            resolved_ref: None,
        };

        self.sources.insert(spec.name.clone(), cached);
        Ok(())
    }

    /// Load a single source — clone or fetch, parse manifest, check version.
    pub fn load_source(&mut self, spec: &SourceSpec, printer: &Printer) -> Result<()> {
        validate_source_name(&spec.name)?;

        // A URL beginning with '-' would be parsed by git as an option rather
        // than the positional remote (clone/ls-remote take the URL positionally).
        // Reject it; the trailing positionals are additionally guarded with
        // --end-of-options as defense in depth. Judged BEFORE the local-origin
        // check below and independently of CFGD_ALLOW_LOCAL_SOURCES, because an
        // injected git option is an attack whatever the origin resolves to, and
        // naming it a "local path" would describe the wrong defect.
        if spec.origin.url.trim_start().starts_with('-') {
            return Err(SourceError::GitError {
                name: spec.name.clone(),
                message: "source origin URL must not begin with '-'".to_string(),
            }
            .into());
        }

        // Reject local file URLs and filesystem paths to prevent local filesystem
        // access from composed sources. A source delivers files, packages and
        // scripts to the machine, so its origin has to be a remote a subscriber
        // can verify and pin — not a directory whose contents anything on the
        // host can rewrite. The check is on what GIT would treat as local rather
        // than on a leading `/`: a bare `acme/config` or `../config` is a
        // relative path git clones from the current directory just as readily as
        // an absolute one, so accepting those left the guard naming a rule it did
        // not enforce. CFGD_ALLOW_LOCAL_SOURCES bypasses this for dev/test
        // environments only.
        let allow_local = std::env::var("CFGD_ALLOW_LOCAL_SOURCES").is_ok();
        if !allow_local && !is_remote_origin_url(&spec.origin.url) {
            return Err(SourceError::GitError {
                name: spec.name.clone(),
                message: format!(
                    "'{}' is a local path or file:// URL, which is not allowed as a source \
                     origin — pass a git URL (e.g. https://github.com/acme/config.git)",
                    spec.origin.url
                ),
            }
            .into());
        }

        // The cache root is a precondition of everything below, the lock file
        // included, so it is created here with the wording a caller who cannot
        // create it needs — before the lock's own silent `create_dir_all` would
        // report the same failure as a bare io error. A root cfgd creates is
        // owner-only; one the operator already made keeps the mode they gave
        // it, because a cache directory can be deliberately shared and
        // re-tightening it on every load would take that choice away.
        //
        // A load that then FAILS leaves the root and its lock file standing.
        // Taking them back would mean deleting a lock file this process holds,
        // which is precisely what strands a contender already blocked on it, so
        // an empty cache root and a zero-byte `cache.lock` are the deliberate
        // residue: the next load reuses both, and neither says anything untrue
        // about the machine.
        let created_cache_root = !self.cache_dir.exists();
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| SourceError::CacheError {
            message: format!("cannot create cache dir: {e}"),
        })?;
        if created_cache_root {
            let _ = crate::set_file_permissions(&self.cache_dir, 0o700);
        }

        self.load_source_locked(spec, printer)
    }

    /// Take the source-cache lock and run the load under it.
    fn load_source_locked(&mut self, spec: &SourceSpec, printer: &Printer) -> Result<()> {
        // Everything from here to the end of the load is a check-then-act over
        // a directory two cfgd processes can be told to own: the origin check
        // reads `.git/config`, the discard removes the tree, and the clone or
        // fetch rebuilds it. Unserialized, a second process's fetch resolves
        // `origin` after this one re-pointed it, or clones into a tree this one
        // is still removing.
        //
        // The notice goes through `alert`, not `status_simple`: the wait is
        // unbounded and can cover another process's network clone, and the
        // command most likely to contend (`cfgd sync`) hands `load_source` a
        // Quiet printer that swallows every role but `Fail`. An advisory about
        // what this run is actually doing has to survive that, so it takes the
        // always-visible stderr channel `alert` and `deprecation` share.
        let _cache_lock = crate::acquire_source_lock(&self.cache_dir, || {
            printer.alert(cache_wait_notice(&spec.name));
        })?;

        self.load_source_guarded(spec, printer)
    }

    /// The body of [`Self::load_source`] that runs under the source-cache lock,
    /// from the origin check through the completed clone or fetch.
    fn load_source_guarded(&mut self, spec: &SourceSpec, printer: &Printer) -> Result<()> {
        let source_dir = self.cache_dir.join(&spec.name);

        // The cache is keyed by the source NAME alone, so nothing else ties an
        // existing checkout to the origin it was cloned from. Left unchecked, a
        // subscription whose origin changed — or two configs sharing one cache
        // under the same name — fetches whatever repository the stale clone
        // still points at and serves content the spec never named. A clone
        // whose recorded origin disagrees is discarded and re-cloned rather
        // than re-pointed: unrelated histories refuse the fast-forward, which
        // would quietly keep serving the OLD checkout after a "successful"
        // fetch. A directory that is not a repository (or records no origin)
        // is equally unfetchable, so it takes the same discard-and-reclone
        // self-heal here on the sync path.
        if source_dir.exists()
            && Self::cached_recorded_origin(&source_dir)
                .is_none_or(|recorded| recorded != spec.origin.url)
        {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Source '{}': cached checkout was cloned from a different origin — discarding it and re-cloning from '{}'",
                    spec.name, spec.origin.url
                ),
            );
            std::fs::remove_dir_all(&source_dir).map_err(|e| SourceError::CacheError {
                message: format!(
                    "failed to discard stale cache for source '{}': {e}",
                    spec.name
                ),
            })?;
        }

        // A pin resolves to a concrete git ref (tag or commit SHA) rather than
        // tracking a branch. Resolution happens on every load so a semver-range
        // pin re-selects the highest matching tag when the remote gains one.
        //
        // When the pin no longer matches any remote ref, the resolution returns
        // `PinRefNotFound`. For a non-required source that already has a
        // resolved checkout on disk, this is NOT fatal: keep the
        // previously-resolved checkout (parse + compose it) instead of dropping
        // the source. A required source — or a first-ever load with no cache —
        // still fails fast. Only the pin-not-found case gets this fallback; a
        // network/ls-remote error (GitError) still propagates.
        let pinned_ref = match spec.sync.pin_version.as_deref() {
            Some(pin) => match self.resolve_pinned_ref(spec, pin) {
                Ok(resolved) => Some(resolved),
                Err(e)
                    if matches!(
                        e,
                        crate::errors::CfgdError::Source(SourceError::PinRefNotFound { .. })
                    ) && !spec.sync.required
                        && source_dir.exists() =>
                {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Source '{}': pin '{}' no longer matches any ref; keeping the previously-resolved checkout",
                            spec.name, pin
                        ),
                    );
                    return self.load_from_existing_cache(spec, &source_dir, printer);
                }
                Err(e) => return Err(e),
            },
            None => None,
        };

        match (&pinned_ref, source_dir.exists()) {
            (Some(resolved), true) => {
                self.checkout_pinned_ref(spec, &source_dir, resolved, printer)?
            }
            (Some(resolved), false) => {
                self.clone_pinned_source(spec, &source_dir, resolved, printer)?
            }
            (None, true) => self.fetch_source(spec, &source_dir, printer)?,
            (None, false) => self.clone_source(spec, &source_dir, printer)?,
        }

        let manifest = self.parse_manifest(&spec.name, &source_dir)?;

        // Signature verification: if the source requires signed commits, verify HEAD
        self.verify_commit_signature(
            spec,
            &source_dir,
            &manifest.spec.policy.constraints,
            printer,
        )?;

        let last_commit = Self::head_commit(&source_dir);

        let resolved_ref = pinned_ref.as_ref().and_then(|r| match r {
            ResolvedRef::Tag { tag, .. } => Some(tag.clone()),
            ResolvedRef::Commit(_) => None,
        });

        let cached = CachedSource {
            name: spec.name.clone(),
            origin_url: spec.origin.url.clone(),
            origin_branch: spec.origin.branch.clone(),
            local_path: source_dir,
            manifest,
            last_commit,
            last_fetched: Some(crate::utc_now_iso8601()),
            resolved_ref,
        };

        self.sources.insert(spec.name.clone(), cached);
        Ok(())
    }

    /// Insert a source from its existing on-disk checkout without re-fetching.
    /// Used when a `pinVersion` no longer resolves but a prior successful load
    /// left a usable checkout: parse + signature-verify the cached manifest and
    /// keep it composed at the previously-resolved ref. A corrupt manifest or
    /// failed signature still surfaces as an error — only the pin-not-found case
    /// routes here.
    fn load_from_existing_cache(
        &mut self,
        spec: &SourceSpec,
        source_dir: &Path,
        printer: &Printer,
    ) -> Result<()> {
        let manifest = self.parse_manifest(&spec.name, source_dir)?;
        self.verify_commit_signature(spec, source_dir, &manifest.spec.policy.constraints, printer)?;

        let last_commit = Self::head_commit(source_dir);

        let cached = CachedSource {
            name: spec.name.clone(),
            origin_url: spec.origin.url.clone(),
            origin_branch: spec.origin.branch.clone(),
            local_path: source_dir.to_path_buf(),
            manifest,
            last_commit,
            last_fetched: None,
            resolved_ref: None,
        };

        self.sources.insert(spec.name.clone(), cached);
        Ok(())
    }

    /// The origin URL the cached checkout at `source_dir` records, or `None`
    /// for a directory that is not a repository or has no `origin` remote.
    /// `None` is deliberately NOT an answer about mismatch: the sync path
    /// discards it as unfetchable either way, while the read path falls
    /// through so a corrupt cache keeps hitting the parse/signature hard
    /// errors it always has instead of being quietly skipped.
    fn cached_recorded_origin(source_dir: &Path) -> Option<String> {
        // The verdict derived from this read is replayed to the operator on
        // every tick that reuses the composition, so the file it rests on has to
        // be part of what retires that composition. Re-pointing an origin in
        // place need not move the checkout directory's own stamp.
        crate::record_config_input(&source_dir.join(".git").join("config"));
        let repo = Repository::open(source_dir).ok()?;
        let remote = repo.find_remote("origin").ok()?;
        remote.url().ok().map(str::to_owned)
    }

    /// Fetch (pull) updates for an already-cloned source.
    fn fetch_source(&self, spec: &SourceSpec, source_dir: &Path, printer: &Printer) -> Result<()> {
        let to_git_err = |e: git2::Error| SourceError::GitError {
            name: spec.name.clone(),
            message: e.to_string(),
        };

        // Try git CLI first with live progress output.
        let mut cmd = crate::git_cmd_safe(
            Some(&spec.origin.url),
            Some(spec.origin.ssh_strict_host_key_checking),
        );
        cmd.args([
            "-C",
            &source_dir.display().to_string(),
            "fetch",
            "origin",
            &spec.origin.branch,
        ]);

        let label = format!("Fetching source '{}'", spec.name);
        let cli_result = printer.run(&mut cmd, &label);
        let cli_ok = matches!(&cli_result, Ok(output) if output.status.success());

        if !cli_ok {
            // Fall back to libgit2 with spinner
            let spinner = printer.spinner(format!("Fetching source '{}' (libgit2)...", spec.name));

            let repo = Repository::open(source_dir).map_err(to_git_err)?;

            let mut remote = repo.find_remote("origin").map_err(to_git_err)?;

            let mut fo = FetchOptions::new();
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(crate::git_ssh_credentials);
            fo.remote_callbacks(callbacks);

            let fetch_result = remote
                .fetch(&[&spec.origin.branch], Some(&mut fo), None)
                .map_err(|e| SourceError::FetchFailed {
                    name: spec.name.clone(),
                    message: e.to_string(),
                });

            match &fetch_result {
                Ok(_) => {
                    let _ = spinner.finish_ok(format!("Fetched source '{}' (libgit2)", spec.name));
                }
                Err(e) => {
                    let _ = spinner
                        .finish_fail(format!("Failed to fetch source '{}' (libgit2)", spec.name))
                        .detail(crate::output::collapse_to_subject_line(e));
                }
            }
            fetch_result?;
        }

        // Fast-forward to FETCH_HEAD
        let repo = Repository::open(source_dir).map_err(to_git_err)?;

        let fetch_head = repo.find_reference("FETCH_HEAD").map_err(to_git_err)?;
        let fetch_commit = repo
            .reference_to_annotated_commit(&fetch_head)
            .map_err(to_git_err)?;

        let (analysis, _) = repo.merge_analysis(&[&fetch_commit]).map_err(to_git_err)?;

        if analysis.is_fast_forward() {
            let refname = format!("refs/heads/{}", spec.origin.branch);
            if let Ok(mut reference) = repo.find_reference(&refname) {
                reference
                    .set_target(fetch_commit.id(), "cfgd source fetch")
                    .map_err(to_git_err)?;
            }
            repo.set_head(&refname).map_err(to_git_err)?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
                .map_err(to_git_err)?;
        }

        Ok(())
    }

    /// Clone a new source repo. Tries git CLI first (respects system credential
    /// helpers and SSH config), falls back to libgit2.
    fn clone_source(&self, spec: &SourceSpec, source_dir: &Path, printer: &Printer) -> Result<()> {
        // Ensure parent dir exists but not source_dir itself (git clone creates it)
        if let Some(parent) = source_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SourceError::CacheError {
                message: format!("cannot create cache dir: {}", e),
            })?;
        }

        // Try git CLI first with live progress output.
        // --depth=1: only fetch latest commit (limits repo size for DoS protection)
        // --no-recurse-submodules: prevent malicious submodule URLs (SSRF, credential theft)
        // --single-branch: only fetch the target branch
        let mut cmd = crate::git_cmd_safe(
            Some(&spec.origin.url),
            Some(spec.origin.ssh_strict_host_key_checking),
        );
        cmd.args([
            "clone",
            "--depth=1",
            "--single-branch",
            "--no-recurse-submodules",
            "--branch",
            &spec.origin.branch,
            "--end-of-options",
            &spec.origin.url,
            &source_dir.display().to_string(),
        ]);

        let label = format!("Cloning source '{}'", spec.name);
        let cli_result = printer.run(&mut cmd, &label);
        if matches!(&cli_result, Ok(output) if output.status.success()) {
            // Restrict cloned directory to owner-only access
            let _ = crate::set_file_permissions(source_dir, 0o700);
            return Ok(());
        }

        // Clean up partial clone before libgit2 retry
        let _ = std::fs::remove_dir_all(source_dir);

        // Fall back to libgit2 with spinner
        let spinner = printer.spinner(format!("Cloning source '{}' (libgit2)...", spec.name));

        let mut fo = FetchOptions::new();
        if spec.origin.url.starts_with("git@") || spec.origin.url.starts_with("ssh://") {
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(crate::git_ssh_credentials);
            fo.remote_callbacks(callbacks);
        }

        // Shallow clone with depth 1, disable submodule init
        fo.depth(1);
        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fo);
        builder.branch(&spec.origin.branch);

        let clone_result =
            builder
                .clone(&spec.origin.url, source_dir)
                .map_err(|e| SourceError::FetchFailed {
                    name: spec.name.clone(),
                    message: e.to_string(),
                });

        match &clone_result {
            Ok(_) => {
                let _ = spinner.finish_ok(format!("Cloned source '{}' (libgit2)", spec.name));
            }
            Err(e) => {
                let _ = spinner
                    .finish_fail(format!("Failed to clone source '{}' (libgit2)", spec.name))
                    .detail(crate::output::collapse_to_subject_line(e));
            }
        }
        clone_result?;

        // Restrict cloned directory to owner-only access
        let _ = crate::set_file_permissions(source_dir, 0o700);

        Ok(())
    }

    /// Clone a source pinned to a resolved git ref (tag or commit SHA).
    ///
    /// Tags use the same fully-hardened shallow `--branch <tag>` clone as the
    /// branch path (libgit2 fallback included). Commit SHAs cannot be cloned
    /// via `--branch`, so the default branch is cloned shallow and the commit is
    /// fetched-by-SHA then checked out (detached); a server that refuses
    /// `allowReachableSHA1InWant` triggers a deeper fetch fallback (announced via
    /// a Printer note so the depth relaxation is never silent). All clones keep
    /// `--depth=1`, `--no-recurse-submodules`, and `0o700` directory perms.
    fn clone_pinned_source(
        &self,
        spec: &SourceSpec,
        source_dir: &Path,
        resolved: &ResolvedRef,
        printer: &Printer,
    ) -> Result<()> {
        match resolved {
            ResolvedRef::Tag { tag, .. } => {
                let tag_spec = SourceSpec {
                    origin: OriginSpec {
                        branch: tag.clone(),
                        ..spec.origin.clone()
                    },
                    ..spec.clone()
                };
                self.clone_source(&tag_spec, source_dir, printer)
            }
            ResolvedRef::Commit(sha) => self.clone_commit_source(spec, source_dir, sha, printer),
        }
    }

    /// Clone the default/declared branch shallow, then fetch + checkout a commit SHA.
    /// On ANY failure the partial clone is removed so the next `load_source` does
    /// not mistake a broken directory for a usable cache entry.
    fn clone_commit_source(
        &self,
        spec: &SourceSpec,
        source_dir: &Path,
        sha: &str,
        printer: &Printer,
    ) -> Result<()> {
        self.clone_commit_source_inner(spec, source_dir, sha, printer)
            .inspect_err(|_| {
                let _ = std::fs::remove_dir_all(source_dir);
            })
    }

    fn clone_commit_source_inner(
        &self,
        spec: &SourceSpec,
        source_dir: &Path,
        sha: &str,
        printer: &Printer,
    ) -> Result<()> {
        if let Some(parent) = source_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SourceError::CacheError {
                message: format!("cannot create cache dir: {}", e),
            })?;
        }

        let mut cmd = crate::git_cmd_safe(
            Some(&spec.origin.url),
            Some(spec.origin.ssh_strict_host_key_checking),
        );
        cmd.args([
            "clone",
            "--depth=1",
            "--single-branch",
            "--no-recurse-submodules",
            "--end-of-options",
            &spec.origin.url,
            &source_dir.display().to_string(),
        ]);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let label = format!("Cloning source '{}'", spec.name);
        let cli_result = printer.run(&mut cmd, &label);
        if !matches!(&cli_result, Ok(output) if output.status.success()) {
            return Err(SourceError::FetchFailed {
                name: spec.name.clone(),
                message: format!("failed to clone '{}' for commit pin", spec.origin.url),
            }
            .into());
        }
        let _ = crate::set_file_permissions(source_dir, 0o700);

        // Shallow-first/stepped-deepen/unbounded fetch of the pinned commit, then
        // detached checkout. `fetch_ref_with_fallback` inserts `--end-of-options`.
        self.fetch_ref_with_fallback(spec, source_dir, sha, sha, printer)?;
        self.git_checkout_detached(spec, source_dir, sha)
    }

    /// Parse the ConfigSource manifest from a source directory.
    pub fn parse_manifest(&self, name: &str, source_dir: &Path) -> Result<ConfigSourceDocument> {
        read_manifest(name, source_dir)
    }

    /// Verify the HEAD commit of a source repo has a valid GPG or SSH signature.
    ///
    /// The requirement is [`SourceSpec::requires_signed_commits`]: the
    /// subscriber's `subscription.requireSignedCommits` ORed with the manifest's
    /// `constraints.requireSignedCommits`. It takes the SPEC rather than a name
    /// precisely so the subscriber's half cannot be forgotten here — the
    /// manifest half arrives from inside the cached clone, which is the one
    /// thing a planted cache controls. `allow_unsigned` on this SourceManager
    /// still bypasses both.
    pub(crate) fn verify_commit_signature(
        &mut self,
        spec: &SourceSpec,
        source_dir: &Path,
        constraints: &crate::config::SourceConstraints,
        printer: &Printer,
    ) -> Result<()> {
        if !spec.requires_signed_commits(constraints.require_signed_commits) {
            return Ok(());
        }

        let name = spec.name.as_str();
        if self.allow_unsigned {
            self.bypass_advisory(
                printer,
                format!(
                    "Source '{name}' requires signed commits, but --allow-unsigned is set — composing it WITHOUT verifying its signature"
                ),
            );
            return Ok(());
        }

        verify_head_signature(name, source_dir)?;
        // The user asked for signed commits, so the check having RUN is part of
        // what they asked for. An ordinary status: a read path composes at
        // `Verbosity::Quiet` and correctly says nothing, and a failure is an
        // error rather than a line.
        printer.status_simple(
            Role::Ok,
            format!("Source '{name}': HEAD commit signature verified"),
        );
        Ok(())
    }

    /// Resolve a `pinVersion` value to a concrete git ref against the remote.
    ///
    /// The pin is interpreted (in order):
    /// 1. A semver range (after [`normalize_semver_pin`]) — list the remote's
    ///    tags, strip a leading `v`, filter by the range, and select the highest
    ///    matching tag.
    /// 2. A 7–40 hex commit SHA.
    /// 3. An exact tag name that matches a remote tag verbatim.
    ///
    /// This pins against the remote's git refs rather than the source's
    /// self-reported `metadata.version`, so a source cannot bypass the pin by
    /// editing its own manifest. (Signature verification of the resulting HEAD
    /// is a separate step — see [`Self::verify_commit_signature`].)
    fn resolve_pinned_ref(&self, spec: &SourceSpec, pin: &str) -> Result<ResolvedRef> {
        let tags = list_remote_tags(&spec.name, &spec.origin)?;
        resolve_ref_from_tags(&spec.name, pin, &tags)
    }

    /// Checkout a previously-resolved pinned ref in an already-cloned source.
    /// Detached-HEAD checkout — re-resolution of a semver range may have selected
    /// a different (higher) tag than the last load, and a SHA re-pin may target a
    /// commit absent from the existing shallow clone, so both first fetch the ref.
    fn checkout_pinned_ref(
        &self,
        spec: &SourceSpec,
        source_dir: &Path,
        resolved: &ResolvedRef,
        printer: &Printer,
    ) -> Result<()> {
        // The existing shallow clone was created for a different ref, so the
        // newly-resolved tag/commit may be absent. Fetch it (with the same
        // stepped-depth fallback as the initial commit clone) before checkout.
        match resolved {
            ResolvedRef::Tag { tag, .. } => self.fetch_ref_with_fallback(
                spec,
                source_dir,
                &format!("refs/tags/{tag}:refs/tags/{tag}"),
                tag,
                printer,
            )?,
            ResolvedRef::Commit(sha) => {
                self.fetch_ref_with_fallback(spec, source_dir, sha, sha, printer)?
            }
        }

        let target = match resolved {
            ResolvedRef::Tag { tag, .. } => tag.as_str(),
            ResolvedRef::Commit(sha) => sha.as_str(),
        };
        self.git_checkout_detached(spec, source_dir, target)
    }

    /// Fetch a single ref (tag refspec or bare commit SHA) into an existing
    /// clone, preserving the shallow-first/stepped-deepen/unbounded ladder.
    ///
    /// `fetch_arg` is the positional passed to `git fetch origin <fetch_arg>`
    /// (a `refs/tags/x:refs/tags/x` refspec or a SHA); `display` names the ref
    /// in the Printer note. Each step inserts `--end-of-options` so an
    /// attacker-shaped tag/refspec can never be parsed as a git flag.
    fn fetch_ref_with_fallback(
        &self,
        spec: &SourceSpec,
        source_dir: &Path,
        fetch_arg: &str,
        display: &str,
        printer: &Printer,
    ) -> Result<()> {
        let dir = source_dir.display().to_string();
        let run_fetch = |depth: Option<u32>| -> bool {
            let mut cmd = crate::git_cmd_safe(
                Some(&spec.origin.url),
                Some(spec.origin.ssh_strict_host_key_checking),
            );
            cmd.args(["-C", &dir, "fetch"]);
            if let Some(d) = depth {
                cmd.arg(format!("--depth={d}"));
            }
            cmd.args(["origin", "--end-of-options", fetch_arg]);
            matches!(
                crate::command_output_with_timeout(&mut cmd, crate::GIT_NETWORK_TIMEOUT),
                Ok(o) if o.status.success()
            )
        };

        // 1) shallow. 2) stepped deepen (keeps a size bound for the common case).
        // 3) unbounded — last resort for a deeply-buried commit.
        if run_fetch(Some(1)) {
            return Ok(());
        }
        printer.note(format!(
            "Source '{}': shallow fetch of {} failed; deepening fetch (--depth=50)",
            spec.name, display
        ));
        if run_fetch(Some(50)) {
            return Ok(());
        }
        printer.note(format!(
            "Source '{}': stepped deepen of {} failed; fetching full history",
            spec.name, display
        ));
        if run_fetch(None) {
            return Ok(());
        }
        Err(SourceError::FetchFailed {
            name: spec.name.clone(),
            message: format!("could not fetch ref '{}' from origin", display),
        }
        .into())
    }

    /// Run a detached-HEAD `git checkout <target>` in the source dir.
    /// `--end-of-options` precedes the (attacker-influenced) target so a tag
    /// named e.g. `-x` can never be parsed as a checkout flag.
    fn git_checkout_detached(
        &self,
        spec: &SourceSpec,
        source_dir: &Path,
        target: &str,
    ) -> Result<()> {
        let mut checkout = crate::git_cmd_local();
        checkout.args([
            "-C",
            &source_dir.display().to_string(),
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "--detach",
            "--end-of-options",
            target,
        ]);
        checkout.stdout(std::process::Stdio::piped());
        checkout.stderr(std::process::Stdio::piped());
        let output = crate::command_output_with_timeout(&mut checkout, crate::COMMAND_TIMEOUT)
            .map_err(|e| SourceError::GitError {
                name: spec.name.clone(),
                message: format!("failed to checkout '{}': {}", target, e),
            })?;
        if !output.status.success() {
            return Err(SourceError::GitError {
                name: spec.name.clone(),
                message: format!(
                    "checkout of '{}' failed: {}",
                    target,
                    crate::stderr_lossy_trimmed(&output)
                ),
            }
            .into());
        }
        Ok(())
    }

    /// Get the HEAD commit hash for a repo.
    fn head_commit(source_dir: &Path) -> Option<String> {
        let repo = Repository::open(source_dir).ok()?;
        let head = repo.head().ok()?;
        head.target().map(|oid| oid.to_string())
    }

    /// Get a cached source by name.
    pub fn get(&self, name: &str) -> Option<&CachedSource> {
        self.sources.get(name)
    }

    /// Get all cached sources.
    pub fn all_sources(&self) -> &HashMap<String, CachedSource> {
        &self.sources
    }

    /// Load a profile from a source's profiles directory.
    pub fn load_source_profile(
        &self,
        source_name: &str,
        profile_name: &str,
    ) -> Result<ProfileDocument> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;

        // Chokepoint lookup: accepts both the canonical bundle form
        // (profiles/<name>/profile.yaml) and the legacy flat form, and fails
        // closed when both coexist in the clone.
        let profiles_dir = cached.local_path.join(PROFILES_DIR);
        let profile_path = match crate::config::find_profile_path(&profiles_dir, profile_name) {
            Ok(path) => path,
            Err(crate::errors::ConfigError::ProfileNotFound { .. }) => {
                return Err(SourceError::ProfileNotFound {
                    name: source_name.to_string(),
                    profile: profile_name.to_string(),
                }
                .into());
            }
            Err(other) => return Err(other.into()),
        };

        crate::config::load_profile(&profile_path)
    }

    /// Get the source profiles directory path.
    pub fn source_profiles_dir(&self, source_name: &str) -> Result<PathBuf> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;
        Ok(cached.local_path.join(PROFILES_DIR))
    }

    /// Deterministic on-disk profiles-directory path for a source name, derived
    /// from the cache layout WITHOUT requiring the source to be loaded into the
    /// in-memory map. Unlike [`source_profiles_dir`](Self::source_profiles_dir),
    /// this never fails on an unloaded source and performs no network or
    /// signature work — it is the offline-scan entry point (e.g. a CLI smart
    /// error that wants to know whether a subscribed source would provide a
    /// given profile). The returned path may not exist.
    pub fn cached_profiles_dir(&self, source_name: &str) -> PathBuf {
        self.cache_dir.join(source_name).join(PROFILES_DIR)
    }

    /// Get the source modules directory path (`<cache>/modules`), where bodies
    /// for the names in the manifest's `provides.modules` allow-list live.
    pub fn source_modules_dir(&self, source_name: &str) -> Result<PathBuf> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;
        Ok(cached.local_path.join("modules"))
    }

    /// The module names this source declares as deliverable — the manifest's
    /// `spec.provides.modules` allow-list. A module body present in the cache but
    /// absent from this list is NOT delivered to subscribers.
    pub fn available_source_modules(&self, source_name: &str) -> Result<Vec<String>> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;
        Ok(cached.manifest.spec.provides.modules.clone())
    }

    /// Get the source files directory path.
    pub fn source_files_dir(&self, source_name: &str) -> Result<PathBuf> {
        let cached = self
            .sources
            .get(source_name)
            .ok_or_else(|| SourceError::NotFound {
                name: source_name.to_string(),
            })?;
        Ok(cached.local_path.join("files"))
    }

    /// Remove a source from cache.
    pub fn remove_source(&mut self, name: &str, printer: &Printer) -> Result<()> {
        self.sources
            .remove(name)
            .ok_or_else(|| SourceError::NotFound {
                name: name.to_string(),
            })?;

        discard_cached_checkout(&self.cache_dir, name, printer)
    }

    /// Compose this manager's already-loaded sources with a local resolved
    /// profile into the effective desired state.
    ///
    /// This is the single composition code path shared by every command (CLI
    /// read/write paths and the daemon reconcile loop): build a
    /// [`CompositionInput`](crate::composition::CompositionInput) per loaded
    /// source, run [`compose`](crate::composition::compose) (fatal on constraint
    /// violations — the sole fail-closed chokepoint), then populate the
    /// `source_commits` and `source_module_roots` that only a `SourceManager`
    /// can supply. Sources listed in `cfg_sources` but not currently loaded
    /// (e.g. cache-miss on a read path) are skipped here.
    ///
    /// Conflict *display* and *persistence* are left to the caller (it owns the
    /// printer-section style and the state store); the returned
    /// [`CompositionResult::conflicts`](crate::composition::CompositionResult)
    /// carries them.
    pub fn compose(
        &self,
        cfg_sources: &[SourceSpec],
        local: &ResolvedProfile,
        mode: crate::composition::ConstraintMode,
    ) -> Result<crate::composition::CompositionResult> {
        use crate::composition::{CompositionInput, SubscriptionConfig};

        // Authoritative fail-closed gate. This is the single chokepoint every
        // read/refresh/daemon path flows through, so enforcing `sync.required`
        // here — rather than only in the refresh-time `load_sources` — covers
        // the cache-only paths (diff/status/verify/compliance/checkin, daemon
        // reconcile) by construction. A required source that did not load for
        // ANY reason (cache miss, warn-skip, fetch/manifest/signature failure)
        // is absent from `self.sources`; without this check the loop below would
        // silently `continue` past it, and the daemon's pruning reconcile would
        // then uninstall its packages/modules as phantom drift.
        for spec in cfg_sources {
            if spec.sync.required && self.get(&spec.name).is_none() {
                return Err(crate::errors::CompositionError::RequiredSourceUnavailable {
                    source_name: spec.name.clone(),
                }
                .into());
            }
        }

        let mut inputs = Vec::new();
        for spec in cfg_sources {
            let Some(cached) = self.get(&spec.name) else {
                continue;
            };

            let mut layers = Vec::new();
            if let Some(ref profile_name) = spec.subscription.profile {
                let profiles_dir = self.source_profiles_dir(&spec.name)?;
                if profiles_dir.exists() {
                    layers = crate::config::resolve_profile(profile_name, &profiles_dir)?.layers;
                }
            }

            inputs.push(CompositionInput {
                source_name: spec.name.clone(),
                priority: spec.subscription.priority,
                policy: cached.manifest.spec.policy.clone(),
                constraints: cached.manifest.spec.policy.constraints.clone(),
                layers,
                subscription: SubscriptionConfig::from_spec(spec),
                allow_scripts: spec.subscription.allow_scripts,
            });
        }

        // No source actually loaded (e.g. every source cache-missed on a read
        // path): return the local profile UNCHANGED. Re-composing with zero
        // inputs would rebuild `merged` from `local.layers` alone, discarding any
        // pre-merged state a caller passed in.
        if inputs.is_empty() {
            return Ok(crate::composition::CompositionResult {
                resolved: local.clone(),
                conflicts: Vec::new(),
                source_env: HashMap::new(),
                source_commits: HashMap::new(),
                source_module_roots: Vec::new(),
                constraint_violations: Vec::new(),
            });
        }

        let mut result = crate::composition::compose(local, &inputs, mode)?;

        for spec in cfg_sources {
            if let Some(cached) = self.get(&spec.name)
                && let Some(ref commit) = cached.last_commit
            {
                result
                    .source_commits
                    .insert(spec.name.clone(), commit.clone());
            }
        }

        for spec in cfg_sources {
            if let Some(cached) = self.get(&spec.name) {
                let modules_dir = self.source_modules_dir(&spec.name)?;
                let offered = self.available_source_modules(&spec.name)?;
                // A source's scripts are permitted iff the subscriber opted in or
                // the source does not constrain scripts. This is the same decision
                // the profile-layer enforcement applies (see compose), carried onto
                // the module-resolution path which lives in a different code path.
                let scripts_permitted = spec.subscription.allow_scripts
                    || !cached.manifest.spec.policy.constraints.no_scripts;
                result
                    .source_module_roots
                    .push(crate::modules::SourceModuleRoot {
                        source_name: spec.name.clone(),
                        priority: spec.subscription.priority,
                        modules_dir,
                        offered,
                        scripts_permitted,
                    });
            }
        }

        Ok(result)
    }

    /// Build a SourceSpec for adding a new source.
    pub fn build_source_spec(name: &str, url: &str, profile: Option<&str>) -> SourceSpec {
        SourceSpec {
            name: name.to_string(),
            origin: OriginSpec {
                origin_type: OriginType::Git,
                url: url.to_string(),
                branch: "master".to_string(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: crate::config::SubscriptionSpec {
                profile: profile.map(|s| s.to_string()),
                ..Default::default()
            },
            sync: Default::default(),
        }
    }
}

/// The advisory a contended source-cache acquire announces, so the two callers
/// that can wait on it cannot describe the same wait two ways.
fn cache_wait_notice(name: &str) -> String {
    format!("Source '{name}': waiting for another cfgd process to finish updating the source cache")
}

/// Remove the cached checkout of `name` under `cache_dir`, holding the
/// source-cache lock across the removal.
///
/// The ONE deletion of a source's checkout, and why it is not an inline
/// `remove_dir_all` at either caller: unserialized, `cfgd source remove` deletes
/// the tree a concurrent `cfgd sync` is cloning into, which is the same
/// interleaving the lock exists to close on the load side. `cfgd source remove`
/// reaches it directly rather than through
/// [`SourceManager::remove_source`], because that command never populates the
/// in-memory `sources` map its `NotFound` is judged against.
///
/// A checkout that is already gone is not an error: the caller asked for its
/// absence, and another process winning the race delivered exactly that.
pub fn discard_cached_checkout(cache_dir: &Path, name: &str, printer: &Printer) -> Result<()> {
    validate_source_name(name)?;
    let checkout = cache_dir.join(name);
    // Judged before the lock so a removal with no cache at all neither creates
    // the cache root nor leaves a lock file in one.
    if !checkout.exists() {
        return Ok(());
    }
    let _cache_lock = crate::acquire_source_lock(cache_dir, || {
        printer.alert(cache_wait_notice(name));
    })?;
    match std::fs::remove_dir_all(&checkout) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SourceError::CacheError {
            message: format!("failed to remove cache for '{name}': {e}"),
        }
        .into()),
    }
}

/// Reject a source name that cannot serve as a cache directory of its own.
///
/// The name is the only thing separating one source's cache from another's, and
/// the sync path removes and re-clones that directory. A name of `.` would point
/// at the cache root shared by every source.
fn validate_source_name(name: &str) -> Result<()> {
    crate::validate_plain_name(name).map_err(|e| SourceError::GitError {
        name: name.to_string(),
        message: format!("invalid source name: {e}"),
    })?;
    // A source's checkout is `<cache_dir>/<name>`, and the cache lock is a file
    // at `<cache_dir>/cache.lock`. A source claiming that name would put a
    // directory where the lock file goes, so neither could be opened.
    if name
        .split(['/', '\\'])
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case(crate::SOURCE_CACHE_LOCK_FILENAME))
    {
        return Err(SourceError::GitError {
            name: name.to_string(),
            message: format!(
                "invalid source name: '{}' is reserved for the source-cache lock",
                crate::SOURCE_CACHE_LOCK_FILENAME
            ),
        }
        .into());
    }
    Ok(())
}

/// Read and parse a cfgd-source.yaml manifest from a directory.
fn read_manifest(name: &str, source_dir: &Path) -> Result<ConfigSourceDocument> {
    let manifest_path = source_dir.join(SOURCE_MANIFEST_FILE);
    crate::record_config_input(&manifest_path);
    if !manifest_path.exists() {
        return Err(SourceError::InvalidManifest {
            name: name.to_string(),
            message: format!("{} not found", SOURCE_MANIFEST_FILE),
        }
        .into());
    }

    let contents =
        std::fs::read_to_string(&manifest_path).map_err(|e| SourceError::InvalidManifest {
            name: name.to_string(),
            message: e.to_string(),
        })?;

    let doc = parse_config_source(&contents).map_err(|e| SourceError::InvalidManifest {
        name: name.to_string(),
        message: e.to_string(),
    })?;

    if doc.spec.provides.profiles.is_empty()
        && doc.spec.provides.profile_details.is_empty()
        && doc.spec.provides.modules.is_empty()
    {
        return Err(SourceError::EmptyProvides {
            name: name.to_string(),
        }
        .into());
    }

    Ok(doc)
}

/// Check if a directory contains a cfgd-source.yaml manifest.
/// Returns Ok(Some(doc)) if found and valid, Ok(None) if not present,
/// Err if file exists but is invalid.
pub fn detect_source_manifest(dir: &Path) -> Result<Option<ConfigSourceDocument>> {
    let manifest_path = dir.join(SOURCE_MANIFEST_FILE);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    read_manifest(name, dir).map(Some)
}

/// Verify that the HEAD commit of a git repo has a valid GPG or SSH signature.
/// Uses `git log --format=%G?` to check signature status. Returns Ok(()) if the
/// signature is valid (G) or valid with unknown trust (U). Returns an error for
/// unsigned, bad, expired, revoked, or unverifiable signatures.
pub fn verify_head_signature(name: &str, repo_dir: &Path) -> Result<()> {
    if !crate::command_available("git") {
        return Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "git CLI is required for signature verification but is not available on PATH"
                .into(),
        }
        .into());
    }

    let output = crate::command_output_with_timeout(
        crate::git_cmd_local().args([
            "-C",
            &repo_dir.display().to_string(), // native-ok: argv for local git invocation on this host
            "log",
            "--format=%G?",
            "-1",
        ]),
        crate::COMMAND_TIMEOUT,
    )
    .map_err(|e| SourceError::SignatureVerificationFailed {
        name: name.to_string(),
        message: format!("failed to run git: {}", e),
    })?;

    if !output.status.success() {
        return Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: format!(
                "git log failed ({}): {}",
                crate::exit_status_reason(&output.status),
                crate::stderr_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    let status = crate::stdout_lossy_trimmed(&output);
    classify_signature_status(name, &status)
}

/// Map a `git log --format=%G?` status code to a `Result`.
///
/// Status codes per `git-log(1)`:
/// - `G`: good (valid) signature
/// - `U`: good signature with unknown validity (untrusted key)
/// - `N`: no signature
/// - `B`: bad signature
/// - `E`: signature cannot be checked
/// - `X`: good signature that has expired
/// - `Y`: good signature made by an expired key
/// - `R`: good signature made by a revoked key
///
/// `G` and `U` are accepted (cfgd treats untrusted-key as "key not in keyring
/// yet" rather than a hard failure). Anything else is a verification failure.
pub(super) fn classify_signature_status(name: &str, status: &str) -> Result<()> {
    match status {
        "G" | "U" => {
            tracing::debug!(
                source = %name,
                "Source '{}' HEAD commit signature verified (status: {})",
                name, status
            );
            Ok(())
        }
        "N" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit is not signed — source requires signed commits".into(),
        }
        .into()),
        "B" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit has a bad (invalid) signature".into(),
        }
        .into()),
        "E" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "signature cannot be checked — ensure the signing key is imported".into(),
        }
        .into()),
        "X" | "Y" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit signature or signing key has expired".into(),
        }
        .into()),
        "R" => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: "HEAD commit was signed with a revoked key".into(),
        }
        .into()),
        other => Err(SourceError::SignatureVerificationFailed {
            name: name.to_string(),
            message: format!("unexpected signature status '{}' from git", other),
        }
        .into()),
    }
}

/// Normalize shorthand semver pins.
/// `~2` means "any 2.x.x" (maps to `>=2.0.0, <3.0.0`, i.e., caret semantics).
/// `~2.1` means "any 2.1.x" (maps to `~2.1.0`, tilde semantics).
/// `~2.1.0` is passed through directly.
/// `^N` uses caret semantics as-is.
fn normalize_semver_pin(pin: &str) -> String {
    let trimmed = pin.trim();

    if let Some(rest) = trimmed.strip_prefix('~') {
        let dots = rest.matches('.').count();
        match dots {
            // ~2 -> ^2.0.0 (user means "any v2")
            0 => format!("^{}.0.0", rest),
            // ~2.1 -> ~2.1.0 (user means "any 2.1.x")
            1 => format!("~{}.0", rest),
            _ => trimmed.to_string(),
        }
    } else if let Some(rest) = trimmed.strip_prefix('^') {
        let dots = rest.matches('.').count();
        match dots {
            0 => format!("^{}.0.0", rest),
            1 => format!("^{}.0", rest),
            _ => trimmed.to_string(),
        }
    } else {
        trimmed.to_string()
    }
}

/// A `pinVersion` value resolved to a concrete git ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedRef {
    /// A git tag and the highest semver it parsed to (for diagnostics/sorting).
    Tag { tag: String, version: Version },
    /// A commit SHA (7–40 hex chars).
    Commit(String),
}

/// A `(sha, tag_name)` pair from `git ls-remote --tags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteTag {
    pub sha: String,
    pub name: String,
}

/// List the remote's tags via `git ls-remote --tags`, parsed into `RemoteTag`s.
/// Peeled-tag lines (`refs/tags/<name>^{}`) are ignored so annotated tags are
/// not double-counted. Bounded by [`crate::GIT_NETWORK_TIMEOUT`].
pub(super) fn list_remote_tags(name: &str, origin: &OriginSpec) -> Result<Vec<RemoteTag>> {
    let mut cmd = crate::git_cmd_safe(Some(&origin.url), Some(origin.ssh_strict_host_key_checking));
    cmd.args(["ls-remote", "--tags", "--end-of-options", &origin.url]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output =
        crate::command_output_with_timeout(&mut cmd, crate::GIT_NETWORK_TIMEOUT).map_err(|e| {
            SourceError::GitError {
                name: name.to_string(),
                message: format!("ls-remote failed: {}", e),
            }
        })?;
    if !output.status.success() {
        return Err(SourceError::GitError {
            name: name.to_string(),
            message: format!(
                "ls-remote --tags failed: {}",
                crate::stderr_lossy_trimmed(&output)
            ),
        }
        .into());
    }

    Ok(parse_ls_remote_tags(&crate::stdout_lossy_trimmed(&output)))
}

/// Parse `git ls-remote --tags` stdout into `(sha, name)` pairs, dropping the
/// `^{}` peeled-tag lines so annotated tags appear once.
pub(super) fn parse_ls_remote_tags(stdout: &str) -> Vec<RemoteTag> {
    let mut tags = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split('\t');
        let (Some(sha), Some(refname)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(name) = refname.strip_prefix("refs/tags/") else {
            continue;
        };
        if name.ends_with("^{}") {
            continue;
        }
        tags.push(RemoteTag {
            sha: sha.trim().to_string(),
            name: name.to_string(),
        });
    }
    tags
}

/// Resolve a pin string against a tag list (the pure, network-free core).
///
/// Disambiguation order: a value that parses as a semver `VersionReq` (after
/// [`normalize_semver_pin`]) is treated as a RANGE over tags; otherwise a 7–40
/// hex value is a commit SHA; otherwise it is an exact tag name. No match for a
/// range/tag yields [`SourceError::PinRefNotFound`].
pub(super) fn resolve_ref_from_tags(
    name: &str,
    pin: &str,
    tags: &[RemoteTag],
) -> Result<ResolvedRef> {
    let trimmed = pin.trim();

    // Argument-injection guard: a `-`-leading pin (and, below, any `-`-leading
    // resolved tag a malicious source might publish) would be parsed as a git
    // flag by `git checkout`/`git fetch`. Reject before resolution; the git
    // call sites additionally pass `--end-of-options` as defense in depth.
    if trimmed.starts_with('-') {
        return Err(SourceError::PinRefNotFound {
            name: name.to_string(),
            pin: pin.to_string(),
            available: available_tags_hint(tags),
        }
        .into());
    }
    let pin_not_found = || -> crate::errors::CfgdError {
        SourceError::PinRefNotFound {
            name: name.to_string(),
            pin: pin.to_string(),
            available: available_tags_hint(tags),
        }
        .into()
    };

    // A bare, fully-specified `X.Y.Z` (no operator/shorthand) reads as "pin
    // exactly this version" — but the `semver` crate parses `2.0.0` with caret
    // semantics (matching 2.1.0 too). Rewrite it to `=X.Y.Z` so a full version
    // selects that tag, not a higher one. Shorthand (`~2`, `^1`) and explicit
    // operators (`>=1.0`, `=2.0.0`) keep their range semantics.
    let normalized = match parse_bare_full_version(trimmed) {
        Some(exact) => exact,
        None => normalize_semver_pin(trimmed),
    };
    // VersionReq is tried first by design: an all-numeric value like `2` parses
    // as a range (`^2`), not a commit SHA. This is intentional — a hex SHA that
    // is also a valid bare version (all-decimal, ≤3 dotted parts) is vanishingly
    // rare, and treating versions as ranges is the documented disambiguation.
    if let Ok(req) = VersionReq::parse(&normalized) {
        let mut best: Option<(Version, String)> = None;
        for tag in tags {
            let stripped = tag.name.strip_prefix('v').unwrap_or(&tag.name);
            let Ok(version) = Version::parse(stripped) else {
                continue;
            };
            if req.matches(&version) && best.as_ref().is_none_or(|(b, _)| version > *b) {
                best = Some((version, tag.name.clone()));
            }
        }
        return match best {
            // A semver tag cannot be `-`-leading (it would not parse), but guard
            // anyway so no `-`-prefixed name ever reaches a git positional.
            Some((_, tag)) if tag.starts_with('-') => Err(pin_not_found()),
            Some((version, tag)) => Ok(ResolvedRef::Tag { tag, version }),
            None => Err(pin_not_found()),
        };
    }

    if is_commit_sha(trimmed) {
        return Ok(ResolvedRef::Commit(trimmed.to_lowercase()));
    }

    if tags.iter().any(|t| t.name == trimmed) {
        // Re-parse to populate the version field; fall back to 0.0.0 for a
        // non-semver tag name selected verbatim.
        let version = Version::parse(trimmed.strip_prefix('v').unwrap_or(trimmed))
            .unwrap_or_else(|_| Version::new(0, 0, 0));
        return Ok(ResolvedRef::Tag {
            tag: trimmed.to_string(),
            version,
        });
    }

    Err(pin_not_found())
}

/// If `pin` is a bare, fully-specified `X.Y.Z` semver with no operator or
/// shorthand prefix, return the exact-match form `=X.Y.Z`; otherwise `None`.
fn parse_bare_full_version(pin: &str) -> Option<String> {
    let trimmed = pin.trim();
    if trimmed.starts_with(['~', '^', '=', '>', '<', '*']) {
        return None;
    }
    Version::parse(trimmed).ok().map(|_| format!("={trimmed}"))
}

/// True for a 7–40 char lowercase/uppercase hex string (a git commit SHA).
fn is_commit_sha(s: &str) -> bool {
    let s = s.trim();
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Bounded, comma-joined list of available tag names for error hints (max 10).
/// Sorted semver-descending (newest first) so a mis-pin error surfaces the most
/// relevant tags; non-semver names sort after all parseable versions, by name.
fn available_tags_hint(tags: &[RemoteTag]) -> Option<String> {
    if tags.is_empty() {
        return None;
    }
    let mut sorted: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    sorted.sort_by(|a, b| {
        let va = Version::parse(a.strip_prefix('v').unwrap_or(a)).ok();
        let vb = Version::parse(b.strip_prefix('v').unwrap_or(b)).ok();
        match (va, vb) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });
    let shown_len = sorted.len().min(10);
    let mut hint = sorted[..shown_len].join(", ");
    if sorted.len() > shown_len {
        hint.push_str(", …");
    }
    Some(hint)
}

/// Clone a git repo with git CLI (with live progress), falling back to libgit2.
///
/// The destination must be absent or empty. Returns Ok(()) on success, Err with
/// description on failure.
/// Whether a clone URL resolves through git's local transport — a `file://` URL
/// or a plain filesystem path. Anything carrying a `scheme://` other than `file`,
/// or an `scp`-style `user@host:path`, is remote.
fn is_local_git_url(url: &str) -> bool {
    if let Some(rest) = url.split_once("://") {
        return rest.0 == "file";
    }
    // `user@host:path` is scp syntax; a bare path may still contain '@'.
    !url.contains(':') || Path::new(url).exists()
}

pub fn git_clone_with_fallback(
    url: &str,
    target: &Path,
    printer: &Printer,
) -> std::result::Result<(), String> {
    // Establishing an empty destination up front is what makes the cleanup
    // between the two clone attempts safe: everything under `target` at that
    // point was put there by the attempt being undone. Without this gate, a CLI
    // clone that failed *because* the destination was populated took the
    // populated content with it — `cfgd init --from <repo>` re-run against an
    // already-initialized `~/.config/cfgd` deleted the user's cfgd.yaml,
    // profiles/, and files/ on its way to reporting the clone failure.
    let destination_is_empty = match std::fs::read_dir(target) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        // Undecidable emptiness is treated as non-empty: refusing costs a
        // clone, deleting costs the directory's contents.
        Err(e) => {
            return Err(format!(
                // native-ok: human-facing error message
                "Cannot inspect clone destination {}: {e}",
                target.display()
            ));
        }
    };
    if !destination_is_empty {
        return Err(format!(
            // native-ok: human-facing error message
            "Refusing to clone {url} into {}: directory is not empty",
            target.display()
        ));
    }

    // Try git CLI first with live progress output.
    let mut cmd = crate::git_cmd_safe(Some(url), None);
    cmd.args([
        "clone",
        "--depth=1",
        "--no-recurse-submodules",
        "--end-of-options",
        url,
        &target.display().to_string(),
    ]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let label = format!("Cloning {}", url);
    let cli_result = printer.run(&mut cmd, &label);
    if matches!(&cli_result, Ok(output) if output.status.success()) {
        return Ok(());
    }
    // When the libgit2 retry also fails it reports its own, usually less
    // specific, error (a local path reads as "shallow fetch is not supported by
    // the local transport" whatever went wrong with the CLI). Carry the CLI's
    // reason into the final message so the actual cause is not lost.
    let cli_failure = match &cli_result {
        Ok(output) => format!(
            "git CLI {}: {}",
            crate::exit_status_reason(&output.status),
            output.stderr.trim()
        ),
        Err(e) => format!("git CLI unavailable: {e}"),
    };

    // Clean up partial clone before libgit2 retry
    let _ = std::fs::remove_dir_all(target);
    let _ = std::fs::create_dir_all(target);

    // Fall back to libgit2 with spinner
    let spinner = printer.spinner("Cloning (libgit2)...");

    let mut fetch_opts = git2::FetchOptions::new();
    // libgit2 rejects a shallow fetch over the local transport outright ("shallow
    // fetch is not supported by the local transport"), so asking for depth=1 on a
    // `file://` or bare-path URL turns the fallback into a guaranteed failure —
    // `cfgd init --from /path/to/repo.git` cannot clone at all on a host without
    // the git CLI. Depth is a transfer-size guard for remotes; a local clone has
    // no transfer to bound.
    if !is_local_git_url(url) {
        fetch_opts.depth(1);
    }
    if url.starts_with("git@") || url.starts_with("ssh://") {
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::git_ssh_credentials);
        fetch_opts.remote_callbacks(callbacks);
    }
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch_opts);

    let result = builder
        .clone(url, target)
        .map(|_| ())
        .map_err(|e| format!("Failed to clone {}: {} [{}]", url, e, cli_failure));

    match &result {
        Ok(_) => {
            let _ = spinner.finish_ok(format!("Cloned {} (libgit2)", url));
        }
        Err(msg) => {
            let _ = spinner
                .finish_fail(format!("Failed to clone {} (libgit2)", url))
                .detail(crate::output::collapse_to_subject_line(msg));
        }
    }
    result
}

#[cfg(test)]
mod tests;
