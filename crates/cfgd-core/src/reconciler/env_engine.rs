//! Shared env-target engine: given the merged env vars, aliases, and an
//! [`EnvScope`], compute the ordered set of targets the planner writes and the
//! verifier re-derives. Keeping both paths on one function is what guarantees
//! `cfgd apply` and `cfgd status`/`verify` agree on the target set (otherwise a
//! newly-written file reports as permanent false drift).
//!
//! Target computation is pure — `$SHELL`, fish presence, and which login
//! dotfiles already exist are captured once into an [`EnvHostProbe`] at the
//! caller boundary, so the matrix is unit-testable without mutating
//! process-global state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{EnvScope, EnvVar, ShellAlias};

use super::env_files::{
    ENV_FILE_HEADER, fish_in_use, generate_env_file_content, generate_fish_env_content,
    generate_powershell_env_content,
};

/// Source line shells evaluate to load the cfgd-managed env file. Uses the
/// POSIX `.` builtin, not the `source` alias: `.profile` is read by `/bin/sh`
/// (dash on Debian, the base `sh` on FreeBSD), which has no `source` — the alias
/// exists only in bash/zsh/csh. `.` is equivalent in bash and zsh, so one line
/// loads correctly across every shell cfgd injects into.
const UNIX_SOURCE_LINE: &str = "[ -f ~/.cfgd.env ] && . ~/.cfgd.env";
const PS_SOURCE_LINE: &str = ". ~/.cfgd-env.ps1";

/// LaunchAgent label for the *user-scope* (`spec.env`) plist. Deliberately
/// distinct from the system configurator's `com.cfgd.environment` so the two
/// never collide.
const MACOS_USER_PLIST_LABEL: &str = "com.cfgd.user-environment";
const MACOS_USER_PLIST_NAME: &str = "com.cfgd.user-environment.plist";

/// Where a managed env value ends up. The planner turns these into
/// [`super::types::EnvAction`]; the verifier checks the file variants exist
/// with the expected content.
pub(super) enum EnvTarget {
    /// A standalone cfgd-owned file — safe to overwrite wholesale.
    ///
    /// `rendered` counts what went into THIS file, not what the run merged:
    /// the systemd `environment.d` and launchd renderings carry env vars
    /// only, so a write line quoting the merged alias count would name
    /// aliases the file does not hold.
    ManagedFile {
        path: PathBuf,
        content: String,
        rendered: RenderedCounts,
    },
    /// An idempotent source-line appended into a user-owned dotfile.
    SourceLine { rc_path: PathBuf, line: String },
    /// A live-session refresh (no file; not a verified-drift surface).
    LiveSession { vars: Vec<(String, String)> },
}

/// Target operating-system family for env target selection. Injected so tests
/// exercise every platform's matrix regardless of the host running the suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnvPlatform {
    Linux,
    MacOs,
    FreeBsd,
    Windows,
}

impl EnvPlatform {
    pub(super) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "freebsd") {
            Self::FreeBsd
        } else {
            Self::Linux
        }
    }
}

/// Host facts that affect env target selection, captured once at the caller
/// boundary so [`env_targets`] stays pure.
pub(super) struct EnvHostProbe {
    /// The user's login shell (`$SHELL`), used to pick the interactive rc file.
    pub shell: String,
    /// Whether a managed fish env file should be written (fish in use *and* its
    /// `conf.d` directory exists).
    pub fish_present: bool,
    /// Whether `~/.bash_profile` already exists (never created here — doing so
    /// would shadow a user's `~/.profile` in bash's first-match login chain).
    pub bash_profile_exists: bool,
    /// Whether `~/.bash_login` already exists.
    pub bash_login_exists: bool,
    /// Whether a POSIX `sh` (Git Bash) is on PATH — Windows-only relevance.
    pub git_bash_present: bool,
    /// Whether zsh is actually in use on this host — the login shell is zsh, a
    /// `zsh` binary is on PATH, or the user already keeps a `~/.zshrc`. Gates
    /// `~/.zshenv`: a bash-only host must not gain an inert cfgd-owned login file.
    pub zsh_present: bool,
}

impl EnvHostProbe {
    pub(super) fn detect(home: &Path) -> Self {
        #[cfg(any(test, feature = "test-helpers"))]
        if let Some(o) = TEST_HOST_PROBE_OVERRIDE.with(|cell| cell.borrow().clone()) {
            return Self {
                shell: o.shell,
                fish_present: o.fish_present,
                bash_profile_exists: o.bash_profile_exists,
                bash_login_exists: o.bash_login_exists,
                git_bash_present: o.git_bash_present,
                zsh_present: o.zsh_present,
            };
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let fish_conf_d = home.join(".config/fish/conf.d");
        let zsh_present = shell.contains("zsh")
            || crate::command_available("zsh")
            || home.join(".zshrc").exists();
        Self {
            shell,
            fish_present: fish_in_use() && fish_conf_d.exists(),
            bash_profile_exists: home.join(".bash_profile").exists(),
            bash_login_exists: home.join(".bash_login").exists(),
            git_bash_present: cfg!(windows) && crate::command_available("sh"),
            zsh_present,
        }
    }
}

// `EnvHostProbe::detect` reads `$SHELL`, `command_available("zsh"/"fish")`,
// and PATH — none of which `with_test_home_guard` isolates, so an
// integration-style test driving a real `cmd_apply`/`cmd_verify` gets
// whatever shell shape the CI runner happens to have. This is the same
// thread-local override idiom as `with_test_home`
// (`cfgd-core/src/util/paths.rs`): a test pins a declared host shape for the
// duration of a guard instead of asserting against ambient reality.
#[cfg(any(test, feature = "test-helpers"))]
thread_local! {
    static TEST_HOST_PROBE_OVERRIDE: std::cell::RefCell<Option<EnvHostProbeOverride>> =
        const { std::cell::RefCell::new(None) };
}

/// A declared host shape for `EnvHostProbe::detect` to return verbatim
/// instead of reading `$SHELL`/PATH/`~`. Field-for-field mirror of
/// `EnvHostProbe` rather than a re-export of it: `EnvHostProbe` stays
/// `pub(super)` (internal to the reconciler), while this override is the
/// crate's public test seam.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone)]
pub struct EnvHostProbeOverride {
    pub shell: String,
    pub fish_present: bool,
    pub bash_profile_exists: bool,
    pub bash_login_exists: bool,
    pub git_bash_present: bool,
    pub zsh_present: bool,
}

/// RAII guard restoring the prior override on drop, mirroring
/// [`crate::TestHomeGuard`].
#[cfg(any(test, feature = "test-helpers"))]
#[must_use = "dropping the guard immediately restores the previous override"]
pub struct EnvHostProbeOverrideGuard {
    prev: Option<EnvHostProbeOverride>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl Drop for EnvHostProbeOverrideGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        TEST_HOST_PROBE_OVERRIDE.with(|cell| *cell.borrow_mut() = prev);
    }
}

/// Install an `EnvHostProbe::detect` override for the current thread and
/// return a guard that restores the prior value (including `None`) on drop.
#[cfg(any(test, feature = "test-helpers"))]
pub fn with_env_host_probe_override_guard(
    probe: EnvHostProbeOverride,
) -> EnvHostProbeOverrideGuard {
    let prev = TEST_HOST_PROBE_OVERRIDE.with(|cell| cell.replace(Some(probe)));
    EnvHostProbeOverrideGuard { prev }
}

fn reaches_login(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::Login | EnvScope::All)
}

fn reaches_all(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::All)
}

/// Compute the ordered list of env targets for a scope. Empty input yields no
/// targets.
///
/// `path_dirs` carries the PATH entries of the package managers the desired
/// state names, so a profile that only bootstraps a manager still gets a
/// managed file *and* the rc source lines that make it reachable.
pub(super) fn env_targets(
    content: EnvContent<'_>,
    scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    platform: EnvPlatform,
) -> Vec<EnvTarget> {
    let mut targets = Vec::new();
    if content.env.is_empty() && content.aliases.is_empty() && content.path_dirs.is_empty() {
        return targets;
    }

    match platform {
        EnvPlatform::Windows => windows_targets(content, home, probe, &mut targets),
        EnvPlatform::Linux | EnvPlatform::MacOs | EnvPlatform::FreeBsd => {
            unix_targets(content, scope, home, probe, platform, &mut targets)
        }
    }

    // Live-session refresh runs last, after the durable files are written.
    if reaches_all(scope) {
        let vars = valid_export_pairs(content.env);
        if !vars.is_empty() {
            targets.push(EnvTarget::LiveSession { vars });
        }
    }

    targets
}

/// The desired-state inputs every generated shell file is derived from,
/// bundled so the per-platform target builders take one parameter instead of
/// parallel slices that must stay in the same order at each call site.
#[derive(Clone, Copy)]
pub(super) struct EnvContent<'a> {
    env: &'a [EnvVar],
    aliases: &'a [ShellAlias],
    path_dirs: &'a [ManagerPathDir],
    origins: &'a EnvOrigins,
}

impl<'a> EnvContent<'a> {
    pub(super) fn new(
        env: &'a [EnvVar],
        aliases: &'a [ShellAlias],
        path_dirs: &'a [ManagerPathDir],
        origins: &'a EnvOrigins,
    ) -> Self {
        Self {
            env,
            aliases,
            path_dirs,
            origins,
        }
    }
}

/// Which layer each merged env var and alias came from, for the provenance
/// comment the generated shell files carry beside the line it explains.
///
/// Names only — a value is whatever survived the merge, and the comment says
/// who put it there. EVERY entry names its layer: the file is the merge of N
/// layers (profile chain, subscribed sources, modules) and so has no default
/// owner a reader could assume, which is what an unannotated line would ask
/// them to do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct EnvOrigins(crate::config::EntryOwners);

impl EnvOrigins {
    /// Seed from the profile-layer merge's own record, so the layer that
    /// declared a surviving value owns it here too.
    pub(super) fn from_owners(owners: &crate::config::EntryOwners) -> Self {
        Self(owners.clone())
    }

    /// Record `module` as the owner of every entry it declares, overwriting an
    /// earlier claim exactly as the merge overwrites its value.
    pub(super) fn claim_module_entries(&mut self, module: &crate::modules::ResolvedModule) {
        self.0.claim(
            &crate::reconciler::Owner::module(&module.name).token(),
            &module.env,
            &module.aliases,
        );
    }

    /// The trailing ` # <kind>:<name>` an env-var line carries, or an empty
    /// string when nothing owns it.
    pub(super) fn env_comment(&self, name: &str) -> String {
        comment(self.0.env.get(name).map(String::as_str))
    }

    /// The owner token of an env var, unwrapped — for the one line whose
    /// comment names TWO producers and so cannot be composed from a rendered
    /// comment.
    fn env_owner(&self, name: &str) -> Option<&str> {
        self.0.env.get(name).map(String::as_str)
    }

    /// The same for an alias line.
    pub(super) fn alias_comment(&self, name: &str) -> String {
        comment(self.0.aliases.get(name).map(String::as_str))
    }
}

/// The manager half of the PATH line's owner comment: the managers whose
/// directories the line publishes, in dir order and deduped
/// (`manager:brew,cargo`). One token for the whole line, because the line is
/// one assignment; `None` when nothing named a manager.
///
/// This vocabulary is deliberately WIDER than [`super::OwnerKind`]'s: the name
/// half is a comma list, which no `Owner` name may hold, so reading it back
/// through `OwnerKind::from_token("manager")` stays `None` on purpose rather
/// than minting an owner that names two things at once.
fn path_dirs_manager_token(dirs: &[ManagerPathDir]) -> Option<String> {
    let mut managers: Vec<&str> = Vec::new();
    for dir in dirs {
        if !dir.manager.is_empty() && !managers.contains(&dir.manager.as_str()) {
            managers.push(&dir.manager);
        }
    }
    (!managers.is_empty()).then(|| format!("manager:{}", managers.join(",")))
}

/// Render an owner token as a trailing shell comment.
///
/// A comment is appended OUTSIDE the quoted token every dialect's quoting
/// helper produced, so it annotates the assignment rather than joining the
/// value. An owner carrying anything but the token's own alphabet is dropped
/// rather than escaped: a `\n` would end the assignment and stand the
/// remainder up as further shell, and there is nothing a comment is worth
/// risking that for. `,` is in the alphabet because the PATH line's comment
/// names every manager that contributed to it, and ` ` because that same line
/// is the one assignment with TWO producers to name (`manager:brew,npm
/// module:nvim`) — neither character can end a shell line.
fn comment(owner: Option<&str>) -> String {
    match owner {
        Some(owner)
            if owner.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | ',' | ' ')
            }) =>
        {
            format!(" # {owner}")
        }
        _ => String::new(),
    }
}

/// A generated line with its trailing owner comment removed, if it has one.
///
/// The exact inverse of [`comment`] and kept beside it so the two cannot
/// drift. Any comparison of a DEPLOYED line against a freshly rendered one
/// folds both sides through this: a file written before owner comments
/// existed carries none, and comparing raw would read every line in such a
/// file as having moved. Applied symmetrically, so a value that itself ends
/// in something comment-shaped folds the same way on both sides and still
/// compares equal to itself.
pub(super) fn without_owner_comment(line: &str) -> &str {
    match line.rsplit_once(" # ") {
        Some((head, tail)) if !tail.is_empty() && comment(Some(tail)) == format!(" # {tail}") => {
            head
        }
        _ => line,
    }
}

/// One PATH directory cfgd published, remembering the manager that created it.
///
/// The manager travels WITH the directory because the generated line names it:
/// a bare `Vec<String>` had already lost which manager each entry belonged to
/// by the time the env file was rendered, so the one line in the file that is
/// cfgd's own bookkeeping was the only line that could not say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerPathDir {
    pub manager: String,
    pub dir: String,
}

impl ManagerPathDir {
    pub fn new(manager: impl Into<String>, dir: impl Into<String>) -> Self {
        Self {
            manager: manager.into(),
            dir: dir.into(),
        }
    }

    /// A directory no manager claims — a fold over it carries no owner
    /// comment, which is what a caller asserting on the ASSIGNMENT rather than
    /// on its provenance wants.
    pub fn unowned(dir: impl Into<String>) -> Self {
        Self {
            manager: String::new(),
            dir: dir.into(),
        }
    }
}

/// How the home directory is spelled inside a generated file's derived PATH
/// line. `$HOME` is live in every dialect whose PATH line interpolates
/// (bash/zsh double quotes, PowerShell double quotes); fish single-quotes each
/// entry to suppress expansion, so there the literal path is the only spelling
/// that resolves.
const HOME_TOKEN: &str = "$HOME";

/// The PATH separator a declared `PATH` value uses on `platform`.
fn path_separator(platform: EnvPlatform) -> char {
    match platform {
        EnvPlatform::Windows => ';',
        _ => ':',
    }
}

/// The manager PATH directories the generated line still has to publish, in
/// the spelling that file uses for the home directory.
///
/// One file, two producers of one variable: this derived line and whatever the
/// merged `PATH` env var declares. A directory both name lands on PATH twice,
/// so the derived line — cfgd's own bookkeeping — yields to the declaration.
/// The match is on [`crate::normalize_path_entry`], because `$HOME/.cargo/bin`
/// and `/home/tj/.cargo/bin` are one directory written two ways.
///
/// `home_token` is the spelling the surviving entries render in, so a file
/// never mixes a literal home with a `$HOME` on two lines of one artifact.
fn effective_path_dirs(
    path_dirs: &[ManagerPathDir],
    env: &[EnvVar],
    home: &Path,
    platform: EnvPlatform,
    home_token: Option<&str>,
) -> Vec<ManagerPathDir> {
    let separator = path_separator(platform);
    let declared: std::collections::HashSet<String> = env
        .iter()
        .filter(|e| e.name == "PATH")
        .flat_map(|e| e.value.split(separator))
        .map(|entry| crate::normalize_path_entry(entry, home))
        .collect();
    let home_str = crate::to_posix_string(home);
    let home_str = home_str.trim_end_matches('/');
    path_dirs
        .iter()
        .filter(|d| !declared.contains(&crate::normalize_path_entry(&d.dir, home)))
        .map(|d| {
            let dir = match home_token {
                Some(token) => home_spelled(&d.dir, home_str, token),
                None => d.dir.clone(),
            };
            ManagerPathDir::new(d.manager.clone(), dir)
        })
        .collect()
}

/// `dir` with a leading `home` replaced by `token`, or `dir` unchanged when it
/// lies outside the home directory. The prefix must end at a segment boundary,
/// so `/home/tjs` is not read as `$HOME` plus an `s`.
fn home_spelled(dir: &str, home: &str, token: &str) -> String {
    if home.is_empty() {
        return dir.to_string();
    }
    let posix = crate::posixify_text(dir);
    match posix.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{token}{rest}"),
        _ => dir.to_string(),
    }
}

/// One entry of the ONE `PATH` assignment a generated file carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PathPart {
    /// A directory, already spelled the way this file spells the home
    /// directory, and not yet quoted — quoting is the dialect's.
    Dir(String),
    /// The `PATH` the shell already had, spliced in where the declaration put
    /// it. Rendered as each dialect's own spelling of that reference, never as
    /// the literal characters a declaration happened to use.
    Inherited,
}

/// The single `PATH` assignment a generated file carries, whichever of its two
/// producers contributed to it.
///
/// One variable, one assignment: cfgd's bootstrapped-manager directories and a
/// declared `spec.env` `PATH` used to render as two `PATH` assignment lines
/// (each dialect's own syntax) in one file, so the file assigned one variable
/// twice and every count over it had to choose between naming lines and
/// naming variables — the write said `4 vars` one row above a session publish
/// saying `3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FoldedPath {
    parts: Vec<PathPart>,
    /// The trailing owner comment, naming BOTH producers when both contributed
    /// (` # manager:brew,npm module:nvim`).
    pub(super) comment: String,
}

impl FoldedPath {
    /// A fold of literal directories and nothing else — no inherited PATH and
    /// no owner comment. The sentinel render behind
    /// `super::env_files::path_dirs_line_prefix`, whose whole job is the text
    /// BEFORE the value.
    pub(super) fn literal(dirs: impl IntoIterator<Item = String>) -> Self {
        Self {
            parts: dirs.into_iter().map(PathPart::Dir).collect(),
            comment: String::new(),
        }
    }

    /// A fold of derived directories ahead of the inherited `PATH` and nothing
    /// else — the shape the line takes when no layer declares `PATH` at all.
    pub(super) fn derived(dirs: &[ManagerPathDir]) -> Self {
        let mut parts: Vec<PathPart> = dirs.iter().map(|d| PathPart::Dir(d.dir.clone())).collect();
        parts.push(PathPart::Inherited);
        Self {
            parts,
            comment: comment(path_dirs_manager_token(dirs).as_deref()),
        }
    }

    /// The assignment's value, rendered in one dialect: `quote` is that
    /// dialect's complete quoting of a single directory, `inherited` its
    /// spelling of the ambient `PATH`, and `separator` what joins entries.
    ///
    /// The fold decides WHICH entries and in what order; a dialect decides only
    /// how they are written, which is why this is a renderer rather than a
    /// string the fold hands out.
    pub(super) fn value(
        &self,
        quote: impl Fn(&str) -> String,
        inherited: &str,
        separator: &str,
    ) -> String {
        self.parts
            .iter()
            .map(|part| match part {
                PathPart::Dir(dir) => quote(dir),
                PathPart::Inherited => inherited.to_string(),
            })
            .collect::<Vec<_>>()
            .join(separator)
    }
}

/// The ONE derivation of a generated file's `PATH` line, over both producers.
///
/// Declared entries come first, in declared order, because they are the user's
/// stated precedence; the surviving derived directories are spliced in where
/// the declaration reaches for the ambient `PATH`, so cfgd's own bookkeeping
/// stays ahead of whatever the shell already had — the ordering the two
/// separate lines used to produce. A declaration naming no ambient `PATH` at
/// all is taken at its word and the derived directories follow its entries.
///
/// `home_token` is the file's spelling of the home directory, applied to every
/// entry on the line whatever produced it: a fish file single-quotes, which
/// suppresses `$HOME`, so a declared `$HOME/.cargo/bin` beside a literal
/// derived directory would be one broken entry on an otherwise working line.
///
/// `None` when neither producer has anything to say.
pub(super) fn fold_path_line(
    env: &[EnvVar],
    path_dirs: &[ManagerPathDir],
    origins: &EnvOrigins,
    home: &Path,
    platform: EnvPlatform,
    home_token: Option<&str>,
) -> Option<FoldedPath> {
    let derived = effective_path_dirs(path_dirs, env, home, platform, home_token);
    let declared = env
        .iter()
        .rfind(|e| e.name == "PATH" && crate::validate_env_var_name(&e.name).is_ok());
    if derived.is_empty() && declared.is_none() {
        return None;
    }

    let home_str = crate::to_posix_string(home);
    let home_str = home_str.trim_end_matches('/');
    let spell = |dir: &str| {
        let expanded = crate::expand_env_value_tilde(dir);
        match home_token {
            // A DECLARED entry keeps the spelling it was written with: this
            // line is the only place the user's own text appears, and a
            // dialect that interpolates resolves it either way.
            Some(_) => expanded,
            // No token means the dialect cannot expand a reference at all, so
            // the literal path is the only spelling that resolves there — a
            // declared `$HOME/...` beside a literal derived directory would
            // otherwise be the one dead entry on a working line.
            None => home_spelled_literal(&expanded, home_str),
        }
    };

    let Some(declared) = declared else {
        return Some(FoldedPath::derived(&derived));
    };

    let mut parts = Vec::new();
    let mut spliced = false;
    for segment in declared.value.split(path_separator(platform)) {
        if segment.is_empty() {
            continue;
        }
        if crate::is_inherited_path_ref(segment) {
            parts.extend(derived.iter().map(|d| PathPart::Dir(d.dir.clone())));
            spliced = true;
            parts.push(PathPart::Inherited);
        } else {
            parts.push(PathPart::Dir(spell(segment)));
        }
    }
    if !spliced {
        parts.extend(derived.iter().map(|d| PathPart::Dir(d.dir.clone())));
    }

    let token = [
        path_dirs_manager_token(&derived),
        origins.env_owner("PATH").map(str::to_string),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    Some(FoldedPath {
        parts,
        comment: comment((!token.is_empty()).then_some(token.as_str())),
    })
}

/// The primary managed file's PATH fold — the spelling BOTH platform target
/// builders use for it, named once so the write and the per-entry comparison
/// that reads it back cannot disagree about what that file's PATH line says.
pub(super) fn primary_folded_path(
    env: &[EnvVar],
    path_dirs: &[ManagerPathDir],
    origins: &EnvOrigins,
    home: &Path,
    platform: EnvPlatform,
) -> Option<FoldedPath> {
    fold_path_line(env, path_dirs, origins, home, platform, Some(HOME_TOKEN))
}

/// The inverse of [`home_spelled`]: `dir` with a leading `$HOME` / `${HOME}`
/// replaced by the literal home directory, for the dialect that quotes each
/// entry so hard that a reference never expands.
fn home_spelled_literal(dir: &str, home: &str) -> String {
    if home.is_empty() {
        return dir.to_string();
    }
    for token in ["${HOME}", "$HOME"] {
        if let Some(rest) = dir.strip_prefix(token)
            && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
        {
            return format!("{home}{rest}");
        }
    }
    dir.to_string()
}

/// The primary managed env file for `platform` — the first `ManagedFile`
/// [`env_targets`] pushes, and the only one the per-item checks read. Named
/// once here because a display surface reads that file back to show what a
/// drifted item ACTUALLY holds, and a second spelling of the path would let
/// the reader and the writer disagree about which file that is.
pub(super) fn primary_env_file_path(home: &Path, platform: EnvPlatform) -> PathBuf {
    match platform {
        EnvPlatform::Windows => home.join(".cfgd-env.ps1"),
        EnvPlatform::Linux | EnvPlatform::MacOs | EnvPlatform::FreeBsd => home.join(".cfgd.env"),
    }
}

/// The verb a recorded `env` resource id was applied under, reconstructed
/// from the id alone: `write` for a file this engine generates whole, `inject`
/// for a user-owned rc file it plants a source line in.
///
/// The recorded id keeps the target path and drops the `env:write:` /
/// `env:inject:` verb (see `parse_resource_from_description`), so a display
/// surface wanting the method back answers it here, from the one fact that
/// survives: every file the engine writes whole carries a cfgd-authored name,
/// and every inject target is a shell's own rc file. The name list lives
/// beside the target builders that spell those paths, and
/// `every_env_target_classifies_under_the_verb_that_produced_it` walks the
/// builders' real output so a sixth generated file cannot land as `inject`.
///
/// Not for the live-session id (`refresh`), which names an act rather than a
/// file — its caller holds that row back before asking.
pub fn recorded_env_method(resource_id: &str) -> &'static str {
    let name = resource_id.rsplit('/').next().unwrap_or(resource_id);
    match name {
        ".cfgd.env" | ".cfgd-env.ps1" | "cfgd-env.fish" | "cfgd.conf" | MACOS_USER_PLIST_NAME => {
            ENV_VERB_WRITE
        }
        _ => ENV_VERB_INJECT,
    }
}

/// The two verbs the env engine's actions go by, on every surface that names
/// one: the `env:write:`/`env:inject:` resource ids, the plan bullets and the
/// status table's Method cells. One spelling here keeps a rename from leaving
/// any of the three silently stale against the other two.
pub const ENV_VERB_WRITE: &str = "write";
pub const ENV_VERB_INJECT: &str = "inject";

fn unix_targets(
    content: EnvContent<'_>,
    scope: EnvScope,
    home: &Path,
    probe: &EnvHostProbe,
    platform: EnvPlatform,
    out: &mut Vec<EnvTarget>,
) {
    let EnvContent {
        env,
        aliases,
        path_dirs,
        origins,
    } = content;
    // Interactive (all scopes): the cfgd-owned env file + a source line in the
    // user's interactive rc, plus fish when it's in use.
    let posix_path = primary_folded_path(env, path_dirs, origins, home, platform);
    out.push(EnvTarget::ManagedFile {
        path: primary_env_file_path(home, platform),
        content: generate_env_file_content(env, aliases, posix_path.as_ref(), origins),
        rendered: RenderedCounts::of(env, aliases, posix_path.is_some()),
    });
    let interactive_rc = if probe.shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bashrc")
    };
    out.push(EnvTarget::SourceLine {
        rc_path: interactive_rc,
        line: UNIX_SOURCE_LINE.to_string(),
    });
    if probe.fish_present {
        let fish_path = fold_path_line(env, path_dirs, origins, home, platform, None);
        out.push(EnvTarget::ManagedFile {
            path: home.join(".config/fish/conf.d/cfgd-env.fish"),
            content: generate_fish_env_content(env, aliases, fish_path.as_ref(), origins),
            rendered: RenderedCounts::of(env, aliases, fish_path.is_some()),
        });
    }

    // Login (Login + All): login shells via source lines into user-owned files.
    if reaches_login(scope) {
        // zsh reads ~/.zshenv in every context, but only write it when zsh is
        // actually in use — a bash-only host would otherwise gain an inert file
        // (and a spurious write line on-camera) for a shell it never runs.
        if probe.zsh_present {
            out.push(EnvTarget::SourceLine {
                rc_path: home.join(".zshenv"),
                line: UNIX_SOURCE_LINE.to_string(),
            });
        }
        // ~/.profile is the safe sh/bash login fallback. Never create
        // ~/.bash_profile — bash reads the first existing of .bash_profile,
        // .bash_login, .profile and stops, so creating one shadows .profile.
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(".profile"),
            line: UNIX_SOURCE_LINE.to_string(),
        });
        if probe.bash_profile_exists {
            out.push(EnvTarget::SourceLine {
                rc_path: home.join(".bash_profile"),
                line: UNIX_SOURCE_LINE.to_string(),
            });
        } else if probe.bash_login_exists {
            out.push(EnvTarget::SourceLine {
                rc_path: home.join(".bash_login"),
                line: UNIX_SOURCE_LINE.to_string(),
            });
        }
    }

    // All: session-manager surfaces. FreeBSD deliberately matches neither arm —
    // it has no systemd (environment.d) and no launchd (LaunchAgent), so the
    // `.cfgd.env` + rc source lines above are its entire env surface. Emitting a
    // systemd environment.d file there would write inert state no consumer reads.
    if reaches_all(scope) {
        if platform == EnvPlatform::Linux {
            // systemd --user + Wayland GUI sessions read environment.d (KEY=VALUE).
            out.push(EnvTarget::ManagedFile {
                path: home.join(".config/environment.d/cfgd.conf"),
                content: generate_environment_d_content(env),
                rendered: RenderedCounts::of(env, &[], false),
            });
        }
        if platform == EnvPlatform::MacOs {
            // A LaunchAgent that runs `launchctl setenv` at load publishes the vars into the
            // GUI session's launchd domain, so launchd-spawned GUI apps inherit them.
            let vars: BTreeMap<String, String> = valid_export_pairs(env).into_iter().collect();
            // No publishable vars ⇒ no agent: an empty `launchctl setenv` script would be an inert
            // `/bin/sh -c ""` job with nothing to set.
            if !vars.is_empty() {
                out.push(EnvTarget::ManagedFile {
                    path: home
                        .join("Library/LaunchAgents")
                        .join(MACOS_USER_PLIST_NAME),
                    content: launchd_env_plist(MACOS_USER_PLIST_LABEL, &vars),
                    rendered: RenderedCounts::vars_only(vars.len()),
                });
            }
        }
    }
}

fn windows_targets(
    content: EnvContent<'_>,
    home: &Path,
    probe: &EnvHostProbe,
    out: &mut Vec<EnvTarget>,
) {
    let EnvContent {
        env,
        aliases,
        path_dirs,
        origins,
    } = content;
    // PowerShell env file + dot-source into both profile locations.
    let ps_path = primary_folded_path(env, path_dirs, origins, home, EnvPlatform::Windows);
    out.push(EnvTarget::ManagedFile {
        path: primary_env_file_path(home, EnvPlatform::Windows),
        content: generate_powershell_env_content(env, aliases, ps_path.as_ref(), origins),
        rendered: RenderedCounts::of(env, aliases, ps_path.is_some()),
    });
    for dir in ["Documents/PowerShell", "Documents/WindowsPowerShell"] {
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(dir).join("Microsoft.PowerShell_profile.ps1"),
            line: PS_SOURCE_LINE.to_string(),
        });
    }
    // Git Bash, when present, gets the same bash env file + source line as Unix.
    if probe.git_bash_present {
        out.push(EnvTarget::ManagedFile {
            path: home.join(".cfgd.env"),
            content: generate_env_file_content(env, aliases, ps_path.as_ref(), origins),
            rendered: RenderedCounts::of(env, aliases, ps_path.is_some()),
        });
        out.push(EnvTarget::SourceLine {
            rc_path: home.join(".bashrc"),
            line: UNIX_SOURCE_LINE.to_string(),
        });
    }
}

/// How many entries a generated file actually holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RenderedCounts {
    pub(super) vars: usize,
    pub(super) aliases: usize,
}

impl RenderedCounts {
    /// What a shell generator will render from `env` and `aliases`: the
    /// entries whose names pass the same safety filter every generator
    /// applies before emitting a line, so the count describes the file rather
    /// than the declaration list it was derived from.
    ///
    /// `path_line` says the file carries the folded `PATH` assignment, which is
    /// a written line like any other — a count that omitted it named 3 vars
    /// over a file holding four `export`s. It is counted ONCE however many
    /// producers fed it: a declared `PATH` is not a second line, it is part of
    /// that one.
    fn of(env: &[EnvVar], aliases: &[ShellAlias], path_line: bool) -> Self {
        let declared = env
            .iter()
            .filter(|e| crate::validate_env_var_name(&e.name).is_ok());
        let declares_path = declared.clone().any(|e| e.name == "PATH");
        Self {
            vars: usize::from(path_line && !declares_path) + declared.count(),
            aliases: aliases
                .iter()
                .filter(|a| crate::validate_alias_name(&a.name).is_ok())
                .count(),
        }
    }

    /// A rendering with no alias syntax at all (`environment.d`, launchd).
    fn vars_only(vars: usize) -> Self {
        Self { vars, aliases: 0 }
    }
}

/// `(name, value)` pairs whose names pass the shell-safety filter — the same
/// filter the per-shell generators apply, centralized so every target agrees.
fn valid_export_pairs(env: &[EnvVar]) -> Vec<(String, String)> {
    env.iter()
        .filter(|e| crate::validate_env_var_name(&e.name).is_ok())
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect()
}

/// `environment.d(5)` content: `KEY=VALUE`, one per line. **Not shell** — no
/// `export`, no quoting; values are literal (systemd expands `${OTHER}` itself).
pub(super) fn generate_environment_d_content(env: &[EnvVar]) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            // tracing-ok: an env var the user declared under a name no shell can carry; the generated file simply omits it and no row names it
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        // Quoted, not raw: a newline in the value would otherwise end the
        // assignment and let the rest of it stand as further assignments in
        // the user's systemd environment.
        lines.push(format!(
            "{}={}",
            ev.name,
            crate::posix_single_quoted(&ev.value)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Render a launchd LaunchAgent/Daemon plist that publishes `vars` into its launchd
/// domain by running `launchctl setenv` once per variable at load.
///
/// A plist `EnvironmentVariables` dict applies only to the job's own process, so it
/// cannot make `spec.env` reach GUI apps. `launchctl setenv` instead sets each
/// variable in the launchd domain the job runs in — the user's GUI session for a
/// LaunchAgent (`spec.env`), the system domain for a LaunchDaemon
/// (`spec.system.environment`) — so every later-spawned process inherits it. The
/// two consumers differ only by `label` and install domain.
///
/// Names that are not shell-safe identifiers are skipped (they would otherwise inject
/// into the shell command); values are shell-escaped, and the whole command is
/// XML-escaped for the plist `<string>`.
pub fn launchd_env_plist(label: &str, vars: &BTreeMap<String, String>) -> String {
    let setenv_script = vars
        .iter()
        .filter(|(key, _)| crate::validate_env_var_name(key).is_ok())
        .map(|(key, value)| {
            format!(
                "/bin/launchctl setenv {key} {}",
                crate::shell_escape_value(value)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>{script}</string>
    </array>
    <key>RunAtLoad</key>
    <true />
</dict>
</plist>
"#,
        label = crate::xml_escape(label),
        script = crate::xml_escape(&setenv_script),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> EnvHostProbe {
        EnvHostProbe {
            shell: "/bin/bash".to_string(),
            fish_present: true,
            bash_profile_exists: false,
            bash_login_exists: false,
            git_bash_present: false,
            zsh_present: false,
        }
    }

    fn ev(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_string(),
            value: value.to_string(),
            platforms: vec![],
        }
    }

    /// `path_dirs` naming three managers, one of which the declared `PATH`
    /// below already publishes under a `$HOME` spelling.
    fn dirs(home: &str) -> Vec<ManagerPathDir> {
        vec![
            ManagerPathDir::new("brew", "/opt/brewroot/bin"),
            ManagerPathDir::new("cargo", format!("{home}/.cargo/bin")),
            ManagerPathDir::new("npm", format!("{home}/.npm-global/bin")),
        ]
    }

    fn managed_file<'a>(targets: &'a [EnvTarget], name: &str) -> (&'a str, RenderedCounts) {
        targets
            .iter()
            .find_map(|t| match t {
                EnvTarget::ManagedFile {
                    path,
                    content,
                    rendered,
                } if crate::to_posix_string(path).ends_with(name) => {
                    Some((content.as_str(), *rendered))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no managed file ending in {name}"))
    }

    fn path_line<'a>(content: &'a str, prefix: &str) -> &'a str {
        content
            .lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} line in:\n{content}"))
    }

    /// The platform is the caller's to name: which dialect a test's expected
    /// lines are written in is decided by nothing else, so it stays visible at
    /// the call site instead of buried here.
    fn unix_targets_for(home: &Path, platform: EnvPlatform) -> Vec<EnvTarget> {
        let home_str = home.to_string_lossy().into_owned();
        let env = vec![ev("PATH", "$HOME/.cargo/bin:$PATH"), ev("EDITOR", "nvim")];
        let dirs = dirs(&home_str);
        let origins = EnvOrigins::default();
        let probe = probe();
        env_targets(
            EnvContent::new(&env, &[], &dirs, &origins),
            EnvScope::All,
            home,
            &probe,
            platform,
        )
    }

    /// bash/zsh: ONE `export PATH=`, the declared entry first and the surviving
    /// derived directories spliced in ahead of the inherited PATH. The entry the
    /// declaration already publishes is gone, the derived entries spell home
    /// `$HOME` — the same spelling the declaration uses — and the count is the
    /// number of `export` lines written.
    #[test]
    fn bash_path_line_folds_both_producers_into_one_export() {
        let home = Path::new("/home/tj");
        let targets = unix_targets_for(home, EnvPlatform::Linux);
        let (content, rendered) = managed_file(&targets, ".cfgd.env");
        let line = path_line(content, "export PATH=");
        assert_eq!(
            line,
            "export PATH=\"$HOME/.cargo/bin:/opt/brewroot/bin:$HOME/.npm-global/bin:$PATH\" \
             # manager:brew,npm",
            "in:\n{content}"
        );
        assert!(
            !line.contains("/home/tj"),
            "one spelling of home per file: {line}"
        );
        let exports = content.lines().filter(|l| l.starts_with("export ")).count();
        assert_eq!(rendered.vars, exports, "in:\n{content}");
        assert_eq!(rendered.vars, 2);
    }

    /// fish single-quotes each entry, which suppresses `$HOME`, so its one
    /// spelling is the literal path — for the DECLARED entries on the folded
    /// line too, or the line would carry one dead entry. The inherited PATH
    /// stays a bare `$PATH`, which is what splices fish's own list.
    #[test]
    fn fish_path_line_folds_both_producers_and_keeps_a_literal_home() {
        let home = Path::new("/home/tj");
        let targets = unix_targets_for(home, EnvPlatform::Linux);
        let (content, rendered) = managed_file(&targets, "cfgd-env.fish");
        let line = path_line(content, "set -gx PATH '");
        assert_eq!(
            line,
            "set -gx PATH '/home/tj/.cargo/bin' '/opt/brewroot/bin' \
             '/home/tj/.npm-global/bin' $PATH # manager:brew,npm",
            "in:\n{content}"
        );
        assert!(!line.contains("$HOME"), "fish cannot expand it: {line}");
        let sets = content
            .lines()
            .filter(|l| l.starts_with("set -gx "))
            .count();
        assert_eq!(rendered.vars, sets, "in:\n{content}");
    }

    /// PowerShell interpolates `$HOME` inside double quotes, so it takes the
    /// token spelling; `;` is the separator a declared value is split on, and
    /// the declaration's `$PATH` is rendered as PowerShell's own spelling of
    /// the reference rather than as a variable nobody set.
    #[test]
    fn powershell_path_line_folds_both_producers_into_one_assignment() {
        let home = Path::new("C:/Users/tj");
        let env = vec![
            ev("PATH", "$HOME/.cargo/bin;$PATH"),
            ev("EDITOR", "nvim.exe"),
        ];
        let dirs = dirs("C:/Users/tj");
        let origins = EnvOrigins::default();
        let targets = env_targets(
            EnvContent::new(&env, &[], &dirs, &origins),
            EnvScope::All,
            home,
            &probe(),
            EnvPlatform::Windows,
        );
        let (content, rendered) = managed_file(&targets, ".cfgd-env.ps1");
        let line = path_line(content, "$env:PATH = ");
        assert_eq!(
            line,
            "$env:PATH = \"$HOME/.cargo/bin;/opt/brewroot/bin;$HOME/.npm-global/bin;$env:PATH\" \
             # manager:brew,npm",
            "in:\n{content}"
        );
        assert!(
            !line.contains("C:/Users/tj"),
            "one spelling of home per file: {line}"
        );
        let assignments = content.lines().filter(|l| l.starts_with("$env:")).count();
        assert_eq!(rendered.vars, assignments, "in:\n{content}");
    }

    /// `environment.d` renders declared variables only — no derived PATH line,
    /// so its count must not gain one.
    #[test]
    fn environment_d_has_no_derived_path_line_and_counts_only_declarations() {
        let home = Path::new("/home/tj");
        let targets = unix_targets_for(home, EnvPlatform::Linux);
        let (content, rendered) = managed_file(&targets, "environment.d/cfgd.conf");
        assert!(
            !content.contains("/opt/brewroot/bin"),
            "no derived PATH line here:\n{content}"
        );
        assert_eq!(rendered.vars, 2, "in:\n{content}");
    }

    /// The launchd agent publishes declared variables through `launchctl
    /// setenv`; it carries no derived PATH line either.
    #[test]
    fn launchd_agent_has_no_derived_path_line_and_counts_only_declarations() {
        let home = Path::new("/Users/tj");
        let home_str = home.to_string_lossy().into_owned();
        let env = vec![ev("PATH", "$HOME/.cargo/bin:$PATH"), ev("EDITOR", "nvim")];
        let dirs = dirs(&home_str);
        let origins = EnvOrigins::default();
        let targets = env_targets(
            EnvContent::new(&env, &[], &dirs, &origins),
            EnvScope::All,
            home,
            &probe(),
            EnvPlatform::MacOs,
        );
        let (content, rendered) = managed_file(&targets, "com.cfgd.user-environment.plist");
        assert!(
            !content.contains("/opt/brewroot/bin"),
            "no derived PATH line here:\n{content}"
        );
        assert_eq!(rendered.vars, 2, "in:\n{content}");
    }

    /// The one line with two producers names them both, in one comment, in the
    /// two-token shape `without_owner_comment` folds back off.
    #[test]
    fn a_folded_path_line_names_both_its_producers() {
        let home = Path::new("/home/tj");
        let env = vec![ev("PATH", "$HOME/.cargo/bin:$PATH")];
        let dirs = dirs("/home/tj");
        let origins = {
            let mut owners = crate::config::EntryOwners::default();
            owners.claim("module:nvim", &env, &[]);
            EnvOrigins::from_owners(&owners)
        };
        let targets = env_targets(
            EnvContent::new(&env, &[], &dirs, &origins),
            EnvScope::All,
            home,
            &probe(),
            EnvPlatform::Linux,
        );
        let (content, _) = managed_file(&targets, ".cfgd.env");
        let line = path_line(content, "export PATH=");
        assert!(
            line.ends_with(" # manager:brew,npm module:nvim"),
            "one comment naming both producers: {line}"
        );
        assert_eq!(
            without_owner_comment(line),
            "export PATH=\"$HOME/.cargo/bin:/opt/brewroot/bin:$HOME/.npm-global/bin:$PATH\"",
            "the two-token comment folds back off whole: {line}"
        );
    }

    /// Every generated file assigns each variable ONCE, whatever fed it.
    ///
    /// A file holding two `export PATH=` lines assigns one variable twice, and
    /// every count over it then has to choose between naming written lines and
    /// naming variables — which is how `write ~/.cfgd.env — 4 vars` came to sit
    /// one row above `publish 3 vars to the live session`, both counting the
    /// same file. The walk is over every dialect `env_targets` can produce on
    /// every platform, so a sixth generator cannot quietly reintroduce the
    /// split.
    #[test]
    fn no_generated_env_file_assigns_one_variable_twice() {
        let cases = [
            (EnvPlatform::Linux, Path::new("/home/tj")),
            (EnvPlatform::MacOs, Path::new("/Users/tj")),
            (EnvPlatform::FreeBsd, Path::new("/home/tj")),
            (EnvPlatform::Windows, Path::new("C:/Users/tj")),
        ];
        let mut files_seen = 0;
        for (platform, home) in cases {
            let separator = path_separator(platform);
            let env = vec![
                ev("PATH", &format!("$HOME/.cargo/bin{separator}$PATH")),
                ev("EDITOR", "nvim"),
            ];
            let dirs = dirs(&crate::to_posix_string(home));
            let origins = EnvOrigins::default();
            for target in env_targets(
                EnvContent::new(&env, &[], &dirs, &origins),
                EnvScope::All,
                home,
                &probe(),
                platform,
            ) {
                let EnvTarget::ManagedFile { path, content, .. } = target else {
                    continue;
                };
                files_seen += 1;
                let mut assigned: Vec<&str> = Vec::new();
                for line in content.lines() {
                    let Some(name) = assigned_variable(line) else {
                        continue;
                    };
                    assert!(
                        !assigned.contains(&name),
                        "{} assigns {name} twice:\n{content}",
                        path.display()
                    );
                    assigned.push(name);
                }
            }
        }
        assert!(
            files_seen >= 6,
            "the walk no longer reaches every dialect — it read {files_seen} files"
        );
    }

    /// [`recorded_env_method`] answers off a recorded id's file name alone,
    /// while the engine decides write-vs-inject by which target arm it builds
    /// — two vocabularies that would drift the first time a new generated
    /// file or rc target lands in [`env_targets`]. This walk classifies every
    /// target the builders really push, on every platform, under two probe
    /// shapes (bash_profile and bash_login hosts), so a new member lands here
    /// before its status row can claim the wrong verb.
    #[test]
    fn every_env_target_classifies_under_the_verb_that_produced_it() {
        let cases = [
            (EnvPlatform::Linux, Path::new("/home/tj")),
            (EnvPlatform::MacOs, Path::new("/Users/tj")),
            (EnvPlatform::FreeBsd, Path::new("/home/tj")),
            (EnvPlatform::Windows, Path::new("C:/Users/tj")),
        ];
        let probes = [
            EnvHostProbe {
                shell: "/bin/zsh".to_string(),
                fish_present: true,
                bash_profile_exists: true,
                bash_login_exists: false,
                git_bash_present: true,
                zsh_present: true,
            },
            EnvHostProbe {
                shell: "/bin/bash".to_string(),
                fish_present: false,
                bash_profile_exists: false,
                bash_login_exists: true,
                git_bash_present: false,
                zsh_present: false,
            },
        ];
        let mut writes = 0;
        let mut injects = 0;
        for (platform, home) in cases {
            for probe in &probes {
                let separator = path_separator(platform);
                let env = vec![ev("PATH", &format!("$HOME/.cargo/bin{separator}$PATH"))];
                let dirs = dirs(&crate::to_posix_string(home));
                let origins = EnvOrigins::default();
                // Every scope, not just the superset: a future arm gated on a
                // narrower scope must classify too.
                for scope in [EnvScope::All, EnvScope::Login, EnvScope::Interactive] {
                    for target in env_targets(
                        EnvContent::new(&env, &[], &dirs, &origins),
                        scope,
                        home,
                        probe,
                        platform,
                    ) {
                        // The recorded id is `to_posix_string(path)` for both verbs
                        // (`format_action_description`'s env arms), so the walk
                        // folds the same way before asking.
                        let (id, expected) = match &target {
                            EnvTarget::ManagedFile { path, .. } => {
                                writes += 1;
                                (crate::to_posix_string(path), ENV_VERB_WRITE)
                            }
                            EnvTarget::SourceLine { rc_path, .. } => {
                                injects += 1;
                                (crate::to_posix_string(rc_path), ENV_VERB_INJECT)
                            }
                            EnvTarget::LiveSession { .. } => continue,
                        };
                        assert_eq!(
                            recorded_env_method(&id),
                            expected,
                            "{id} classifies under the verb that produced it"
                        );
                    }
                }
            }
        }
        assert!(
            writes >= 6 && injects >= 6,
            "the walk no longer reaches both verbs' members ({writes} writes, {injects} injects)"
        );
    }

    /// The variable an assignment line names, in any dialect this crate
    /// generates. `None` for a line that assigns nothing (the header, a blank,
    /// an `alias`/`abbr`/`Set-Alias`, a plist element).
    fn assigned_variable(line: &str) -> Option<&str> {
        for prefix in ["export ", "set -gx "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                return rest.split(['=', ' ']).next();
            }
        }
        if let Some(rest) = line.strip_prefix("$env:") {
            return rest.split([' ', '=']).next();
        }
        // `environment.d` is bare `KEY=VALUE`; a plist line starts with `<`.
        let (name, _) = line.split_once('=')?;
        (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .then_some(name)
    }

    /// The write's own count and what the session publish says are one number
    /// for a file both producers fed: the row above and the row below are
    /// counting the same variables.
    #[test]
    fn the_written_var_count_matches_the_session_publish() {
        let home = Path::new("/home/tj");
        let targets = unix_targets_for(home, EnvPlatform::Linux);
        let (_, rendered) = managed_file(&targets, ".cfgd.env");
        let published = targets
            .iter()
            .find_map(|t| match t {
                EnvTarget::LiveSession { vars } => Some(vars.len()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a scope of All publishes to the live session"));
        assert_eq!(
            rendered.vars, published,
            "the file's variables and the session publish's are one count"
        );
    }
}
