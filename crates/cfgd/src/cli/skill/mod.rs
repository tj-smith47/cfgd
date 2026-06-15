//! `cfgd skill ...` — install, list, remove, and update agent-skill primitives
//! across the supported coding-agent providers (Claude Code, Gemini, Copilot,
//! Codex, Cursor).
//!
//! Each author kind ([`SkillKind`]) renders to every provider's native primitive
//! via [`cfgd_core::providers::skill`]. The command bodies are implemented in a
//! later task; the variants and dispatch stubs here pin the CLI surface.

use std::path::PathBuf;

use anyhow::anyhow;
use cfgd_core::output::{Doc, Printer, Role, collapse_to_subject_line};
use cfgd_core::providers::skill::{Detection, SkillScope, all_skill_providers};
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
}

impl SkillInstallResult {
    /// A provider that wrote its skill file at `path`.
    fn installed(provider: String, path: PathBuf) -> Self {
        Self {
            provider,
            path: Some(path.display().to_string()),
            status: SkillResultStatus::Installed,
            reason: None,
        }
    }

    /// A provider deliberately not written (undetected, unsupported scope, or a
    /// declined overwrite), carrying the human-readable `reason`.
    fn skipped(provider: String, reason: impl Into<String>) -> Self {
        Self {
            provider,
            path: None,
            status: SkillResultStatus::Skipped,
            reason: Some(reason.into()),
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

/// Install an agent skill for one author kind across detected providers.
pub fn cmd_skill_install(
    printer: &Printer,
    kind: SkillKind,
    global: bool,
    providers: &[String],
    force: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let scope = if global {
        SkillScope::User
    } else {
        SkillScope::Project
    };
    let model = cfgd_core::generate::skill_model_for(kind.to_core());

    let all = all_skill_providers();

    // An explicit `--provider` list is a hard contract: every named id must
    // exist, else it is a user error (never silently ignored).
    if !providers.is_empty() {
        for name in providers {
            if !all.iter().any(|p| p.id() == name) {
                let valid: Vec<&str> = all.iter().map(|p| p.id()).collect();
                return Err(anyhow!(
                    "unknown provider '{name}'; valid providers: {}",
                    valid.join(", ")
                ));
            }
        }
    }

    let explicit = !providers.is_empty();
    let mut results: Vec<SkillInstallResult> = Vec::new();
    let mut any_targeted_failure = false;

    for provider in &all {
        let id = provider.id().to_string();
        if explicit && !providers.iter().any(|n| n == &id) {
            continue;
        }

        // Named or auto: detection decides each provider's fate.
        let detection = provider.detect(scope);
        let targeted = explicit || force || detection == Detection::Present;

        match detection {
            Detection::Unsupported(reason) => {
                // Never fabricate a path for a scope the provider has no
                // primitive at — even when forced or explicitly named.
                results.push(SkillInstallResult::skipped(id, reason));
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

    let scope_word = match scope {
        SkillScope::Project => "project",
        SkillScope::User => "user",
    };
    let heading = format!(
        "Installing skill {} ({scope_word} scope)",
        kind.to_core().as_str()
    );
    // A section renders its rows contiguously (kubectl/docker spacing); bare
    // top-level status rows would each get a blank-line separator.
    let doc = Doc::new().section(heading, |mut sec| {
        for r in &results {
            let role = r.status.role();
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
    });

    let payload = SkillInstallPayload {
        kind: kind.to_core().as_str().to_string(),
        scope,
        cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
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

/// List installed agent skills.
pub fn cmd_skill_list(_printer: &Printer, _global: bool) -> anyhow::Result<()> {
    Ok(())
}

/// Remove an installed agent skill for one author kind.
pub fn cmd_skill_remove(
    _printer: &Printer,
    _kind: SkillKind,
    _global: bool,
    _providers: &[String],
    _yes: bool,
) -> anyhow::Result<()> {
    Ok(())
}

/// Update one or all installed agent skills to the current rendering.
pub fn cmd_skill_update(
    _printer: &Printer,
    _kind: Option<SkillKind>,
    _all: bool,
    _global: bool,
    _providers: &[String],
) -> anyhow::Result<()> {
    Ok(())
}
