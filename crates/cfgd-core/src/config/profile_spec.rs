use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use super::module::ScriptEntry;
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};

/// A package-manager spec struct that can be built from a bare list of package
/// names, so a manager accepts both `manager: [a, b]` and
/// `manager: {<knobs>}`. The bare list maps to the manager's primary list
/// field; every other field takes its default.
pub trait FromPackageList {
    /// Build the spec from a bare list of package names.
    fn from_package_list(packages: Vec<String>) -> Self;
}

/// Accept either a YAML sequence (the package list) or a map with a `packages:`
/// key (rejecting any other key) for a field whose type stays `Vec<String>`.
///
/// This gives the 12 bare-`Vec<String>` managers a struct form
/// (`manager: {packages: [...]}`) without changing their field type, so the
/// list and struct forms are interchangeable by construction. A map with an
/// unrecognized key still errors loudly (typo-detection preserved) — a hand
/// visitor is used rather than `#[serde(untagged)]` because untagged collapses
/// the precise `unknown field` error into a useless "did not match any variant".
fn list_or_packages_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PackagesMap {
        #[serde(default)]
        packages: Vec<String>,
    }

    struct ListOrPackagesVisitor;

    impl<'de> de::Visitor<'de> for ListOrPackagesVisitor {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list of package names or a map with a `packages` key")
        }

        // A serialized-then-reloaded empty manager surfaces as `manager: null`;
        // treat it as the empty list so round-trips stay lossless.
        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Vec::new())
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut packages = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                packages.push(item);
            }
            Ok(packages)
        }

        fn visit_map<M>(self, map: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let m = PackagesMap::deserialize(de::value::MapAccessDeserializer::new(map))?;
            Ok(m.packages)
        }
    }

    deserializer.deserialize_any(ListOrPackagesVisitor)
}

/// Accept either a YAML sequence (→ `T::from_package_list`) or a map (→ derived
/// `T`) for an `Option<T>` field. An absent field stays `None` (via the field's
/// `#[serde(default)]`); a present value is resolved as list-or-map through one
/// shared mechanism for every struct-backed manager.
///
/// A hand visitor (not `#[serde(untagged)]`) preserves `T`'s precise
/// `deny_unknown_fields` error on a typo'd map key.
fn list_or_struct<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + FromPackageList,
{
    use std::marker::PhantomData;

    use serde::de;

    // The visitor yields `Option<U>` so `manager: null` (a serialized-then-
    // reloaded empty manager) round-trips back to `None`, matching the prior
    // `Option<XSpec>` behavior, while a sequence or map resolves to `Some`.
    struct ListOrStructVisitor<U>(PhantomData<U>);

    impl<'de, U> de::Visitor<'de> for ListOrStructVisitor<U>
    where
        U: Deserialize<'de> + FromPackageList,
    {
        type Value = Option<U>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list of package names, a manager spec map, or null")
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D2>(self, deserializer: D2) -> std::result::Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut packages = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                packages.push(item);
            }
            Ok(Some(U::from_package_list(packages)))
        }

        fn visit_map<M>(self, map: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            U::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_option(ListOrStructVisitor(PhantomData))
}
// --- Profile ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ProfileMetadata,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSpec {
    #[serde(default)]
    pub inherits: Vec<String>,

    #[serde(default)]
    pub modules: Vec<String>,

    #[serde(default)]
    pub env: Vec<EnvVar>,

    /// How far `spec.env` exports reach across the current user's environment.
    /// Omitted means "inherit" (a parent layer's value survives); the resolved
    /// default when no layer sets it is [`EnvScope::All`] — every standard user
    /// entry point cfgd can safely touch. Narrow it to `Login` or `Interactive`
    /// to opt out of the broader session surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_scope: Option<EnvScope>,

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

/// How far `spec.env` exports reach across the current user's environment.
///
/// The two env fields differ by *scope of affected users*: `spec.env` targets
/// the current user, `spec.system.environment` targets all users (privileged).
/// This knob narrows the *current-user* reach; it never widens beyond the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EnvScope {
    /// Every standard user entry point cfgd can safely touch: interactive +
    /// login shells, `systemd --user` / Wayland GUI sessions, macOS GUI apps,
    /// and an immediate live-session refresh. The default — no gotchas.
    #[default]
    All,
    /// Interactive shells plus login shells (`~/.zshenv`, `~/.profile`, and an
    /// existing `~/.bash_profile`). Excludes the GUI / `systemd --user` session
    /// surfaces and the live-session refresh.
    Login,
    /// Interactive shells only (`~/.bashrc` / `~/.zshrc`, fish conf.d) — the
    /// historical behavior before full reach.
    Interactive,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackagesSpec {
    #[serde(default, deserialize_with = "list_or_struct")]
    pub brew: Option<BrewSpec>,
    #[serde(default, deserialize_with = "list_or_struct")]
    pub apt: Option<AptSpec>,
    #[serde(default, deserialize_with = "list_or_struct")]
    pub cargo: Option<CargoSpec>,
    #[serde(default, deserialize_with = "list_or_struct")]
    pub npm: Option<NpmSpec>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub pipx: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub dnf: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub apk: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub pacman: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub zypper: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub yum: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub pkg: Vec<String>,
    #[serde(default, deserialize_with = "list_or_struct")]
    pub snap: Option<SnapSpec>,
    #[serde(default, deserialize_with = "list_or_struct")]
    pub flatpak: Option<FlatpakSpec>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub nix: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub go: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub winget: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub chocolatey: Vec<String>,
    #[serde(default, deserialize_with = "list_or_packages_vec")]
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

    /// Return the names of every package manager that has at least one entry,
    /// including the virtual `brew-tap` / `brew-cask` managers and any custom
    /// managers. Order is stable but not significant.
    ///
    /// Built-in managers are walked from [`crate::config::ALL_MANAGER_NAMES`] and
    /// kept when [`crate::config::desired_packages_for_spec`] yields a non-empty
    /// list, so this never re-encodes which field each manager reads.
    pub fn manager_names(&self) -> Vec<String> {
        let mut names: Vec<String> = crate::config::ALL_MANAGER_NAMES
            .iter()
            .filter(|name| !crate::config::desired_packages_for_spec(name, self).is_empty())
            .map(|name| name.to_string())
            .collect();
        for custom in &self.custom {
            if !custom.packages.is_empty() {
                names.push(custom.name.clone());
            }
        }
        names
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

impl FromPackageList for BrewSpec {
    fn from_package_list(packages: Vec<String>) -> Self {
        // Bare `brew: [...]` is the common case: formulae. Taps and casks
        // remain struct-only knobs.
        BrewSpec {
            formulae: packages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AptSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

impl FromPackageList for AptSpec {
    fn from_package_list(packages: Vec<String>) -> Self {
        AptSpec {
            packages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NpmSpec {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub global: Vec<String>,
}

impl FromPackageList for NpmSpec {
    fn from_package_list(packages: Vec<String>) -> Self {
        // Bare `npm: [...]` maps to globally-installed packages.
        NpmSpec {
            global: packages,
            ..Default::default()
        }
    }
}

/// Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`)
/// and object form (`cargo: { file: Cargo.toml, packages: [...] }`) via the
/// shared `list_or_struct` deserializer on the `PackagesSpec::cargo` field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
}

impl FromPackageList for CargoSpec {
    fn from_package_list(packages: Vec<String>) -> Self {
        CargoSpec {
            packages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub classic: Vec<String>,
}

impl FromPackageList for SnapSpec {
    fn from_package_list(packages: Vec<String>) -> Self {
        SnapSpec {
            packages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlatpakSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub remote: Option<String>,
}

impl FromPackageList for FlatpakSpec {
    fn from_package_list(packages: Vec<String>) -> Self {
        FlatpakSpec {
            packages,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EncryptionSpec {
    /// The encryption backend to use (e.g. "sops", "age").
    pub backend: String,
    /// When encryption must be enforced. Defaults to `InRepo`.
    #[serde(default)]
    pub mode: EncryptionMode,
}

/// Encryption constraint applied to files from a config source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_spec_rejects_unknown_field() {
        let yaml = "modules: []\nbogus: 1\n";
        let err = serde_yaml::from_str::<ProfileSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogus");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn packages_spec_rejects_typo_for_known_manager() {
        // `brwe:` typo (meant `brew:`) must error loudly, not silently drop.
        let yaml = "brwe:\n  formulae: [ripgrep]\n";
        let err = serde_yaml::from_str::<PackagesSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject brwe typo");
        let msg = format!("{}", err);
        assert!(
            msg.contains("unknown field") && msg.contains("brwe"),
            "expected unknown-field error mentioning brwe, got: {msg}"
        );
    }

    #[test]
    fn managed_file_spec_rejects_unknown_field() {
        let yaml = "source: a\ntarget: /tmp/b\nbogus: 1\n";
        let err = serde_yaml::from_str::<ManagedFileSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogus");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn env_scope_omitted_is_none_so_inheritance_can_apply() {
        let spec = serde_yaml::from_str::<ProfileSpec>("modules: []\n").unwrap();
        assert_eq!(spec.env_scope, None);
    }

    #[test]
    fn env_scope_parses_pascal_case_variants() {
        let all = serde_yaml::from_str::<ProfileSpec>("envScope: All\n").unwrap();
        assert_eq!(all.env_scope, Some(EnvScope::All));
        let login = serde_yaml::from_str::<ProfileSpec>("envScope: Login\n").unwrap();
        assert_eq!(login.env_scope, Some(EnvScope::Login));
        let interactive = serde_yaml::from_str::<ProfileSpec>("envScope: Interactive\n").unwrap();
        assert_eq!(interactive.env_scope, Some(EnvScope::Interactive));
    }

    #[test]
    fn env_scope_rejects_unknown_variant() {
        serde_yaml::from_str::<ProfileSpec>("envScope: Everywhere\n")
            .expect_err("unknown EnvScope variant must error, not silently default");
    }

    #[test]
    fn env_scope_default_is_all() {
        assert_eq!(EnvScope::default(), EnvScope::All);
    }
}
