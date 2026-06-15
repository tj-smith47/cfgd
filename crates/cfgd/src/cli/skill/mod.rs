//! `cfgd skill ...` — install, list, remove, and update agent-skill primitives
//! across the supported coding-agent providers (Claude Code, Gemini, Copilot,
//! Codex, Cursor).
//!
//! Each author kind ([`SkillKind`]) renders to every provider's native primitive
//! via [`cfgd_core::providers::skill`].

use std::path::PathBuf;

use anyhow::anyhow;
use cfgd_core::output::{Doc, Printer, Role, collapse_to_subject_line};
use cfgd_core::providers::skill::{
    Detection, InstalledSkill, SkillProvider, SkillScope, all_skill_providers,
};
use serde::Serialize;

/// The author-facing resource kinds a skill can teach, as a clap positional
/// value. Maps 1:1 to [`cfgd_core::generate::SkillKind`] via [`SkillKind::to_core`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SkillKind {
    Module,
    Profile,
    Source,
    MachineConfig,
    ConfigPolicy,
    ClusterConfigPolicy,
}

impl SkillKind {
    /// Map the clap-facing kind to the `cfgd-core` authoring kind.
    pub fn to_core(self) -> cfgd_core::generate::SkillKind {
        use cfgd_core::generate::SkillKind as Core;
        match self {
            Self::Module => Core::Module,
            Self::Profile => Core::Profile,
            Self::Source => Core::Source,
            Self::MachineConfig => Core::MachineConfig,
            Self::ConfigPolicy => Core::ConfigPolicy,
            Self::ClusterConfigPolicy => Core::ClusterConfigPolicy,
        }
    }
}

/// The terminal outcome of one provider in a skill operation's structured
/// payload. Single-sources the `results[].status` wire contract that scripts/CI
/// parse and that the 5.3 list/remove/update bodies reuse. Each variant pins its
/// exact lowercase wire token via an explicit `rename` (the codebase forbids a
/// `rename_all` on enums; per-variant single-word renames are the sanctioned
/// way to fix wire bytes — see `generate::AgentDecision`).
#[derive(Debug, Clone, Copy, Serialize)]
enum SkillResultStatus {
    #[serde(rename = "installed")]
    Installed,
    #[serde(rename = "removed")]
    Removed,
    #[serde(rename = "updated")]
    Updated,
    #[serde(rename = "skipped")]
    Skipped,
    #[serde(rename = "failed")]
    Failed,
}

impl SkillResultStatus {
    /// The output [`Role`] that renders this status's human status line.
    fn role(self) -> Role {
        match self {
            Self::Installed => Role::Ok,
            Self::Removed => Role::Ok,
            Self::Updated => Role::Ok,
            Self::Failed => Role::Fail,
            Self::Skipped => Role::Skipped,
        }
    }
}

/// One provider's outcome in the structured install payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillInstallResult {
    provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    status: SkillResultStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Render this skip as a visible warning rather than a quiet skip. Human-only:
    /// elevates the row's [`Role`] without changing the wire `status` (the JSON
    /// stays `"skipped"`; a structured consumer reads the severity off `reason`).
    /// Set only for an unsupported-at-this-scope skip — where the user's explicit
    /// request cannot be honored for that provider, unlike a routine "not detected".
    #[serde(skip)]
    warn: bool,
}

impl SkillInstallResult {
    /// A provider that wrote its skill file at `path`.
    fn installed(provider: String, path: PathBuf) -> Self {
        Self {
            provider,
            path: Some(path.display().to_string()),
            status: SkillResultStatus::Installed,
            reason: None,
            warn: false,
        }
    }

    /// A provider whose installed skill was excised from `path`.
    fn removed(provider: String, path: PathBuf) -> Self {
        Self {
            provider,
            path: Some(path.display().to_string()),
            status: SkillResultStatus::Removed,
            reason: None,
            warn: false,
        }
    }

    /// A provider whose installed skill was re-rendered in place at `path`.
    fn updated(provider: String, path: PathBuf) -> Self {
        Self {
            provider,
            path: Some(path.display().to_string()),
            status: SkillResultStatus::Updated,
            reason: None,
            warn: false,
        }
    }

    /// A provider deliberately not written (undetected or a declined overwrite),
    /// carrying the human-readable `reason`. A quiet skip — for an
    /// unsupported-at-this-scope skip use [`unsupported`](Self::unsupported).
    fn skipped(provider: String, reason: impl Into<String>) -> Self {
        Self {
            provider,
            path: None,
            status: SkillResultStatus::Skipped,
            reason: Some(reason.into()),
            warn: false,
        }
    }

    /// A provider that has no primitive at the requested scope (`reason` explains
    /// why). The wire `status` is `"skipped"` like any other skip, but `warn` is
    /// set so the human row renders as a visible warning: the user's explicit
    /// scope request cannot be honored for this provider.
    fn unsupported(provider: String, reason: impl Into<String>) -> Self {
        Self {
            provider,
            path: None,
            status: SkillResultStatus::Skipped,
            reason: Some(reason.into()),
            warn: true,
        }
    }

    /// A provider whose install was attempted but errored, carrying the
    /// collapsed failure `reason`.
    fn failed(provider: String, reason: impl Into<String>) -> Self {
        Self {
            provider,
            path: None,
            status: SkillResultStatus::Failed,
            reason: Some(reason.into()),
            warn: false,
        }
    }
}

/// The full structured payload emitted by `cmd_skill_install`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillInstallPayload {
    kind: String,
    scope: SkillScope,
    cfgd_version: String,
    results: Vec<SkillInstallResult>,
}

/// Flatten a skill-install error into an actionable single-line reason.
///
/// Lock contention is the fail-fast apply-lock contract: surface a retry hint
/// rather than the raw `flock`/debug string. Every other error collapses to its
/// subject line so a multi-line `io::Error` never reaches a status subject.
fn install_failure_reason(err: &cfgd_core::errors::CfgdError) -> String {
    use cfgd_core::errors::{CfgdError, SkillError};
    if let CfgdError::Skill(SkillError::Lock(_)) = err {
        return "another cfgd install is in progress; retry shortly".to_string();
    }
    collapse_to_subject_line(err)
}

/// The running cfgd version, stamped into every skill payload's `cfgdVersion`.
fn cfgd_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Resolve the install/remove/update scope from the `--global` flag.
fn resolve_scope(global: bool) -> SkillScope {
    if global {
        SkillScope::User
    } else {
        SkillScope::Project
    }
}

/// Validate an explicit `--provider` list against the registry. Every named id
/// must exist, else it is a user error (never silently ignored). An empty list
/// (auto mode) always passes.
fn validate_provider_ids(
    all: &[Box<dyn SkillProvider>],
    providers: &[String],
) -> anyhow::Result<()> {
    for name in providers {
        if !all.iter().any(|p| p.id() == name) {
            let valid: Vec<&str> = all.iter().map(|p| p.id()).collect();
            return Err(anyhow!(
                "unknown provider '{name}'; valid providers: {}",
                valid.join(", ")
            ));
        }
    }
    Ok(())
}

/// Whether a provider is a target given an explicit `--provider` selection: in
/// auto mode (`providers` empty) every provider is a candidate; with explicit
/// names only the named ones are.
fn is_target(id: &str, providers: &[String]) -> bool {
    providers.is_empty() || providers.iter().any(|n| n == id)
}

/// The structured payload shape shared by remove/update (and reused for any
/// per-provider operation that isn't an install). `kind` is `None` for the
/// `update --all` enumeration, which spans every installed kind.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillOpPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    scope: SkillScope,
    cfgd_version: String,
    results: Vec<SkillInstallResult>,
}

/// The human word for a scope, for headings.
fn scope_word(scope: SkillScope) -> &'static str {
    match scope {
        SkillScope::Project => "project",
        SkillScope::User => "user",
    }
}

/// Build the contiguous status-row section shared by every skill command's Doc.
/// A section renders its rows kubectl-tight (no blank-line separators); bare
/// top-level status rows would each gain a blank line.
fn results_section(heading: String, results: &[SkillInstallResult]) -> Doc {
    Doc::new().section(heading, |mut sec| {
        for r in results {
            let role = if r.warn { Role::Warn } else { r.status.role() };
            let subject = match &r.path {
                Some(path) => format!("{}: {path}", r.provider),
                None => r.provider.clone(),
            };
            sec = sec.status_with(role, subject, |f| match &r.reason {
                Some(reason) => f.detail(reason.clone()),
                None => f,
            });
        }
        sec
    })
}

/// Install an agent skill for one author kind across detected providers.
pub fn cmd_skill_install(
    printer: &Printer,
    kind: SkillKind,
    global: bool,
    providers: &[String],
    force: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let scope = resolve_scope(global);
    let model = cfgd_core::generate::skill_model_for(kind.to_core());

    let all = all_skill_providers();
    validate_provider_ids(&all, providers)?;

    let explicit = !providers.is_empty();
    let mut results: Vec<SkillInstallResult> = Vec::new();
    let mut any_targeted_failure = false;

    for provider in &all {
        let id = provider.id().to_string();
        if explicit && !is_target(&id, providers) {
            continue;
        }

        // Named or auto: detection decides each provider's fate.
        let detection = provider.detect(scope);
        let targeted = explicit || force || detection == Detection::Present;

        match detection {
            Detection::Unsupported(reason) => {
                // Never fabricate a path for a scope the provider has no
                // primitive at — even when forced or explicitly named. Surface it
                // as a visible warning: the user's explicit scope request cannot be
                // honored for this provider, unlike a routine "not detected" skip.
                results.push(SkillInstallResult::unsupported(id, reason));
                continue;
            }
            Detection::Absent if !targeted => {
                results.push(SkillInstallResult::skipped(id, "not detected"));
                continue;
            }
            Detection::Present | Detection::Absent => {}
        }

        // Confirm before clobbering an already-installed skill, unless the user
        // opted out (`--yes`) or pinned this provider (force / explicit name).
        // A prompt error (structured/non-TTY mode) propagates with the standard
        // "re-run with --yes" guidance — matching `source add` — rather than
        // silently skipping, so a scripted overwrite fails loudly.
        if !yes
            && !force
            && !explicit
            && skill_already_installed(provider.as_ref(), kind, scope)
            && !printer.prompt_confirm(&format!("Overwrite existing {id} skill?"))?
        {
            results.push(SkillInstallResult::skipped(id, "declined overwrite"));
            continue;
        }

        match provider.install(&model, scope) {
            Ok(path) => results.push(SkillInstallResult::installed(id, path)),
            Err(e) => {
                any_targeted_failure = true;
                results.push(SkillInstallResult::failed(id, install_failure_reason(&e)));
            }
        }
    }

    let heading = format!(
        "Installing skill {} ({} scope)",
        kind.to_core().as_str(),
        scope_word(scope)
    );
    let doc = results_section(heading, &results);

    let payload = SkillInstallPayload {
        kind: kind.to_core().as_str().to_string(),
        scope,
        cfgd_version: cfgd_version(),
        results,
    };
    printer.emit(doc.with_data(&payload));

    // Emit-then-exit: a hard exit avoids re-entering the error sink (which would
    // double-emit the Doc under `-o json`). Skips never count as failure.
    if any_targeted_failure {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// Whether `provider` has already installed the skill for `kind` at `scope`,
/// via its public `list` (which detects both whole-file and managed-block
/// installs). Best-effort: a `list` error means "treat as not installed" so a
/// transient read hiccup never blocks a fresh install behind a phantom prompt.
fn skill_already_installed(
    provider: &dyn cfgd_core::providers::skill::SkillProvider,
    kind: SkillKind,
    scope: SkillScope,
) -> bool {
    let core_kind = kind.to_core();
    provider
        .list(scope)
        .map(|installed| installed.iter().any(|s| s.kind == core_kind))
        .unwrap_or(false)
}

/// The structured payload emitted by `cmd_skill_list`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillListPayload {
    scope: SkillScope,
    cfgd_version: String,
    installed: Vec<InstalledSkill>,
}

/// List installed agent skills across every provider at the resolved scope.
///
/// A provider with nothing installed contributes no rows (empty is not an
/// error); a provider whose `list` errors surfaces as a failed-listing
/// propagation rather than a silent drop.
pub fn cmd_skill_list(printer: &Printer, global: bool) -> anyhow::Result<()> {
    let scope = resolve_scope(global);
    let mut installed: Vec<InstalledSkill> = Vec::new();
    for provider in &all_skill_providers() {
        let listed = provider.list(scope).map_err(|e| {
            anyhow!(
                "listing {} skills failed: {}",
                provider.id(),
                collapse_to_subject_line(&e)
            )
        })?;
        installed.extend(listed);
    }

    let heading = format!("Installed skills ({} scope)", scope_word(scope));
    let doc = if installed.is_empty() {
        Doc::new().section(heading, |sec| {
            sec.status_with(Role::Info, "no skills installed".to_string(), |f| f)
        })
    } else {
        Doc::new().section(heading, |mut sec| {
            for s in &installed {
                let version = s.cfgd_version.as_deref().unwrap_or("unknown");
                let subject = format!(
                    "{}/{}: {} ({})",
                    s.provider,
                    s.kind.as_str(),
                    s.path.display(),
                    version
                );
                let role = if s.stale { Role::Warn } else { Role::Ok };
                sec = sec.status_with(role, subject, |f| {
                    if s.stale {
                        f.detail("stale — run `cfgd skill update`".to_string())
                    } else {
                        f
                    }
                });
            }
            sec
        })
    };

    let payload = SkillListPayload {
        scope,
        cfgd_version: cfgd_version(),
        installed,
    };
    printer.emit(doc.with_data(&payload));
    Ok(())
}

/// Remove an installed agent skill for one author kind across target providers.
pub fn cmd_skill_remove(
    printer: &Printer,
    kind: SkillKind,
    global: bool,
    providers: &[String],
    yes: bool,
) -> anyhow::Result<()> {
    let scope = resolve_scope(global);
    let core_kind = kind.to_core();

    let all = all_skill_providers();
    validate_provider_ids(&all, providers)?;

    // Partition the targeted providers by install state in a single `list` pass
    // each: those with the skill installed get a real `remove`; an
    // explicitly-named provider with nothing installed still earns a `skipped`
    // row (so a `--provider <id>` request is never silently dropped).
    let mut targets: Vec<&Box<dyn SkillProvider>> = Vec::new();
    let mut not_installed: Vec<&Box<dyn SkillProvider>> = Vec::new();
    for provider in all.iter().filter(|p| is_target(p.id(), providers)) {
        if skill_already_installed(provider.as_ref(), kind, scope) {
            targets.push(provider);
        } else {
            not_installed.push(provider);
        }
    }

    // Confirm before excising, unless opted out. Prompting only when something is
    // actually installed avoids a phantom confirm on a no-op; the prompt errs in
    // structured/non-TTY mode (matching `source rm`) rather than silently
    // proceeding, so a scripted removal without `--yes` fails loudly.
    if !yes
        && !targets.is_empty()
        && !printer.prompt_confirm(&format!(
            "Remove the {} skill from {} provider(s)?",
            core_kind.as_str(),
            targets.len()
        ))?
    {
        let doc = results_section(
            format!(
                "Removing skill {} ({} scope)",
                core_kind.as_str(),
                scope_word(scope)
            ),
            &[],
        );
        let payload = SkillOpPayload {
            kind: Some(core_kind.as_str().to_string()),
            scope,
            cfgd_version: cfgd_version(),
            results: Vec::new(),
        };
        printer.emit(doc.with_data(&payload));
        return Ok(());
    }

    let mut results: Vec<SkillInstallResult> = Vec::new();
    let mut any_failure = false;
    for provider in &targets {
        let id = provider.id().to_string();
        match provider.remove(core_kind, scope) {
            Ok(Some(path)) => results.push(SkillInstallResult::removed(id, path)),
            Ok(None) => results.push(SkillInstallResult::skipped(id, "not installed")),
            Err(e) => {
                any_failure = true;
                results.push(SkillInstallResult::failed(id, install_failure_reason(&e)));
            }
        }
    }
    for provider in &not_installed {
        results.push(SkillInstallResult::skipped(
            provider.id().to_string(),
            "not installed",
        ));
    }

    let heading = format!(
        "Removing skill {} ({} scope)",
        core_kind.as_str(),
        scope_word(scope)
    );
    let doc = results_section(heading, &results);
    let payload = SkillOpPayload {
        kind: Some(core_kind.as_str().to_string()),
        scope,
        cfgd_version: cfgd_version(),
        results,
    };
    printer.emit(doc.with_data(&payload));

    if any_failure {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}

/// Re-render and re-install one already-installed (provider, kind) pair, mapping
/// the outcome to an `Updated`/`Failed` result row. Update never installs into a
/// provider that didn't already have the kind — callers gate on `list` first.
fn update_one(
    provider: &dyn SkillProvider,
    core_kind: cfgd_core::generate::SkillKind,
    scope: SkillScope,
) -> SkillInstallResult {
    let id = provider.id().to_string();
    let model = cfgd_core::generate::skill_model_for(core_kind);
    match provider.install(&model, scope) {
        Ok(path) => SkillInstallResult::updated(id, path),
        Err(e) => SkillInstallResult::failed(id, install_failure_reason(&e)),
    }
}

/// Update one or all installed agent skills to the current rendering.
///
/// `--all` re-renders every currently-installed (provider, kind) pair at scope;
/// a single `<kind>` re-renders that kind only where it is already installed.
/// Update never freshly installs a kind into a provider that lacked it.
pub fn cmd_skill_update(
    printer: &Printer,
    kind: Option<SkillKind>,
    all: bool,
    global: bool,
    providers: &[String],
) -> anyhow::Result<()> {
    let scope = resolve_scope(global);

    let registry = all_skill_providers();
    validate_provider_ids(&registry, providers)?;

    let mut results: Vec<SkillInstallResult> = Vec::new();
    let mut any_failure = false;

    if all {
        // Enumerate currently-installed skills and re-render each in place.
        for provider in registry.iter().filter(|p| is_target(p.id(), providers)) {
            let listed = provider.list(scope).map_err(|e| {
                anyhow!(
                    "listing {} skills failed: {}",
                    provider.id(),
                    collapse_to_subject_line(&e)
                )
            })?;
            for s in listed {
                let r = update_one(provider.as_ref(), s.kind, scope);
                if matches!(r.status, SkillResultStatus::Failed) {
                    any_failure = true;
                }
                results.push(r);
            }
        }
    } else {
        // Exactly one kind (clap guarantees `kind` is set when `--all` is not).
        let kind = kind.ok_or_else(|| anyhow!("skill update requires a <kind> or --all"))?;
        let core_kind = kind.to_core();
        for provider in registry.iter().filter(|p| is_target(p.id(), providers)) {
            let id = provider.id().to_string();
            if !skill_already_installed(provider.as_ref(), kind, scope) {
                results.push(SkillInstallResult::skipped(id, "not installed"));
                continue;
            }
            let r = update_one(provider.as_ref(), core_kind, scope);
            if matches!(r.status, SkillResultStatus::Failed) {
                any_failure = true;
            }
            results.push(r);
        }
    }

    let target = match kind {
        Some(k) => k.to_core().as_str().to_string(),
        None => "all".to_string(),
    };
    let heading = format!("Updating skill {target} ({} scope)", scope_word(scope));
    let doc = results_section(heading, &results);
    let payload = SkillOpPayload {
        kind: kind.map(|k| k.to_core().as_str().to_string()),
        scope,
        cfgd_version: cfgd_version(),
        results,
    };
    printer.emit(doc.with_data(&payload));

    if any_failure {
        cfgd_core::exit::ExitCode::Error.exit();
    }
    Ok(())
}
