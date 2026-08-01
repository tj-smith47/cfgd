use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use super::module::ScriptEntry;
use super::source::{EnvVar, ShellAlias};
use crate::PathDisplayExt;
use crate::errors::{ConfigError, Result};

/// A package-manager spec struct that can be built from a bare list of package
/// names, so a manager accepts both `manager: [a, b]` and
/// `manager: {<knobs>}`. The bare list maps to the manager's primary list
/// field; every other field takes its default.
pub trait FromPackageList {
    /// Build the spec from a bare list of package names.
    fn from_package_list(packages: Vec<String>) -> Self;
}

/// Layer one manager spec's values onto another during profile composition.
///
/// Each implementor owns its own per-field merge semantics — scalar `Option`
/// fields are overwritten when the incoming side is `Some`, list fields are
/// union-extended (dedup, order-preserving). Centralizing the policy on the
/// type (rather than open-coding it in the composition engine) means a new
/// field on a manager spec is merged by editing only that struct's
/// `merge_from`, so the merge layer cannot silently drift per manager.
pub trait MergeSpec {
    /// Layer `other`'s values onto `self`.
    fn merge_from(&mut self, other: &Self);
}

impl MergeSpec for BrewSpec {
    fn merge_from(&mut self, other: &Self) {
        if other.file.is_some() {
            self.file = other.file.clone();
        }
        crate::union_extend(&mut self.taps, &other.taps);
        crate::union_extend(&mut self.formulae, &other.formulae);
        crate::union_extend(&mut self.casks, &other.casks);
    }
}

impl MergeSpec for AptSpec {
    fn merge_from(&mut self, other: &Self) {
        if other.file.is_some() {
            self.file = other.file.clone();
        }
        crate::union_extend(&mut self.packages, &other.packages);
    }
}

impl MergeSpec for CargoSpec {
    fn merge_from(&mut self, other: &Self) {
        if other.file.is_some() {
            self.file = other.file.clone();
        }
        crate::union_extend(&mut self.packages, &other.packages);
    }
}

impl MergeSpec for NpmSpec {
    fn merge_from(&mut self, other: &Self) {
        if other.file.is_some() {
            self.file = other.file.clone();
        }
        crate::union_extend(&mut self.global, &other.global);
    }
}

impl MergeSpec for SnapSpec {
    fn merge_from(&mut self, other: &Self) {
        crate::union_extend(&mut self.packages, &other.packages);
        crate::union_extend(&mut self.classic, &other.classic);
    }
}

impl MergeSpec for FlatpakSpec {
    fn merge_from(&mut self, other: &Self) {
        crate::union_extend(&mut self.packages, &other.packages);
        if other.remote.is_some() {
            self.remote = other.remote.clone();
        }
    }
}

impl MergeSpec for CustomManagerSpec {
    fn merge_from(&mut self, other: &Self) {
        self.check = other.check.clone();
        self.list_installed = other.list_installed.clone();
        self.install = other.install.clone();
        self.uninstall = other.uninstall.clone();
        if other.update.is_some() {
            self.update = other.update.clone();
        }
        crate::union_extend(&mut self.packages, &other.packages);
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileDocument {
    pub api_version: String,
    pub kind: String,
    pub metadata: ProfileMetadata,
    pub spec: ProfileSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileMetadata {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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
    #[schemars(with = "std::collections::HashMap<String, serde_json::Value>")]
    pub system: HashMap<String, serde_yaml::Value>,

    #[serde(default)]
    pub secrets: Vec<SecretSpec>,

    #[serde(default)]
    pub scripts: Option<ScriptSpec>,

    #[serde(default)]
    pub backups: Vec<BackupSpec>,
}

/// How far `spec.env` exports reach across the current user's environment.
///
/// The two env fields differ by *scope of affected users*: `spec.env` targets
/// the current user, `spec.system.environment` targets all users (privileged).
/// This knob narrows the *current-user* reach; it never widens beyond the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, schemars::JsonSchema)]
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

case_insensitive_enum!(EnvScope {
    "All" => EnvScope::All,
    "Login" => EnvScope::Login,
    "Interactive" => EnvScope::Interactive,
});

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesSpec {
    #[serde(default)]
    pub managed: Vec<ManagedFileSpec>,
    #[serde(default)]
    pub permissions: HashMap<String, String>,
}

/// File deployment strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, schemars::JsonSchema)]
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
    /// Merge structured keys/values into the target, or pipe it through a
    /// script, leaving everything else untouched. Requires a `modify:` block.
    Modify,
}

case_insensitive_enum!(FileStrategy {
    "Symlink" => FileStrategy::Symlink,
    "Copy" => FileStrategy::Copy,
    "Template" => FileStrategy::Template,
    "Hardlink" => FileStrategy::Hardlink,
    "Modify" => FileStrategy::Modify,
});

impl FileStrategy {
    /// Every variant, in declaration order. Keep in step with the enum and the
    /// `case_insensitive_enum!` token list above.
    pub const ALL: &'static [FileStrategy] = &[
        FileStrategy::Symlink,
        FileStrategy::Copy,
        FileStrategy::Template,
        FileStrategy::Hardlink,
        FileStrategy::Modify,
    ];

    /// Canonical PascalCase spelling — what cfgd serializes and what the
    /// published editor schemas offer.
    pub fn as_str(self) -> &'static str {
        match self {
            FileStrategy::Symlink => "Symlink",
            FileStrategy::Copy => "Copy",
            FileStrategy::Template => "Template",
            FileStrategy::Hardlink => "Hardlink",
            FileStrategy::Modify => "Modify",
        }
    }

    /// Whether the strategy is meaningful as the global `spec.fileStrategy`
    /// default.
    ///
    /// `Modify` is not: it is defined by a per-file `modify:` block, which a
    /// file inheriting the global default cannot have. The config parser and
    /// the published schema both derive their accepted value set from this, so
    /// an editor and `cfgd` can never disagree about it.
    pub fn valid_as_global_default(self) -> bool {
        !matches!(self, FileStrategy::Modify)
    }
}

/// File format used to interpret and re-serialize a `Modify`-strategy target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum ModifyFormat {
    /// INI sections/keys, edited line-by-line to preserve comments and layout.
    Ini,
    /// JSON, re-serialized on write (no comments to preserve).
    Json,
    /// YAML; comments are NOT preserved across a merge (see docs for the caveat).
    Yaml,
    /// TOML, edited via `toml_edit` to preserve comments and layout.
    Toml,
}

case_insensitive_enum!(ModifyFormat {
    "Ini" => ModifyFormat::Ini,
    "Json" => ModifyFormat::Json,
    "Yaml" => ModifyFormat::Yaml,
    "Toml" => ModifyFormat::Toml,
});

/// Configuration for the `Modify` file strategy: a structured merge (`ensure`)
/// or a content-rewriting script, applied on top of the target's current
/// content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModifySpec {
    /// File format to parse the target as. Inferred from the target's
    /// extension when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<ModifyFormat>,
    /// Keys/values to deep-merge into the target, leaving unmentioned keys
    /// untouched. Values are literal (no template rendering). Mutually
    /// exclusive with `script`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub ensure: Option<serde_yaml::Value>,
    /// A script path (relative to the module directory) or an inline command
    /// that receives the target's current content on stdin and writes the new
    /// content to stdout. Mutually exclusive with `ensure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

/// Controls when encryption is required for a managed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, schemars::JsonSchema)]
pub enum EncryptionMode {
    /// File must be encrypted when stored in the repository.
    #[default]
    InRepo,
    /// File must always be encrypted, including at rest on disk.
    Always,
}

case_insensitive_enum!(EncryptionMode {
    "InRepo" => EncryptionMode::InRepo,
    "Always" => EncryptionMode::Always,
});

/// Encryption settings for a managed file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EncryptionSpec {
    /// The encryption backend to use (e.g. "sops", "age").
    pub backend: String,
    /// When encryption must be enforced. Defaults to `InRepo`.
    #[serde(default)]
    pub mode: EncryptionMode,
}

/// Encryption constraint applied to files from a config source.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedFileSpec {
    /// Not required when `strategy` is `Modify`; required otherwise
    /// (enforced by `validate_managed_file_specs`, not the JSON schema).
    #[serde(default)]
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
    /// Structured merge or script configuration for `strategy: Modify`.
    /// Required when `strategy` is `Modify`, rejected otherwise (enforced by
    /// `validate_managed_file_specs`, not the JSON schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modify: Option<ModifySpec>,
}

// `target` XOR `envs` (at least one required) is enforced at runtime by
// `validate_secret_specs`, not in the JSON schema: both are plain `Option`
// fields, so the generated schema marks them optional. Expressing the XOR
// would require a hand-written `oneOf`, which would drift from this struct —
// the by-construction generation is the priority, runtime validation is the
// backstop.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

/// Deserialize a [`ProfileSpec`] from an in-memory YAML value.
///
/// Keeps `ProfileSpec` deserialization inside `config/` (the config-parsing
/// boundary) for callers that hold a pre-transformed value rather than raw
/// document text — e.g. composition turning a subscriber's `overrides` mapping
/// into a spec. `deny_unknown_fields` still surfaces typo'd keys as an error.
pub(crate) fn profile_spec_from_value(
    value: serde_yaml::Value,
) -> std::result::Result<ProfileSpec, serde_yaml::Error> {
    serde_yaml::from_value::<ProfileSpec>(value)
}

/// Validate the `source` / `strategy` / `modify` / `encryption` shape shared by
/// `ManagedFileSpec` and `ModuleFileEntry`: `source` is required unless
/// `strategy` is `Modify`; a `modify` block is required when `strategy` is
/// `Modify` and rejected otherwise; within a `modify` block exactly one of
/// `ensure`/`script` must be set; `encryption` is rejected on a `Modify` entry.
pub(crate) fn validate_file_modify_shape(
    subject: &str,
    source_is_empty: bool,
    strategy: Option<FileStrategy>,
    modify: Option<&ModifySpec>,
    encryption_declared: bool,
    private: bool,
) -> Result<()> {
    let is_modify = matches!(strategy, Some(FileStrategy::Modify));
    // `private` marks the SOURCE file local-only (gitignored, skipped where it
    // is absent). `Modify` has no source, so the flag can only ever be a no-op
    // that reads as a promise the strategy never keeps.
    if is_modify && private {
        return Err(ConfigError::Invalid {
            message: format!("{subject}: 'private' is not supported with strategy 'modify'"),
        }
        .into());
    }
    // Every `encryption` mode constrains the SOURCE file a strategy deploys
    // ("must be encrypted in the repo"). `Modify` has no source — it rewrites
    // the target's own plaintext structure — so the constraint could only be
    // silently ignored. Reject it instead of pretending it was honoured.
    if is_modify && encryption_declared {
        return Err(ConfigError::Invalid {
            message: format!("{subject}: 'encryption' is not supported with strategy 'modify'"),
        }
        .into());
    }
    match (is_modify, modify) {
        (true, None) => Err(ConfigError::Invalid {
            message: format!("{subject}: strategy 'modify' requires a 'modify' block"),
        }
        .into()),
        (false, Some(_)) => Err(ConfigError::Invalid {
            message: format!("{subject}: 'modify' is only valid when strategy is 'modify'"),
        }
        .into()),
        (true, Some(m)) => match (m.ensure.is_some(), m.script.is_some()) {
            (true, true) => Err(ConfigError::Invalid {
                message: format!(
                    "{subject}: 'modify' must set exactly one of 'ensure' or 'script', not both"
                ),
            }
            .into()),
            (false, false) => Err(ConfigError::Invalid {
                message: format!(
                    "{subject}: 'modify' must set exactly one of 'ensure' or 'script'"
                ),
            }
            .into()),
            _ => Ok(()),
        },
        (false, None) => {
            if source_is_empty {
                Err(ConfigError::Invalid {
                    message: format!("{subject}: 'source' is required unless strategy is 'modify'"),
                }
                .into())
            } else {
                Ok(())
            }
        }
    }
}

/// Validate the `modify` strategy shape of every managed file
/// (`spec.files.managed`). See [`validate_file_modify_shape`].
pub fn validate_managed_file_specs(specs: &[ManagedFileSpec]) -> Result<()> {
    for spec in specs {
        validate_file_modify_shape(
            &format!("managed file '{}'", spec.target.posix()),
            spec.source.is_empty(),
            spec.strategy,
            spec.modify.as_ref(),
            spec.encryption.is_some(),
            spec.private,
        )?;
    }
    Ok(())
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

/// A declarative backup: snapshot `source` (a file or directory) into
/// `destination`, retaining the newest `retention` snapshots.
///
/// The shape is validated at parse time; the backup engine, CLI surface
/// (`cfgd backup ...`), and daemon scheduling that actually take snapshots
/// are not yet implemented.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupSpec {
    /// Unique identifier for this backup within `spec.backups`. Keys the
    /// `destination` default, run records, and CLI selection — uniqueness is
    /// enforced by [`validate_backup_specs`].
    pub name: String,
    /// File or directory to snapshot.
    pub source: PathBuf,
    /// Where snapshots are written. Defaults to `<state_dir>/backups/<name>/`
    /// when omitted — resolved by the backup engine, not at parse time, since
    /// the state dir depends on runtime scope/overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    /// Filename template for each snapshot. Supports `{name}`, `{filename}`,
    /// and `{timestamp}` (UTC, formatted per [`crate::BACKUP_TIMESTAMP_FORMAT`]).
    /// See [`render_backup_name_pattern`]. Unknown `{var}` tokens are rejected
    /// at parse time by [`validate_backup_specs`]. Defaults to
    /// `"{filename}.{timestamp}"`.
    #[serde(default = "default_backup_name_pattern")]
    pub name_pattern: String,
    /// When to run this backup: a `parse_duration_str` interval (e.g. `"6h"`)
    /// or a cron expression (e.g. `"0 3 * * *"`), validated by
    /// [`validate_backup_specs`]. Omitted means "run on every apply".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Number of newest snapshots to keep for this backup; older snapshots
    /// are pruned. Defaults to 10.
    #[serde(default = "default_backup_retention")]
    pub retention: u32,
    /// Scripts run before the snapshot is taken (e.g. stop a service that
    /// holds `source` open so the snapshot is consistent).
    #[serde(default)]
    pub pre_backup: Vec<ScriptEntry>,
    /// Scripts run after the snapshot completes (e.g. restart the service
    /// stopped by `preBackup`).
    #[serde(default)]
    pub post_backup: Vec<ScriptEntry>,
}

fn default_backup_name_pattern() -> String {
    "{filename}.{timestamp}".to_string()
}

fn default_backup_retention() -> u32 {
    10
}

/// The only `{var}` tokens a backup `namePattern` may reference.
const BACKUP_NAME_PATTERN_VARS: &[&str] = &["name", "filename", "timestamp"];

/// Extract every `{var}` placeholder token from a `namePattern` string, in
/// order of appearance (duplicates included, unclosed `{` ignored).
fn name_pattern_vars(pattern: &str) -> Vec<&str> {
    let mut vars = Vec::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                vars.push(&after[..close]);
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    vars
}

/// Render a backup `namePattern` by substituting `{name}`, `{filename}`, and
/// `{timestamp}` with the supplied values. `timestamp` should already be
/// formatted per [`crate::BACKUP_TIMESTAMP_FORMAT`] — this function does no
/// formatting of its own, only substitution.
///
/// Unknown `{var}` tokens are rejected at config-parse time by
/// [`validate_backup_specs`], so every token reaching this function is one of
/// the three known variables.
pub fn render_backup_name_pattern(
    pattern: &str,
    name: &str,
    filename: &str,
    timestamp: &str,
) -> String {
    pattern
        .replace("{name}", name)
        .replace("{filename}", filename)
        .replace("{timestamp}", timestamp)
}

/// Validate a backup `namePattern`: every `{var}` token must be one of
/// `name`/`filename`/`timestamp`.
fn validate_backup_name_pattern(subject: &str, pattern: &str) -> Result<()> {
    for var in name_pattern_vars(pattern) {
        if !BACKUP_NAME_PATTERN_VARS.contains(&var) {
            let valid = BACKUP_NAME_PATTERN_VARS
                .iter()
                .map(|v| format!("{{{v}}}"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ConfigError::Invalid {
                message: format!(
                    "{subject}: namePattern references unknown variable '{{{var}}}'; valid variables are {valid}"
                ),
            }
            .into());
        }
    }
    Ok(())
}

/// Validate a backup `schedule`: it must parse as either a
/// [`crate::parse_duration_str`] interval or a `croner` cron expression.
/// Naming both attempted interpretations' errors on failure so a typo in
/// either form is diagnosable from the message alone.
fn validate_backup_schedule(subject: &str, schedule: &str) -> Result<()> {
    let duration_err = match crate::parse_duration_str(schedule) {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    let cron_err = match schedule.parse::<croner::Cron>() {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    Err(ConfigError::Invalid {
        message: format!(
            "{subject}: schedule '{schedule}' is not a valid interval ({duration_err}) and not a valid cron expression ({cron_err})"
        ),
    }
    .into())
}

/// Validate `spec.backups[]`: `name` unique across the list, `namePattern`
/// references only known variables, and `schedule` (when set) parses as an
/// interval or a cron expression.
pub fn validate_backup_specs(specs: &[BackupSpec]) -> Result<()> {
    let mut seen_names = std::collections::HashSet::new();
    for spec in specs {
        let subject = format!("backup '{}'", spec.name);
        if !seen_names.insert(spec.name.as_str()) {
            return Err(ConfigError::Invalid {
                message: format!(
                    "duplicate backup name '{}': names must be unique across spec.backups",
                    spec.name
                ),
            }
            .into());
        }
        validate_backup_name_pattern(&subject, &spec.name_pattern)?;
        if let Some(schedule) = &spec.schedule {
            validate_backup_schedule(&subject, schedule)?;
        }
    }
    Ok(())
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

    #[test]
    fn env_scope_parses_case_insensitively() {
        for (token, expected) in [
            ("all", EnvScope::All),
            ("ALL", EnvScope::All),
            ("login", EnvScope::Login),
            ("Login", EnvScope::Login),
            ("interactive", EnvScope::Interactive),
            ("INTERACTIVE", EnvScope::Interactive),
            ("Interactive", EnvScope::Interactive),
        ] {
            let parsed: EnvScope = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(parsed, expected, "token {token}");
        }
    }

    #[test]
    fn file_strategy_parses_case_insensitively() {
        for (token, expected) in [
            ("symlink", FileStrategy::Symlink),
            ("SYMLINK", FileStrategy::Symlink),
            ("copy", FileStrategy::Copy),
            ("Copy", FileStrategy::Copy),
            ("template", FileStrategy::Template),
            ("hardlink", FileStrategy::Hardlink),
            ("HardLink", FileStrategy::Hardlink),
            ("modify", FileStrategy::Modify),
            ("Modify", FileStrategy::Modify),
            ("MODIFY", FileStrategy::Modify),
        ] {
            let parsed: FileStrategy = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(parsed, expected, "token {token}");
        }
    }

    #[test]
    fn file_strategy_rejects_garbage() {
        serde_yaml::from_str::<FileStrategy>("move").expect_err("unknown FileStrategy must error");
    }

    /// `FileStrategy::ALL` is hand-written next to the enum, and nothing about
    /// adding a variant forces it to be updated — a variant present in the enum
    /// and in the `case_insensitive_enum!` token list but absent from `ALL`
    /// would silently vanish from the published `spec.fileStrategy` schema.
    ///
    /// The macro embeds its own token list in serde's `unknown_variant` error,
    /// so parsing that message recovers the deserializer's real accepted set
    /// and pins `ALL` against it from the other direction.
    #[test]
    fn file_strategy_all_matches_the_deserializers_accepted_tokens() {
        let err = serde_yaml::from_str::<FileStrategy>("definitely-not-a-strategy")
            .expect_err("unknown FileStrategy must error");
        let message = err.to_string();
        let listed = message
            .split_once("expected one of ")
            .unwrap_or_else(|| panic!("error must list the accepted tokens, got: {message}"))
            .1;
        let accepted: std::collections::BTreeSet<&str> = listed
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::trim)
            .collect();
        let declared: std::collections::BTreeSet<&str> =
            FileStrategy::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            declared, accepted,
            "FileStrategy::ALL and the case_insensitive_enum! token list disagree"
        );
    }

    #[test]
    fn file_strategy_serializes_canonical_pascalcase() {
        let s = serde_yaml::to_string(&FileStrategy::Symlink).expect("serialize");
        assert_eq!(s.trim(), "Symlink");
        let s = serde_yaml::to_string(&FileStrategy::Modify).expect("serialize");
        assert_eq!(s.trim(), "Modify");
    }

    #[test]
    fn modify_format_parses_case_insensitively() {
        for (token, expected) in [
            ("ini", ModifyFormat::Ini),
            ("INI", ModifyFormat::Ini),
            ("json", ModifyFormat::Json),
            ("Json", ModifyFormat::Json),
            ("yaml", ModifyFormat::Yaml),
            ("YAML", ModifyFormat::Yaml),
            ("toml", ModifyFormat::Toml),
            ("Toml", ModifyFormat::Toml),
        ] {
            let parsed: ModifyFormat = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(parsed, expected, "token {token}");
        }
    }

    #[test]
    fn modify_format_rejects_garbage() {
        serde_yaml::from_str::<ModifyFormat>("xml").expect_err("unknown ModifyFormat must error");
    }

    #[test]
    fn modify_spec_rejects_unknown_field() {
        let yaml = "ensure:\n  a: b\nbogus: 1\n";
        let err = serde_yaml::from_str::<ModifySpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogus");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn encryption_mode_parses_case_insensitively() {
        for (token, expected) in [
            ("inrepo", EncryptionMode::InRepo),
            ("INREPO", EncryptionMode::InRepo),
            ("InRepo", EncryptionMode::InRepo),
            ("always", EncryptionMode::Always),
            ("Always", EncryptionMode::Always),
        ] {
            let parsed: EncryptionMode = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(parsed, expected, "token {token}");
        }
    }

    #[test]
    fn encryption_mode_rejects_garbage() {
        serde_yaml::from_str::<EncryptionMode>("never")
            .expect_err("unknown EncryptionMode must error");
    }

    // --- BackupSpec ---

    #[test]
    fn backup_spec_parses_pinned_design_shape() {
        let yaml = r#"
name: openlist-db
source: /var/lib/openlist/data.db
destination: ~/backups/openlist
namePattern: "{filename}.{timestamp}"
schedule: "0 3 * * *"
retention: 7
preBackup:
  - run: systemctl stop openlist
postBackup:
  - run: systemctl start openlist
"#;
        let spec: BackupSpec = serde_yaml::from_str(yaml).expect("pinned shape should parse");
        assert_eq!(spec.name, "openlist-db");
        assert_eq!(spec.source, PathBuf::from("/var/lib/openlist/data.db"));
        assert_eq!(spec.destination, Some(PathBuf::from("~/backups/openlist")));
        assert_eq!(spec.name_pattern, "{filename}.{timestamp}");
        assert_eq!(spec.schedule.as_deref(), Some("0 3 * * *"));
        assert_eq!(spec.retention, 7);
        assert_eq!(spec.pre_backup.len(), 1);
        assert_eq!(spec.post_backup.len(), 1);
    }

    #[test]
    fn backup_spec_only_name_and_source_required() {
        let yaml = "name: db\nsource: /var/lib/db\n";
        let spec: BackupSpec = serde_yaml::from_str(yaml).expect("minimal spec should parse");
        assert_eq!(spec.destination, None);
        assert_eq!(spec.name_pattern, "{filename}.{timestamp}");
        assert_eq!(spec.schedule, None);
        assert_eq!(spec.retention, 10);
        assert!(spec.pre_backup.is_empty());
        assert!(spec.post_backup.is_empty());
    }

    #[test]
    fn backup_spec_rejects_unknown_field() {
        let yaml = "name: db\nsource: /var/lib/db\nbogus: 1\n";
        let err = serde_yaml::from_str::<BackupSpec>(yaml)
            .expect_err("expected deny_unknown_fields to reject bogus");
        assert!(format!("{}", err).contains("unknown field"));
    }

    #[test]
    fn validate_backup_specs_rejects_duplicate_names() {
        let specs = vec![
            BackupSpec {
                name: "db".into(),
                source: PathBuf::from("/a"),
                destination: None,
                name_pattern: default_backup_name_pattern(),
                schedule: None,
                retention: default_backup_retention(),
                pre_backup: vec![],
                post_backup: vec![],
            },
            BackupSpec {
                name: "db".into(),
                source: PathBuf::from("/b"),
                destination: None,
                name_pattern: default_backup_name_pattern(),
                schedule: None,
                retention: default_backup_retention(),
                pre_backup: vec![],
                post_backup: vec![],
            },
        ];
        let err = validate_backup_specs(&specs).expect_err("duplicate names must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("duplicate backup name"), "got: {msg}");
        assert!(msg.contains("'db'"), "got: {msg}");
    }

    #[test]
    fn validate_backup_specs_accepts_unique_names() {
        let specs = vec![
            BackupSpec {
                name: "db".into(),
                source: PathBuf::from("/a"),
                destination: None,
                name_pattern: default_backup_name_pattern(),
                schedule: None,
                retention: default_backup_retention(),
                pre_backup: vec![],
                post_backup: vec![],
            },
            BackupSpec {
                name: "config".into(),
                source: PathBuf::from("/b"),
                destination: None,
                name_pattern: default_backup_name_pattern(),
                schedule: None,
                retention: default_backup_retention(),
                pre_backup: vec![],
                post_backup: vec![],
            },
        ];
        validate_backup_specs(&specs).expect("unique names should validate");
    }

    #[test]
    fn validate_backup_specs_accepts_good_interval_schedule() {
        for schedule in ["30s", "5m", "1h", "1d", "3600"] {
            let specs = vec![BackupSpec {
                name: "db".into(),
                source: PathBuf::from("/a"),
                destination: None,
                name_pattern: default_backup_name_pattern(),
                schedule: Some(schedule.to_string()),
                retention: default_backup_retention(),
                pre_backup: vec![],
                post_backup: vec![],
            }];
            validate_backup_specs(&specs)
                .unwrap_or_else(|e| panic!("interval '{schedule}' should validate: {e}"));
        }
    }

    #[test]
    fn validate_backup_specs_accepts_good_cron_schedule() {
        for schedule in ["0 3 * * *", "*/15 * * * *", "0 0 1 * *"] {
            let specs = vec![BackupSpec {
                name: "db".into(),
                source: PathBuf::from("/a"),
                destination: None,
                name_pattern: default_backup_name_pattern(),
                schedule: Some(schedule.to_string()),
                retention: default_backup_retention(),
                pre_backup: vec![],
                post_backup: vec![],
            }];
            validate_backup_specs(&specs)
                .unwrap_or_else(|e| panic!("cron '{schedule}' should validate: {e}"));
        }
    }

    #[test]
    fn validate_backup_specs_rejects_bad_schedule_naming_both_attempts() {
        let specs = vec![BackupSpec {
            name: "db".into(),
            source: PathBuf::from("/a"),
            destination: None,
            name_pattern: default_backup_name_pattern(),
            schedule: Some("not-a-schedule".into()),
            retention: default_backup_retention(),
            pre_backup: vec![],
            post_backup: vec![],
        }];
        let err = validate_backup_specs(&specs).expect_err("garbage schedule must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("not a valid interval") && msg.contains("not a valid cron expression"),
            "expected message naming both attempted interpretations, got: {msg}"
        );
    }

    #[test]
    fn validate_backup_specs_rejects_unknown_name_pattern_var() {
        let specs = vec![BackupSpec {
            name: "db".into(),
            source: PathBuf::from("/a"),
            destination: None,
            name_pattern: "{bogus}.bak".into(),
            schedule: None,
            retention: default_backup_retention(),
            pre_backup: vec![],
            post_backup: vec![],
        }];
        let err = validate_backup_specs(&specs).expect_err("unknown var must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("{bogus}"), "got: {msg}");
        assert!(
            msg.contains("{name}") && msg.contains("{filename}") && msg.contains("{timestamp}"),
            "expected message naming the valid var set, got: {msg}"
        );
    }

    #[test]
    fn validate_backup_specs_accepts_all_known_name_pattern_vars() {
        let specs = vec![BackupSpec {
            name: "db".into(),
            source: PathBuf::from("/a"),
            destination: None,
            name_pattern: "{name}-{filename}-{timestamp}".into(),
            schedule: None,
            retention: default_backup_retention(),
            pre_backup: vec![],
            post_backup: vec![],
        }];
        validate_backup_specs(&specs).expect("known vars should validate");
    }

    #[test]
    fn render_backup_name_pattern_substitutes_all_vars() {
        let rendered = render_backup_name_pattern(
            "{name}/{filename}.{timestamp}",
            "openlist-db",
            "data.db",
            "20260801T120000Z",
        );
        assert_eq!(rendered, "openlist-db/data.db.20260801T120000Z");
    }

    #[test]
    fn render_backup_name_pattern_default_shape() {
        let rendered =
            render_backup_name_pattern(&default_backup_name_pattern(), "db", "data.db", "TS");
        assert_eq!(rendered, "data.db.TS");
    }

    #[test]
    fn render_backup_name_pattern_repeated_var() {
        // Duplicate `{name}` tokens both substitute — `replace` is global, not
        // first-match-only.
        let rendered = render_backup_name_pattern("{name}-{name}", "db", "f", "ts");
        assert_eq!(rendered, "db-db");
    }
}
