use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::parse::check_yaml_anchor_limit;
use super::profile_spec::{EncryptionSpec, FileStrategy, ScriptSpec};
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};

// --- Module ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ModuleMetadata,
    pub spec: ModuleSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleMetadata {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<ModulePackageEntry>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ModuleFileEntry>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<ShellAlias>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<ScriptSpec>,

    /// System configurator settings contributed by this module.
    /// Deep-merged into the profile system map; module values override profile values at leaf level.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub system: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModulePackageEntry {
    #[serde(default)]
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<String>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub aliases: HashMap<String, String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleFileEntry {
    pub source: String,
    pub target: String,
    /// Per-file deployment strategy override. If None, uses the global default.
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
}

/// Interpreter for inline lifecycle scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScriptEntry {
    Simple(String),
    Full {
        run: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
        /// Kill the script if it produces no stdout/stderr output for this duration.
        /// Prevents scripts from silently hanging on unresponsive resources.
        /// Format: "30s", "2m", etc. If unset, no idle timeout is enforced.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "idleTimeout"
        )]
        idle_timeout: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "continueOnError"
        )]
        continue_on_error: Option<bool>,
        /// Interpreter to use for inline commands. Ignored (and rejected) on file scripts.
        #[serde(default, skip_serializing_if = "is_shell_auto")]
        shell: ScriptShell,
        /// Run the script only if this command exits zero. A non-zero exit skips
        /// the script (the condition for running was not met). Evaluated with the
        /// same shell, working directory, and environment as the body.
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "onlyIf")]
        only_if: Option<String>,
        /// Run the script only if this command exits NON-zero. A zero exit
        /// (success) skips the script (the guarded state already holds).
        /// Evaluated with the same shell, working directory, and environment as
        /// the body.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unless: Option<String>,
        /// Skip the script if this path already exists. A leading `~` expands to
        /// the home directory; a relative path resolves against the script's
        /// working directory. Existence follows symlinks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        creates: Option<String>,
        /// Run the script attached to the terminal (inherited stdin/stdout/stderr,
        /// no spinner, no output capture, no idle timeout) so it can prompt the
        /// user — e.g. `echo "press Enter when done"; read`. Requires a TTY: when
        /// stdin is not a terminal (CI, piped input, or any daemon-run phase) the
        /// script is skipped with a warning rather than hanging on instant EOF.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interactive: bool,
    },
}

fn is_shell_auto(s: &ScriptShell) -> bool {
    *s == ScriptShell::Auto
}

impl ScriptEntry {
    /// Extract the run command string from any variant.
    pub fn run_str(&self) -> &str {
        match self {
            ScriptEntry::Simple(s) => s,
            ScriptEntry::Full { run, .. } => run,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleLockfile {
    #[serde(default)]
    pub modules: Vec<ModuleLockEntry>,
}

/// A single locked remote module.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleRegistryEntry {
    /// Short name / alias for this source (defaults to GitHub org name).
    pub name: String,
    /// Git URL of the source repository.
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

    Ok(doc)
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

    #[test]
    fn script_entry_full_deserializes_shell_field() {
        let yaml = r#"
run: echo hello
shell: zsh
"#;
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match entry {
            ScriptEntry::Full { shell, run, .. } => {
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
            ScriptEntry::Full { shell, .. } => {
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
        let entry = ScriptEntry::Full {
            run: "make build".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Bash,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };
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
        let entry = ScriptEntry::Full {
            run: "echo hi".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: false,
        };
        let yaml = serde_yaml::to_string(&entry).unwrap();
        assert!(
            !yaml.contains("shell"),
            "Auto shell should be skipped in serialization: {yaml}"
        );
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
            ScriptEntry::Full {
                only_if,
                unless,
                creates,
                ..
            } => {
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
            ScriptEntry::Full {
                only_if,
                unless,
                creates,
                ..
            } => {
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
            ScriptEntry::Full { interactive, .. } => assert!(interactive),
            other => panic!("expected Full variant, got: {other:?}"),
        }
    }

    #[test]
    fn script_entry_interactive_defaults_to_false() {
        let yaml = "run: echo hi\n";
        let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
        match entry {
            ScriptEntry::Full { interactive, .. } => assert!(!interactive),
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
        let entry = ScriptEntry::Full {
            run: "echo hi; read".into(),
            timeout: None,
            idle_timeout: None,
            continue_on_error: None,
            shell: ScriptShell::Auto,
            only_if: None,
            unless: None,
            creates: None,
            interactive: true,
        };
        let yaml = serde_yaml::to_string(&entry).unwrap();
        assert!(
            yaml.contains("interactive: true"),
            "yaml should contain 'interactive: true': {yaml}"
        );
        let roundtripped: ScriptEntry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(entry, roundtripped);
    }
}
