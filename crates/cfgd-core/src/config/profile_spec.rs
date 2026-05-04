use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::module::ScriptEntry;
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};
// --- Profile ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ProfileMetadata,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSpec {
    #[serde(default)]
    pub inherits: Vec<String>,

    #[serde(default)]
    pub modules: Vec<String>,

    #[serde(default)]
    pub env: Vec<EnvVar>,

    #[serde(default)]
    pub aliases: Vec<ShellAlias>,

    #[serde(default)]
    pub packages: Option<PackagesSpec>,

    #[serde(default)]
    pub files: Option<FilesSpec>,

    #[serde(default)]
    pub system: HashMap<String, serde_yaml::Value>,

    #[serde(default)]
    pub secrets: Vec<SecretSpec>,

    #[serde(default)]
    pub scripts: Option<ScriptSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagesSpec {
    #[serde(default)]
    pub brew: Option<BrewSpec>,
    #[serde(default)]
    pub apt: Option<AptSpec>,
    #[serde(default)]
    pub cargo: Option<CargoSpec>,
    #[serde(default)]
    pub npm: Option<NpmSpec>,
    #[serde(default)]
    pub pipx: Vec<String>,
    #[serde(default)]
    pub dnf: Vec<String>,
    #[serde(default)]
    pub apk: Vec<String>,
    #[serde(default)]
    pub pacman: Vec<String>,
    #[serde(default)]
    pub zypper: Vec<String>,
    #[serde(default)]
    pub yum: Vec<String>,
    #[serde(default)]
    pub pkg: Vec<String>,
    #[serde(default)]
    pub snap: Option<SnapSpec>,
    #[serde(default)]
    pub flatpak: Option<FlatpakSpec>,
    #[serde(default)]
    pub nix: Vec<String>,
    #[serde(default)]
    pub go: Vec<String>,
    #[serde(default)]
    pub winget: Vec<String>,
    #[serde(default)]
    pub chocolatey: Vec<String>,
    #[serde(default)]
    pub scoop: Vec<String>,
    #[serde(default)]
    pub custom: Vec<CustomManagerSpec>,
}

impl PackagesSpec {
    /// Return a mutable reference to the package list for a simple `Vec<String>` manager.
    /// Returns `None` for managers that use struct wrappers (brew, apt, cargo, npm, snap, flatpak)
    /// or for unknown manager names.
    pub fn simple_list_mut(&mut self, manager: &str) -> Option<&mut Vec<String>> {
        match manager {
            "pipx" => Some(&mut self.pipx),
            "dnf" => Some(&mut self.dnf),
            "apk" => Some(&mut self.apk),
            "pacman" => Some(&mut self.pacman),
            "zypper" => Some(&mut self.zypper),
            "yum" => Some(&mut self.yum),
            "pkg" => Some(&mut self.pkg),
            "nix" => Some(&mut self.nix),
            "go" => Some(&mut self.go),
            "winget" => Some(&mut self.winget),
            "chocolatey" => Some(&mut self.chocolatey),
            "scoop" => Some(&mut self.scoop),
            _ => None,
        }
    }

    /// Return a reference to the package list for a simple `Vec<String>` manager.
    /// Returns `None` for struct-wrapper managers or unknown names.
    pub fn simple_list(&self, manager: &str) -> Option<&[String]> {
        match manager {
            "pipx" => Some(&self.pipx),
            "dnf" => Some(&self.dnf),
            "apk" => Some(&self.apk),
            "pacman" => Some(&self.pacman),
            "zypper" => Some(&self.zypper),
            "yum" => Some(&self.yum),
            "pkg" => Some(&self.pkg),
            "nix" => Some(&self.nix),
            "go" => Some(&self.go),
            "winget" => Some(&self.winget),
            "chocolatey" => Some(&self.chocolatey),
            "scoop" => Some(&self.scoop),
            _ => None,
        }
    }

    /// Return all non-empty simple-list managers as `(name, packages)` pairs.
    pub fn non_empty_simple_lists(&self) -> Vec<(&str, &[String])> {
        let mut result = Vec::new();
        for name in &[
            "pipx",
            "dnf",
            "apk",
            "pacman",
            "zypper",
            "yum",
            "pkg",
            "nix",
            "go",
            "winget",
            "chocolatey",
            "scoop",
        ] {
            if let Some(list) = self.simple_list(name)
                && !list.is_empty()
            {
                result.push((*name, list));
            }
        }
        result
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrewSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub taps: Vec<String>,
    #[serde(default)]
    pub formulae: Vec<String>,
    #[serde(default)]
    pub casks: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AptSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpmSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub global: Vec<String>,
}

/// Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`)
/// and object form (`cargo: { file: Cargo.toml, packages: [...] }`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CargoSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

impl<'de> Deserialize<'de> for CargoSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CargoSpecFull {
            #[serde(default)]
            file: Option<String>,
            #[serde(default)]
            packages: Vec<String>,
        }

        // Try to deserialize as either a list of strings or a map with file/packages
        struct CargoSpecVisitor;

        impl<'de> de::Visitor<'de> for CargoSpecVisitor {
            type Value = CargoSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a list of package names or a map with file/packages keys")
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<CargoSpec, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut packages = Vec::new();
                while let Some(item) = seq.next_element::<String>()? {
                    packages.push(item);
                }
                Ok(CargoSpec {
                    file: None,
                    packages,
                })
            }

            fn visit_map<M>(self, map: M) -> std::result::Result<CargoSpec, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let full = CargoSpecFull::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(CargoSpec {
                    file: full.file,
                    packages: full.packages,
                })
            }
        }

        deserializer.deserialize_any(CargoSpecVisitor)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub classic: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatpakSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomManagerSpec {
    pub name: String,
    pub check: String,
    pub list_installed: String,
    pub install: String,
    pub uninstall: String,
    #[serde(default)]
    pub update: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesSpec {
    #[serde(default)]
    pub managed: Vec<ManagedFileSpec>,
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

/// File deployment strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FileStrategy {
    /// Create a symbolic link from target to source (default).
    #[default]
    Symlink,
    /// Copy source content to target.
    Copy,
    /// Render a Tera template and write the output (auto-selected for .tera files).
    Template,
    /// Create a hard link from target to source.
    Hardlink,
}

/// Controls when encryption is required for a managed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EncryptionMode {
    /// File must be encrypted when stored in the repository.
    #[default]
    InRepo,
    /// File must always be encrypted, including at rest on disk.
    Always,
}

/// Encryption settings for a managed file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionSpec {
    /// The encryption backend to use (e.g. "sops", "age").
    pub backend: String,
    /// When encryption must be enforced. Defaults to `InRepo`.
    #[serde(default)]
    pub mode: EncryptionMode,
}

/// Encryption constraint applied to files from a config source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionConstraint {
    /// Glob patterns or explicit paths that must be encrypted.
    #[serde(default)]
    pub required_targets: Vec<String>,
    /// If set, restrict which backend is acceptable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// If set, restrict which encryption mode is acceptable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<EncryptionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedFileSpec {
    pub source: String,
    pub target: PathBuf,
    /// Per-file deployment strategy override. If None, uses the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<FileStrategy>,
    /// When true, the source file is local-only: auto-added to .gitignore,
    /// silently skipped on machines where it doesn't exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Which source this file came from (None = local config).
    /// Used by the template sandbox to restrict variable access.
    #[serde(skip)]
    pub origin: Option<String>,
    /// Encryption settings for this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionSpec>,
    /// Unix permission bits (e.g. "600", "644") to apply after deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretSpec {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envs: Option<Vec<String>>,
}

/// Validate that each secret has at least one delivery target (`target` or `envs`).
pub fn validate_secret_specs(specs: &[SecretSpec]) -> Result<()> {
    for spec in specs {
        if spec.target.is_none() && spec.envs.as_ref().is_none_or(|e| e.is_empty()) {
            return Err(ConfigError::Invalid {
                message: format!(
                    "secret '{}' must have at least one of 'target' or 'envs'",
                    spec.source
                ),
            }
            .into());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSpec {
    #[serde(default)]
    pub pre_apply: Vec<ScriptEntry>,
    #[serde(default)]
    pub post_apply: Vec<ScriptEntry>,
    #[serde(default)]
    pub pre_reconcile: Vec<ScriptEntry>,
    #[serde(default)]
    pub post_reconcile: Vec<ScriptEntry>,
    #[serde(default)]
    pub on_drift: Vec<ScriptEntry>,
    #[serde(default)]
    pub on_change: Vec<ScriptEntry>,
}
