use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::parse::check_yaml_anchor_limit;
use super::profile_spec::{
    EncryptionSpec, FileStrategy, PatchSpec, ScriptSpec, SystemSettings, validate_file_patch_shape,
};
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};

// --- Module ---

/// A `module.yaml` document: a reusable, named bundle of packages/files/env/
/// aliases/scripts/system settings a profile pulls in by name.
///
/// ```yaml
/// apiVersion: cfgd.io/v1alpha1
/// kind: Module
/// metadata:
///   name: nvim
/// spec:
///   packages:
///     - name: neovim
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleDocument {
    /// API group/version, e.g. `cfgd.io/v1alpha1`.
    pub api_version: String,
    /// Document kind. Always `Module` for this file.
    pub kind: String,
    /// Identifying metadata for this module.
    pub metadata: ModuleMetadata,
    /// The module's declared surface.
    pub spec: ModuleSpec,
}

/// `metadata`: identifying information for a module.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleMetadata {
    /// The module's name, referenced from a profile's `modules:` list.
    pub name: String,
    /// A one-line human summary shown in `cfgd module list` / `cfgd module show`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The module's own release version, as `MAJOR.MINOR.PATCH` with optional
    /// pre-release and build metadata (`1.2.0`, `2.0.0-rc.1`). It names the
    /// `<module>/v<version>` release tag that the workflow from
    /// `cfgd workflow generate` cuts when the module changes, so bumping it is
    /// what publishes a new release. Absent on modules that are not released
    /// independently.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_module_version"
    )]
    // The published editor schema is a second enforcement point: without the
    // pattern an editor green-lights a value the parser then rejects.
    #[schemars(pattern(SEMVER_PATTERN))]
    pub version: Option<String>,
}

/// JSON Schema `pattern` for `metadata.version`, the semver.org reference regexp.
/// Kept in step with what [`module_version_error`] accepts.
const SEMVER_PATTERN: &str = r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$";

/// Reject a `metadata.version` that is not strict semver, at every parse path:
/// wiring the check into the field's `Deserialize` means `parse_module`, the
/// kind-registry validator, and any direct `serde_yaml::from_str` all agree on
/// what the field accepts.
fn deserialize_module_version<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = &raw {
        // The raw message, not the wrapped CfgdError: serde already re-wraps into a
        // parse error naming the offending line, so a second "config error: invalid
        // config:" prefix would just nest inside it.
        module_version_error(value).map_or(Ok(()), |m| Err(serde::de::Error::custom(m)))?;
    }
    Ok(raw)
}

/// The rejection message for a `metadata.version` that is not strict semver, or
/// `None` when the value is acceptable.
///
/// Strict on purpose: a loose `0.10` and a `v`-prefixed `v1.2.3` both produce
/// release tags that no consumer can resolve back to a version, so they are
/// rejected rather than coerced — which also rules out the crate's deliberately
/// lenient `parse_loose_version`.
fn module_version_error(value: &str) -> Option<String> {
    if semver::Version::parse(value).is_ok() {
        return None;
    }
    Some(format!(
        "metadata.version '{value}' is not a valid semantic version: expected MAJOR.MINOR.PATCH with optional pre-release and build metadata (for example 1.2.0 or 2.0.0-rc.1)"
    ))
}

/// `spec`: the declared surface of a module — everything it contributes to a
/// profile that includes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSpec {
    /// Names of other modules this one requires; cfgd resolves and applies them
    /// first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<String>,

    /// Platform tags gating the whole module. When non-empty and the current
    /// platform matches none of them, the module is skipped entirely (it
    /// appears as a skipped action rather than vanishing). Tags are matched
    /// against the machine's OS, distro, and arch; use `macos` for macOS.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "crate::platform::deserialize_platform_tags"
    )]
    pub platforms: Vec<String>,

    /// Packages this module installs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<ModulePackageEntry>,

    /// Files this module deploys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ModuleFileEntry>,

    /// Environment variables this module contributes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,

    /// Shell aliases this module contributes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<ShellAlias>,

    /// Lifecycle scripts (`preApply`, `postApply`, …) this module runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<ScriptSpec>,

    /// System configurator settings contributed by this module.
    /// Deep-merged into the profile system map; module values override profile values at leaf level.
    #[serde(default, skip_serializing_if = "SystemSettings::is_empty")]
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    pub system: SystemSettings,
}

/// One entry of `spec.packages[]`: a package this module installs.
///
/// ```yaml
/// packages:
///   - name: neovim
///     minVersion: "0.9"
///     prefer: [brew, apt]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModulePackageEntry {
    /// The package name as the chosen manager knows it.
    #[serde(default)]
    pub name: String,

    /// Minimum acceptable installed version, loosely parsed (`"1.2"`, `"1"`).
    /// A version below this is treated as not satisfying the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,

    /// Manager preference order for this package, overriding the profile's
    /// default manager priority (e.g. `[brew, apt]`, or `[script]` to force
    /// this entry's own `script`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<String>,

    /// Manager-specific package name aliases (e.g. `{apt: "neovim", brew:
    /// "neovim"}`) for a package named differently across managers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub aliases: HashMap<String, String>,

    /// Shell script to run instead of a manager install, selected via
    /// `prefer: [script]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Run the install script only if this command exits zero. A non-zero exit
    /// skips the install (the condition for installing was not met). Only
    /// meaningful for a `prefer: [script]` install; ignored for manager-backed
    /// installs (those are idempotent via the manager's installed-package query).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "onlyIf")]
    pub only_if: Option<String>,

    /// Run the install script only if this command exits NON-zero. A zero exit
    /// (success) skips the install (the package already appears present). Only
    /// meaningful for a `prefer: [script]` install; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<String>,

    /// Skip the install script if this path already exists. A leading `~`
    /// expands to the home directory; a relative path resolves against the
    /// script's working directory. Existence follows symlinks. Only meaningful
    /// for a `prefer: [script]` install; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creates: Option<String>,

    /// Package managers to never use for this package, even if otherwise
    /// available and preferred by the profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,

    /// Platform tags gating this package alone. Empty means install on every
    /// platform the module itself is not already gated off of.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "crate::platform::deserialize_platform_tags"
    )]
    pub platforms: Vec<String>,
}

/// One entry of `spec.files[]`: a file this module deploys.
///
/// ```yaml
/// files:
///   - source: files/init.lua
///     target: ~/.config/nvim/init.lua
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleFileEntry {
    /// Path to the source file, relative to the module directory. Not
    /// required when `strategy` is `Patch`; required otherwise.
    #[serde(default)]
    pub source: String,
    /// Destination path on the machine. A leading `~` expands to the home
    /// directory.
    pub target: String,
    /// Per-file deployment strategy override. Omitted, the module-wide default
    /// applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<FileStrategy>,
    /// When true, the source file is local-only: auto-added to .gitignore,
    /// silently skipped on machines where it doesn't exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Encryption settings for this module file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionSpec>,
    /// Unix permission bits (e.g. "600", "644") to apply after deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    /// Structured merge or script configuration for `strategy: Patch`.
    /// Required when `strategy` is `Patch`, rejected otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSpec>,
}

/// Validate the `patch` strategy shape of every module file entry
/// (`spec.files`). See `validate_file_patch_shape`.
pub fn validate_module_file_entries(entries: &[ModuleFileEntry]) -> Result<()> {
    for entry in entries {
        validate_file_patch_shape(
            &format!("module file '{}'", entry.target),
            entry.source.is_empty(),
            entry.strategy,
            entry.patch.as_ref(),
            entry.encryption.is_some(),
            entry.private,
        )?;
    }
    Ok(())
}

/// Interpreter for inline lifecycle scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ScriptShell {
    /// Platform default: `sh` on Unix, `cmd.exe` on Windows.
    #[default]
    Auto,
    Sh,
    Bash,
    Zsh,
    Pwsh,
    Cmd,
}

case_insensitive_enum!(ScriptShell {
    "auto" => ScriptShell::Auto,
    "sh" => ScriptShell::Sh,
    "bash" => ScriptShell::Bash,
    "zsh" => ScriptShell::Zsh,
    "pwsh" => ScriptShell::Pwsh,
    "cmd" => ScriptShell::Cmd,
});

/// A lifecycle script entry: either a bare command string, or a mapping for
/// one that needs a timeout, shell, or guard condition.
///
/// ```yaml
/// preApply: "echo starting"
/// # or
/// postApply:
///   run: brew update
///   timeout: 2m
///   onlyIf: command -v brew
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ScriptEntry {
    /// A bare command string, run with the platform's default shell and no
    /// timeout/guard.
    Simple(String),
    /// The mapping form, carrying the body and its knobs.
    // A named type rather than an inline variant so `cfgd explain` shows a
    // reader `<(string | ScriptCommand)>` — a name they can look up — instead
    // of `<(string | object)>`.
    Full(ScriptCommand),
}

/// The mapping form of a script entry: a command with a timeout, shell,
/// guard condition or working directory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScriptCommand {
    /// The command or script body to run.
    pub run: String,
    /// Kill the script if it runs longer than this duration (`"30s"`, `"2m"`).
    /// Unset means no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Kill the script if it produces no stdout/stderr output for this duration.
    /// Prevents scripts from silently hanging on unresponsive resources.
    /// Format: "30s", "2m", etc. If unset, no idle timeout is enforced.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "idleTimeout"
    )]
    pub idle_timeout: Option<String>,
    /// Treat a non-zero exit as success and continue reconciliation instead
    /// of failing the run. Default: `false`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "continueOnError"
    )]
    pub continue_on_error: Option<bool>,
    /// Interpreter to use for inline commands. Ignored (and rejected) on file scripts.
    #[serde(default, skip_serializing_if = "is_shell_auto")]
    pub shell: ScriptShell,
    /// Run the script only if this command exits zero. A non-zero exit skips
    /// the script (the condition for running was not met). Evaluated with the
    /// same shell, working directory, and environment as the body.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "onlyIf")]
    pub only_if: Option<String>,
    /// Run the script only if this command exits NON-zero. A zero exit
    /// (success) skips the script (the guarded state already holds).
    /// Evaluated with the same shell, working directory, and environment as
    /// the body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<String>,
    /// Skip the script if this path already exists. A leading `~` expands to
    /// the home directory; a relative path resolves against the script's
    /// working directory. Existence follows symlinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creates: Option<String>,
    /// Run the script attached to the terminal (inherited stdin/stdout/stderr,
    /// no spinner, no output capture, no idle timeout) so it can prompt the
    /// user — e.g. `echo "press Enter when done"; read`. Requires a TTY: when
    /// stdin is not a terminal (CI, piped input, or any daemon-run phase) the
    /// script is skipped with a warning rather than hanging on instant EOF.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interactive: bool,
    /// Working directory for the script. By default every lifecycle script
    /// runs in the user's home directory — never the config source tree — so
    /// a relative write can't pollute the user's GitOps repo. Set `workdir`
    /// to override: a leading `~` expands to home and `$VAR`/`${VAR}` expand
    /// against the script environment (which always carries `$CFGD_MODULE_DIR`
    /// and `$CFGD_CONFIG_DIR`), so `workdir: ~/.local/share/app`,
    /// `workdir: $CFGD_MODULE_DIR`, or an absolute path all work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

fn is_shell_auto(s: &ScriptShell) -> bool {
    *s == ScriptShell::Auto
}

impl ScriptEntry {
    /// Extract the run command string from any variant.
    pub fn run_str(&self) -> &str {
        match self {
            ScriptEntry::Simple(s) => s,
            ScriptEntry::Full(ScriptCommand { run, .. }) => run,
        }
    }
}

impl std::fmt::Display for ScriptEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.run_str())
    }
}

// --- Module Lockfile ---

/// Lockfile recording pinned remote modules with integrity hashes.
/// Stored at `<config_dir>/modules.lock`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleLockfile {
    /// Every locked remote module, one per module resolved from a registry.
    #[serde(default)]
    pub modules: Vec<ModuleLockEntry>,
}

/// A single locked remote module.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleLockEntry {
    /// Module name (matches metadata.name in the module spec).
    pub name: String,
    /// Git URL of the remote module repository.
    pub url: String,
    /// Pinned git ref — tag or commit SHA (branches not allowed for remote modules).
    pub pinned_ref: String,
    /// Resolved commit SHA at the time of locking.
    pub commit: String,
    /// SHA-256 hash of the module directory contents for integrity verification.
    pub integrity: String,
    /// Subdirectory within the repo containing the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
}

// --- Module Registries ---

/// A module registry — a git repo containing modules in `modules/<name>/module.yaml` structure.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleRegistryEntry {
    /// Short name / alias for this source (defaults to GitHub org name).
    pub name: String,
    /// Git URL of the registry repository, in any form git accepts — an HTTPS
    /// or SSH clone URL, or a GitHub `owner/repo` shorthand cfgd expands to the
    /// full URL. Required. The repository is cloned into the local cache and
    /// scanned for `modules/<name>/module.yaml` entries, which is what
    /// `cfgd module search` and `cfgd module add` resolve against.
    pub url: String,
}

/// Parse a Module document from YAML content.
pub fn parse_module(contents: &str) -> Result<ModuleDocument> {
    check_yaml_anchor_limit(contents, Path::new("Module"))?;
    let doc: ModuleDocument = serde_yaml::from_str(contents).map_err(ConfigError::from)?;

    if doc.kind != "Module" {
        return Err(ConfigError::Invalid {
            message: format!("expected kind 'Module', got '{}'", doc.kind),
        }
        .into());
    }
    super::parse::validate_api_version(&doc.api_version)?;
    validate_module_file_entries(&doc.spec.files)?;

    Ok(doc)
}

impl crate::platform::PlatformGated for ModuleSpec {
    fn platforms(&self) -> &[String] {
        &self.platforms
    }
}

impl crate::platform::PlatformGated for ModulePackageEntry {
    fn platforms(&self) -> &[String] {
        &self.platforms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_spec_rejects_unknown_field() {
        let yaml = "depends: []\nbogus: 1\n";
        let err = serde_yaml::from_str::<ModuleSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogus");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn module_document_rejects_unknown_top_level_field() {
        let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
bogusField: nope
metadata:
  name: m
spec: {}
"#;
        let err = serde_yaml::from_str::<ModuleDocument>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogusField");
        let msg = format!("{}", err);
        assert!(
            msg.contains("unknown field") && msg.contains("bogusField"),
            "expected unknown-field error mentioning bogusField, got: {msg}"
        );
    }

    fn module_yaml_with_version(version_line: &str) -> String {
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\n{version_line}spec: {{}}\n"
        )
    }

    #[test]
    fn module_metadata_version_parses_and_round_trips() {
        let doc = parse_module(&module_yaml_with_version("  version: \"1.2.3\"\n"))
            .expect("semver version should parse");
        assert_eq!(doc.metadata.version.as_deref(), Some("1.2.3"));

        let round_tripped = serde_yaml::to_string(&doc).expect("document should serialize");
        assert!(
            round_tripped.contains("version: 1.2.3"),
            "version must survive a round trip, got: {round_tripped}"
        );
    }

    #[test]
    fn module_metadata_version_accepts_prerelease_and_build() {
        for version in ["2.0.0-rc.1", "1.0.0+build.5", "0.1.0-alpha.2+sha.abc"] {
            let doc = parse_module(&module_yaml_with_version(&format!(
                "  version: \"{version}\"\n"
            )))
            .unwrap_or_else(|e| panic!("{version} should parse: {e}"));
            assert_eq!(doc.metadata.version.as_deref(), Some(version));
        }
    }

    #[test]
    fn module_metadata_version_rejects_non_semver() {
        for version in ["0.10", "v1.2.3", "", "1", "latest"] {
            let err = parse_module(&module_yaml_with_version(&format!(
                "  version: \"{version}\"\n"
            )))
            .expect_err(&format!("{version} must be rejected"));
            let msg = err.to_string();
            assert!(
                msg.contains("metadata.version") && msg.contains(version),
                "error must name the field and the offending value, got: {msg}"
            );
        }
    }

    #[test]
    fn module_metadata_version_is_optional() {
        let doc =
            parse_module(&module_yaml_with_version("")).expect("module without version must parse");
        assert!(doc.metadata.version.is_none());

        let round_tripped = serde_yaml::to_string(&doc).expect("document should serialize");
        assert!(
            !round_tripped.contains("version:"),
            "an absent version must not be materialized on write, got: {round_tripped}"
        );
    }

    #[test]
    fn module_file_entry_patch_ensure_parses_for_each_format() {
        for fmt in ["ini", "json", "yaml", "toml"] {
            let yaml = format!(
                "target: /tmp/settings.{fmt}\nstrategy: patch\npatch:\n  format: {fmt}\n  ensure:\n    General:\n      theme: dark\n"
            );
            let entry: ModuleFileEntry = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("format {fmt} should parse: {e}"));
            assert_eq!(entry.strategy, Some(FileStrategy::Patch));
            let patch = entry.patch.as_ref().expect("patch block should be present");
            assert!(patch.ensure.is_some());
            assert!(patch.script.is_none());
            validate_module_file_entries(std::slice::from_ref(&entry))
                .unwrap_or_else(|e| panic!("format {fmt} should validate: {e}"));
        }
    }

    #[test]
    fn module_file_entry_patch_script_parses() {
        let yaml = "target: ~/.zshrc\nstrategy: patch\npatch:\n  script: scripts/patch-zshrc.sh\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.strategy, Some(FileStrategy::Patch));
        let patch = entry.patch.as_ref().expect("patch block should be present");
        assert!(patch.script.is_some());
        assert!(patch.ensure.is_none());
        assert_eq!(entry.source, "");
        validate_module_file_entries(&[entry]).expect("script-mode patch should validate");
    }

    #[test]
    fn module_file_entry_patch_rejects_ensure_and_script_together() {
        let yaml =
            "target: /tmp/a.ini\nstrategy: patch\npatch:\n  ensure:\n    a: b\n  script: x.sh\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(
            err.to_string()
                .contains("exactly one of 'ensure' or 'script'")
        );
    }

    #[test]
    fn module_file_entry_patch_rejects_neither_ensure_nor_script() {
        let yaml = "target: /tmp/a.ini\nstrategy: patch\npatch: {}\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(
            err.to_string()
                .contains("exactly one of 'ensure' or 'script'")
        );
    }

    #[test]
    fn module_file_entry_patch_block_without_patch_strategy_rejected() {
        let yaml = "source: a\ntarget: /tmp/a.ini\nstrategy: copy\npatch:\n  ensure:\n    a: b\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(
            err.to_string()
                .contains("only valid when strategy is 'patch'")
        );
    }

    /// See the `ManagedFileSpec` counterparts: a `Patch` entry has no source
    /// file, so neither `encryption` nor `private` can be honoured.
    #[test]
    fn module_file_entry_patch_rejects_encryption() {
        let yaml = "target: /tmp/a.ini\nstrategy: patch\npatch:\n  ensure:\n    a: b\nencryption:\n  backend: sops\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(
            err.to_string()
                .contains("'encryption' is not supported with strategy 'patch'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn module_file_entry_patch_rejects_private() {
        let yaml =
            "target: /tmp/a.ini\nstrategy: patch\nprivate: true\npatch:\n  ensure:\n    a: b\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        assert!(entry.private);
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(
            err.to_string()
                .contains("'private' is not supported with strategy 'patch'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn module_file_entry_patch_strategy_without_patch_block_rejected() {
        let yaml = "target: /tmp/a.ini\nstrategy: patch\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(err.to_string().contains("requires a 'patch' block"));
    }

    #[test]
    fn module_file_entry_non_patch_strategy_requires_nonempty_source() {
        let yaml = "target: /tmp/a.ini\nstrategy: copy\n";
        let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.source, "");
        let err = validate_module_file_entries(&[entry]).unwrap_err();
        assert!(
            err.to_string()
                .contains("'source' is required unless strategy is 'patch'")
        );
    }

    #[test]
    fn module_document_with_invalid_patch_file_entry_rejected_by_parse_module() {
        let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: m
spec:
  files:
    - target: /tmp/a.ini
      strategy: patch
"#;
        let err = parse_module(yaml).unwrap_err();
        assert!(err.to_string().contains("requires a 'patch' block"));
    }

    #[test]
    fn module_document_with_valid_patch_file_entry_parses() {
        let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: m
spec:
  files:
    - target: /tmp/a.ini
      strategy: patch
      patch:
        format: ini
        ensure:
          General:
            theme: dark
"#;
        let doc = parse_module(yaml).expect("valid patch file entry should parse");
        assert_eq!(doc.spec.files.len(), 1);
    }

    #[test]
    fn unknown_apiversion_is_rejected_with_actionable_error() {
        let yaml = "apiVersion: cfgd.io/v1alpha2\nkind: Module\nmetadata:\n  name: m\nspec: {}\n";
        let err = crate::config::parse_module(yaml).unwrap_err();
        assert!(err.to_string().contains("apiVersion"));
        assert!(err.to_string().contains("cfgd.io/v1alpha1")); // tells the user the supported version
    }

    #[test]
    fn unknown_apiversion_is_rejected_for_config_source() {
        let yaml =
            "apiVersion: cfgd.io/v1alpha2\nkind: ConfigSource\nmetadata:\n  name: s\nspec: {}\n";
        let err = crate::config::parse_config_source(yaml).unwrap_err();
        assert!(err.to_string().contains("apiVersion"));
        assert!(err.to_string().contains("cfgd.io/v1alpha1"));
    }

    #[test]
    fn module_spec_emits_json_schema() {
        let schema = schemars::schema_for!(ModuleSpec);
        let json = serde_json::to_value(&schema).unwrap();
        assert!(
            json["properties"].get("packages").is_some(),
            "ModuleSpec schema missing packages property: {json}"
        );
    }

    #[test]
    fn script_entry_untagged_renders_anyof() {
        // ScriptEntry is `#[serde(untagged)]`; schemars 0.8 must render it as an
        // anyOf covering both the Simple(String) and Full{..} shapes, or the
        // field would vanish from the generated schema.
        let schema = schemars::schema_for!(ScriptEntry);
        let json = serde_json::to_value(&schema).unwrap();
        let any_of = json["anyOf"]
            .as_array()
            .expect("untagged ScriptEntry must render a top-level anyOf");
        assert!(
            any_of.len() >= 2,
            "anyOf must cover Simple + Full variants, got: {json}"
        );
    }

    #[test]
    fn module_spec_platforms_deserializes() {
        let yaml = "platforms: [macos]\n";
        let spec: ModuleSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.platforms, vec!["macos".to_string()]);
    }

    #[test]
    fn module_spec_platforms_absent_is_empty_and_not_serialized() {
        let spec: ModuleSpec = serde_yaml::from_str("depends: []\n").unwrap();
        assert!(spec.platforms.is_empty());

        let spec = ModuleSpec {
            platforms: vec!["macos".to_string()],
            ..Default::default()
        };
        let out = serde_yaml::to_string(&spec).unwrap();
        assert!(
            out.contains("platforms"),
            "platforms should serialize: {out}"
        );
        let roundtripped: ModuleSpec = serde_yaml::from_str(&out).unwrap();
        assert_eq!(roundtripped.platforms, vec!["macos".to_string()]);

        // Empty platforms must not appear in serialized output.
        let empty = ModuleSpec::default();
        let out = serde_yaml::to_string(&empty).unwrap();
        assert!(
            !out.contains("platforms"),
            "empty platforms should be skipped: {out}"
        );
    }

    #[test]
    fn script_entry_full_deserializes_shell_field() {
        let yaml = r#"
run: echo hello
shell: zsh
"#;
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match entry {
            ScriptEntry::Full(ScriptCommand { shell, run, .. }) => {
                assert_eq!(shell, ScriptShell::Zsh);
                assert_eq!(run, "echo hello");
            }
            other => panic!("expected Full variant, got: {other:?}"),
        }
    }

    #[test]
    fn script_entry_full_shell_defaults_to_auto() {
        let yaml = r#"
run: echo hello
"#;
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match entry {
            ScriptEntry::Full(ScriptCommand { shell, .. }) => {
                assert_eq!(shell, ScriptShell::Auto);
            }
            other => panic!("expected Full variant, got: {other:?}"),
        }
    }

    #[test]
    fn script_entry_unknown_shell_variant_rejected() {
        let yaml = r#"
run: echo hello
shell: ruby
"#;
        let err = serde_yaml::from_str::<ScriptEntry>(yaml)
            .expect_err("unknown shell variant must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("did not match any variant"),
            "error should indicate parse failure: {msg}"
        );
    }

    #[test]
    fn script_shell_roundtrip_serialization() {
        let entry = ScriptEntry::Full(ScriptCommand {
            workdir: None,
            run: "make build".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Bash,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        });
        let yaml = serde_yaml::to_string(&entry).unwrap();
        assert!(
            yaml.contains("shell: bash"),
            "yaml should contain 'shell: bash': {yaml}"
        );

        let roundtripped: ScriptEntry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(entry, roundtripped);
    }

    #[test]
    fn script_shell_auto_not_serialized() {
        let entry = ScriptEntry::Full(ScriptCommand {
            workdir: None,
            run: "echo hi".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        });
        let yaml = serde_yaml::to_string(&entry).unwrap();
        assert!(
            !yaml.contains("shell"),
            "Auto shell should be skipped in serialization: {yaml}"
        );
    }

    #[test]
    fn script_workdir_roundtrip() {
        let yaml = r#"
run: touch .marker
workdir: ~/.local/share/clift
"#;
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match &entry {
            ScriptEntry::Full(ScriptCommand { workdir, .. }) => {
                assert_eq!(workdir.as_deref(), Some("~/.local/share/clift"));
            }
            other => panic!("expected Full variant, got {other:?}"),
        }
        // Round-trips and is omitted from output when unset.
        let yaml_out = serde_yaml::to_string(&entry).unwrap();
        assert!(yaml_out.contains("workdir: ~/.local/share/clift"));
        let bare: ScriptEntry = serde_yaml::from_str("run: echo hi\n").unwrap();
        assert!(!serde_yaml::to_string(&bare).unwrap().contains("workdir"));
    }

    #[test]
    fn script_guards_roundtrip() {
        let yaml = r#"
run: install-thing
onlyIf: test -d /opt
unless: command -v thing
creates: ~/.local/bin/thing
"#;
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match &entry {
            ScriptEntry::Full(ScriptCommand {
                only_if,
                unless,
                creates,
                ..
            }) => {
                assert_eq!(only_if.as_deref(), Some("test -d /opt"));
                assert_eq!(unless.as_deref(), Some("command -v thing"));
                assert_eq!(creates.as_deref(), Some("~/.local/bin/thing"));
            }
            other => panic!("expected Full variant, got: {other:?}"),
        }

        let out = serde_yaml::to_string(&entry).unwrap();
        assert!(out.contains("onlyIf: test -d /opt"), "onlyIf: {out}");
        assert!(out.contains("unless: command -v thing"), "unless: {out}");
        assert!(
            out.contains("creates: ~/.local/bin/thing"),
            "creates: {out}"
        );
        let roundtripped: ScriptEntry = serde_yaml::from_str(&out).unwrap();
        assert_eq!(entry, roundtripped);
    }

    #[test]
    fn script_guards_absent_are_none() {
        let yaml = "run: echo hi\n";
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match &entry {
            ScriptEntry::Full(ScriptCommand {
                only_if,
                unless,
                creates,
                ..
            }) => {
                assert!(only_if.is_none());
                assert!(unless.is_none());
                assert!(creates.is_none());
            }
            other => panic!("expected Full variant, got: {other:?}"),
        }
        // Absent guards must not serialize.
        let out = serde_yaml::to_string(&entry).unwrap();
        assert!(!out.contains("onlyIf"), "onlyIf should be absent: {out}");
        assert!(!out.contains("unless"), "unless should be absent: {out}");
        assert!(!out.contains("creates"), "creates should be absent: {out}");
    }

    #[test]
    fn module_package_entry_parses_script_guards() {
        let yaml = r#"
name: rustup
prefer: [script]
script: curl -sSf https://sh.rustup.rs | sh
creates: ~/.cargo/bin/rustc
onlyIf: test -d /opt
unless: command -v rustc
"#;
        let entry: ModulePackageEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.prefer, vec!["script".to_string()]);
        assert_eq!(entry.creates.as_deref(), Some("~/.cargo/bin/rustc"));
        assert_eq!(entry.only_if.as_deref(), Some("test -d /opt"));
        assert_eq!(entry.unless.as_deref(), Some("command -v rustc"));
    }

    #[test]
    fn module_package_entry_guards_default_absent() {
        let yaml = "name: ripgrep\n";
        let entry: ModulePackageEntry = serde_yaml::from_str(yaml).unwrap();
        assert!(entry.creates.is_none());
        assert!(entry.only_if.is_none());
        assert!(entry.unless.is_none());

        // Absent guards must not serialize.
        let out = serde_yaml::to_string(&entry).unwrap();
        assert!(!out.contains("creates"), "creates should be absent: {out}");
        assert!(!out.contains("onlyIf"), "onlyIf should be absent: {out}");
        assert!(!out.contains("unless"), "unless should be absent: {out}");
    }

    #[test]
    fn module_package_entry_guards_roundtrip_camelcase() {
        let entry = ModulePackageEntry {
            name: "thing".into(),
            prefer: vec!["script".into()],
            script: Some("install-thing".into()),
            creates: Some("~/.local/bin/thing".into()),
            only_if: Some("test -d /opt".into()),
            unless: Some("command -v thing".into()),
            ..Default::default()
        };
        let out = serde_yaml::to_string(&entry).unwrap();
        assert!(out.contains("onlyIf: test -d /opt"), "onlyIf: {out}");
        let roundtripped: ModulePackageEntry = serde_yaml::from_str(&out).unwrap();
        assert_eq!(roundtripped.creates, entry.creates);
        assert_eq!(roundtripped.only_if, entry.only_if);
        assert_eq!(roundtripped.unless, entry.unless);
    }

    #[test]
    fn module_package_entry_rejects_unknown_field() {
        let yaml = "name: x\nbogusGuard: nope\n";
        let err = serde_yaml::from_str::<ModulePackageEntry>(yaml)
            .expect_err("deny_unknown_fields must reject bogusGuard");
        assert!(format!("{err}").contains("unknown field"));
    }

    #[test]
    fn script_simple_bare_string_still_parses() {
        let yaml = "make build\n";
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry, ScriptEntry::Simple("make build".into()));
    }

    #[test]
    fn script_entry_full_deserializes_interactive_field() {
        let yaml = r#"
run: 'echo press Enter; read'
interactive: true
"#;
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match entry {
            ScriptEntry::Full(ScriptCommand { interactive, .. }) => assert!(interactive),
            other => panic!("expected Full variant, got: {other:?}"),
        }
    }

    #[test]
    fn script_entry_interactive_defaults_to_false() {
        let yaml = "run: echo hi\n";
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match entry {
            ScriptEntry::Full(ScriptCommand { interactive, .. }) => assert!(!interactive),
            other => panic!("expected Full variant, got: {other:?}"),
        }
        // Default-false must not serialize.
        let out = serde_yaml::to_string(&entry).unwrap();
        assert!(
            !out.contains("interactive"),
            "interactive should be absent when false: {out}"
        );
    }

    #[test]
    fn script_interactive_roundtrip_serialization() {
        let entry = ScriptEntry::Full(ScriptCommand {
            workdir: None,
            run: "echo hi; read".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: true,
        });
        let yaml = serde_yaml::to_string(&entry).unwrap();
        assert!(
            yaml.contains("interactive: true"),
            "yaml should contain 'interactive: true': {yaml}"
        );
        let roundtripped: ScriptEntry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(entry, roundtripped);
    }

    #[test]
    fn script_shell_parses_case_insensitively() {
        for (token, expected) in [
            ("auto", ScriptShell::Auto),
            ("AUTO", ScriptShell::Auto),
            ("Auto", ScriptShell::Auto),
            ("sh", ScriptShell::Sh),
            ("bash", ScriptShell::Bash),
            ("BASH", ScriptShell::Bash),
            ("zsh", ScriptShell::Zsh),
            ("pwsh", ScriptShell::Pwsh),
            ("Pwsh", ScriptShell::Pwsh),
            ("cmd", ScriptShell::Cmd),
            ("CMD", ScriptShell::Cmd),
        ] {
            let parsed: ScriptShell = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(parsed, expected, "token {token}");
        }
    }

    #[test]
    fn script_shell_rejects_garbage() {
        serde_yaml::from_str::<ScriptShell>("fish").expect_err("unknown ScriptShell must error");
    }

    #[test]
    fn script_shell_serializes_canonical_camelcase() {
        let s = serde_yaml::to_string(&ScriptShell::Auto).expect("serialize");
        assert_eq!(s.trim(), "auto");
    }
}
