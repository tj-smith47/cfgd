use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use super::module::ScriptEntry;
use super::source::{EnvVar, ShellAlias};
use crate::PathDisplayExt;
use crate::errors::{ConfigError, Result};

/// The `spec.system` map: configurator name → that configurator's settings block.
///
/// Ordered by key rather than hashed, because every surface that walks it is
/// user-facing or persisted — the plan tree's System phase, the checkin payload's
/// serialized config, `profile show`, the composition constraint report — and a
/// hash map hands each of them a different order per process. Two runs on an
/// unchanged machine then disagree about what they did.
pub type SystemSettings = BTreeMap<String, serde_yaml::Value>;

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

/// A `profile.yaml` document: a named, inheritable bundle of everything cfgd
/// reconciles for a machine — packages, files, env, aliases, system settings,
/// scripts, and backups.
///
/// ```yaml
/// apiVersion: cfgd.io/v1alpha1
/// kind: Profile
/// metadata:
///   name: work
/// spec:
///   modules: [nvim, zsh]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileDocument {
    /// API group/version, e.g. `cfgd.io/v1alpha1`.
    pub api_version: String,
    /// Document kind. Always `Profile` for this file.
    pub kind: String,
    /// Identifying metadata for this profile.
    pub metadata: ProfileMetadata,
    /// The profile's declared surface.
    pub spec: ProfileSpec,
}

/// `metadata`: identifying information for a profile.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileMetadata {
    /// The profile's name, referenced by `spec.profile` in `cfgd.yaml` and by
    /// `inherits:` in another profile.
    pub name: String,
}

/// `spec`: the declared surface of a profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSpec {
    /// Names of base profiles to merge under this one. Later fields in this
    /// profile override an inherited base's; lists are unioned.
    #[serde(default)]
    pub inherits: Vec<String>,

    /// Names of modules this profile includes.
    #[serde(default)]
    pub modules: Vec<String>,

    /// Environment variables this profile sets.
    #[serde(default)]
    pub env: Vec<EnvVar>,

    /// How far `spec.env` exports reach across the current user's environment.
    /// Omitted means "inherit" (a parent layer's value survives); the resolved
    /// default when no layer sets it is `All` — every standard user entry point
    /// cfgd can safely touch. Narrow it to `Login` or `Interactive` to opt out
    /// of the broader session surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_scope: Option<EnvScope>,

    /// Shell aliases this profile sets.
    #[serde(default)]
    pub aliases: Vec<ShellAlias>,

    /// Packages this profile installs, grouped by manager.
    #[serde(default)]
    pub packages: Option<PackagesSpec>,

    /// Files this profile deploys.
    #[serde(default)]
    pub files: Option<FilesSpec>,

    /// System configurator settings (`macosDefaults`, `systemd`, `sysctl`, …),
    /// keyed by configurator name.
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    pub system: SystemSettings,

    /// Secrets this profile resolves into files or environment variables.
    #[serde(default)]
    pub secrets: Vec<SecretSpec>,

    /// Lifecycle scripts (`preApply`, `postApply`, …) this profile runs.
    #[serde(default)]
    pub scripts: Option<ScriptSpec>,

    /// Declarative backup jobs this profile schedules.
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

/// `spec.packages`: packages to install, grouped by package manager.
///
/// Every manager field accepts either a bare list of names or (for the
/// managers with options of their own) a mapping:
///
/// ```yaml
/// packages:
///   brew:
///     formulae: [ripgrep, fzf]
///     casks: [alacritty]
///   apt: [curl, git]
///   cargo: [ripgrep]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackagesSpec {
    /// Homebrew packages (macOS/Linux). Accepts a bare list of formulae or a
    /// `BrewSpec` mapping.
    #[serde(default, deserialize_with = "list_or_struct")]
    pub brew: Option<BrewSpec>,
    /// APT packages (Debian/Ubuntu). Accepts a bare list or an `AptSpec` mapping.
    #[serde(default, deserialize_with = "list_or_struct")]
    pub apt: Option<AptSpec>,
    /// Cargo packages (`cargo install`). Accepts a bare list or a `CargoSpec` mapping.
    #[serde(default, deserialize_with = "list_or_struct")]
    pub cargo: Option<CargoSpec>,
    /// npm global packages. Accepts a bare list or an `NpmSpec` mapping.
    #[serde(default, deserialize_with = "list_or_struct")]
    pub npm: Option<NpmSpec>,
    /// pipx-installed Python applications.
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub pipx: Vec<String>,
    /// DNF packages (Fedora/RHEL).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub dnf: Vec<String>,
    /// APK packages (Alpine).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub apk: Vec<String>,
    /// Pacman packages (Arch).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub pacman: Vec<String>,
    /// Zypper packages (openSUSE).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub zypper: Vec<String>,
    /// Yum packages (legacy RHEL/CentOS).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub yum: Vec<String>,
    /// pkg packages (FreeBSD).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub pkg: Vec<String>,
    /// Snap packages (Linux). Accepts a bare list or a `SnapSpec` mapping.
    #[serde(default, deserialize_with = "list_or_struct")]
    pub snap: Option<SnapSpec>,
    /// Flatpak packages (Linux). Accepts a bare list or a `FlatpakSpec` mapping.
    #[serde(default, deserialize_with = "list_or_struct")]
    pub flatpak: Option<FlatpakSpec>,
    /// Nix packages (`nix-env` / `nix profile`).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub nix: Vec<String>,
    /// Go packages (`go install`).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub go: Vec<String>,
    /// Winget packages (Windows).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub winget: Vec<String>,
    /// Chocolatey packages (Windows).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub chocolatey: Vec<String>,
    /// Scoop packages (Windows).
    #[serde(default, deserialize_with = "list_or_packages_vec")]
    pub scoop: Vec<String>,
    /// User-defined package managers not built into cfgd, each with its own
    /// check/install/uninstall commands.
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

/// Homebrew package spec. Supports both list form (`brew: [ripgrep, fzf]`,
/// mapped to `formulae`) and object form for taps/casks:
///
/// ```yaml
/// brew:
///   taps: [homebrew/cask-fonts]
///   formulae: [ripgrep, fzf]
///   casks: [alacritty]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrewSpec {
    /// Path to a Brewfile to apply instead of (or alongside) the lists below.
    #[serde(default)]
    pub file: Option<String>,
    /// Third-party taps to add before installing formulae/casks.
    #[serde(default)]
    pub taps: Vec<String>,
    /// Homebrew formulae (CLI packages) to install.
    #[serde(default)]
    pub formulae: Vec<String>,
    /// Homebrew casks (GUI applications) to install.
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

/// APT package spec. Supports both list form (`apt: [curl, git]`) and object
/// form (`apt: { file: packages.txt, packages: [...] }`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AptSpec {
    /// Path to a package-list file to install from, one name per line.
    #[serde(default)]
    pub file: Option<String>,
    /// APT package names to install.
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

/// npm package spec. Supports both list form (`npm: [pnpm]`, mapped to
/// `global`) and object form.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NpmSpec {
    /// Path to a `package.json` to install dependencies from.
    #[serde(default)]
    pub file: Option<String>,
    /// Package names to install globally (`npm install -g`).
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

/// Cargo package spec. The `cargo` field accepts both a list form
/// (`cargo: [bat, ripgrep]`) and an object form
/// (`cargo: { file: Cargo.toml, packages: [...] }`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoSpec {
    /// Path to a `Cargo.toml` whose binaries to install instead of the list below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Crate names to install (`cargo install`).
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

/// Snap package spec. Supports both list form (`snap: [spotify]`, mapped to
/// `packages`) and object form for classic-confinement snaps.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapSpec {
    /// Snap names installed with default (strict) confinement.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Snap names installed with `--classic` confinement.
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

/// Flatpak package spec. Supports both list form (`flatpak: [org.gimp.GIMP]`,
/// mapped to `packages`) and object form for a non-default remote.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlatpakSpec {
    /// Flatpak application ids to install.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Remote to install from (e.g. `flathub`). Falls back to Flatpak's
    /// configured default remote when omitted.
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

/// A user-defined package manager under `spec.packages.custom[]`, driven
/// entirely by shell commands.
///
/// ```yaml
/// custom:
///   - name: asdf
///     check: "command -v asdf"
///     listInstalled: "asdf list"
///     install: "asdf install {package}"
///     uninstall: "asdf uninstall {package}"
///     packages: [nodejs]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomManagerSpec {
    /// Manager name, used in `prefer:`/`deny:` lists and status output.
    pub name: String,
    /// Command that exits zero when this manager is available on the machine.
    pub check: String,
    /// Command whose stdout lists installed package names, one per line.
    pub list_installed: String,
    /// Command template to install a package; `{package}` is substituted.
    pub install: String,
    /// Command template to uninstall a package; `{package}` is substituted.
    pub uninstall: String,
    /// Command to refresh the manager's own package index/cache before installs.
    #[serde(default)]
    pub update: Option<String>,
    /// Package names to install with this manager.
    #[serde(default)]
    pub packages: Vec<String>,
}

/// `spec.files`: files this profile deploys and their permission overrides.
///
/// ```yaml
/// files:
///   managed:
///     - source: files/gitconfig
///       target: ~/.gitconfig
///   permissions:
///     ~/.ssh/id_ed25519: "0600"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesSpec {
    /// Files this profile deploys, each pairing a source in the config
    /// directory with a target on the machine. Empty, no files are managed.
    #[serde(default)]
    pub managed: Vec<ManagedFileSpec>,
    /// Octal permission strings (`"0600"`) keyed by target path, applied after
    /// deployment.
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
    /// script, leaving everything else untouched. Requires a `patch:` block.
    Patch,
}

case_insensitive_enum!(FileStrategy {
    "Symlink" => FileStrategy::Symlink,
    "Copy" => FileStrategy::Copy,
    "Template" => FileStrategy::Template,
    "Hardlink" => FileStrategy::Hardlink,
    "Patch" => FileStrategy::Patch,
});

impl FileStrategy {
    /// Whether the strategy is meaningful as the global `spec.fileStrategy`
    /// default.
    ///
    /// `Patch` is not: it is defined by a per-file `patch:` block, which a
    /// file inheriting the global default cannot have. The config parser and
    /// the published schema both derive their accepted value set from this, so
    /// an editor and `cfgd` can never disagree about it.
    pub fn valid_as_global_default(self) -> bool {
        !matches!(self, FileStrategy::Patch)
    }
}

/// File format used to interpret and re-serialize a `Patch`-strategy target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum PatchFormat {
    /// INI sections/keys, edited line-by-line to preserve comments and layout.
    Ini,
    /// JSON, re-serialized on write (no comments to preserve).
    Json,
    /// YAML; comments are NOT preserved across a merge (see docs for the caveat).
    Yaml,
    /// TOML, edited in place to preserve comments and layout.
    Toml,
}

case_insensitive_enum!(PatchFormat {
    "Ini" => PatchFormat::Ini,
    "Json" => PatchFormat::Json,
    "Yaml" => PatchFormat::Yaml,
    "Toml" => PatchFormat::Toml,
});

/// Configuration for the `Patch` file strategy: a structured merge (`ensure`)
/// or a content-rewriting script, applied on top of the target's current
/// content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchSpec {
    /// File format to parse the target as. Inferred from the target's
    /// extension when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<PatchFormat>,
    /// Keys/values to deep-merge into the target, leaving unmentioned keys
    /// untouched. Values are literal (no template rendering). Mutually
    /// exclusive with `script`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub ensure: Option<serde_yaml::Value>,
    /// A script path or an inline command that receives the target's current
    /// content on stdin and writes the new content to stdout. A relative path
    /// resolves against the module directory for a module file
    /// (`spec.files[]`) and against the config directory for a profile file
    /// (`spec.files.managed[]`); a value that resolves to no file is run as an
    /// inline command. Mutually exclusive with `ensure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Name of the source whose `constraints.noScripts` bars this filter, set
    /// by composition when the subscriber did not opt in.
    ///
    /// Not part of the config surface (`#[serde(skip)]`, so `deny_unknown_fields`
    /// rejects it in YAML and it never reaches the published schema): composition
    /// is the only writer. Poisoning the spec rather than dropping it keeps the
    /// file visible on read-only surfaces while making the filter unrunnable by
    /// construction — every evaluation path funnels through `compute_patched`,
    /// which refuses a marked spec.
    #[serde(skip)]
    pub blocked_by: Option<String>,
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

/// One entry of `spec.files.managed[]`: a file this profile deploys.
///
/// ```yaml
/// files:
///   managed:
///     - source: files/gitconfig
///       target: ~/.gitconfig
///       permissions: "644"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedFileSpec {
    /// Path to the source file. Not required when `strategy` is `Patch`;
    /// required otherwise.
    #[serde(default)]
    pub source: String,
    /// Destination path on the machine. A leading `~` expands to the home
    /// directory.
    pub target: PathBuf,
    /// Per-file deployment strategy override. Omitted, the profile-wide
    /// default applies.
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
    /// Structured merge or script configuration for `strategy: Patch`.
    /// Required when `strategy` is `Patch`, rejected otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSpec>,
}

// "At least one of `target` / `envs`" is enforced at runtime by
// `validate_secret_specs`, not in the JSON schema: both are plain `Option`
// fields, so the generated schema marks them optional. Expressing the
// constraint would require a hand-written `anyOf`, which would drift from
// this struct — the by-construction generation is the priority, runtime
// validation is the backstop.
/// One entry of `spec.secrets[]`: a secret resolved into a file, into
/// environment variables, or both. At least one of `target` / `envs` must be
/// set; an entry carrying both writes the file AND exports the variables from
/// one resolution.
///
/// ```yaml
/// secrets:
///   - source: op://Personal/GitHub/token
///     envs: [GITHUB_TOKEN]
///   - source: ssh_key
///     target: ~/.ssh/id_ed25519
///   - source: vault://secret/data/api#key
///     target: ~/.config/api-key
///     envs: [API_KEY]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretSpec {
    /// Backend-specific reference to the secret (a 1Password `op://` URI, a
    /// Vault path, a sops-encrypted file key, …).
    pub source: String,
    /// File path to write the decrypted secret to. May be combined with `envs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    /// Template rendered around the resolved value before it is written to
    /// `target` or exported under `envs`: every `${secret:value}` in it is
    /// replaced by the value (`template: "token: ${secret:value}"`). Only a
    /// provider reference (`op://`, `vault://`, …) resolves to a single value,
    /// so `template` is rejected on a sops-encrypted file source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Secret backend name to resolve `source` with. Falls back to
    /// `spec.secrets.backend` from `cfgd.yaml` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Environment variable names to export the decrypted value under. May be
    /// combined with `target`.
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

/// Validate the `source` / `strategy` / `patch` / `encryption` shape shared by
/// `ManagedFileSpec` and `ModuleFileEntry`: `source` is required unless
/// `strategy` is `Patch`; a `patch` block is required when `strategy` is
/// `Patch` and rejected otherwise; within a `patch` block exactly one of
/// `ensure`/`script` must be set; `encryption` is rejected on a `Patch` entry.
pub(crate) fn validate_file_patch_shape(
    subject: &str,
    source_is_empty: bool,
    strategy: Option<FileStrategy>,
    patch: Option<&PatchSpec>,
    encryption_declared: bool,
    private: bool,
) -> Result<()> {
    let is_patch = matches!(strategy, Some(FileStrategy::Patch));
    // `private` marks the SOURCE file local-only (gitignored, skipped where it
    // is absent). `Patch` has no source, so the flag can only ever be a no-op
    // that reads as a promise the strategy never keeps.
    if is_patch && private {
        return Err(ConfigError::Invalid {
            message: format!("{subject}: 'private' is not supported with strategy 'patch'"),
        }
        .into());
    }
    // Every `encryption` mode constrains the SOURCE file a strategy deploys
    // ("must be encrypted in the repo"). `Patch` has no source — it rewrites
    // the target's own plaintext structure — so the constraint could only be
    // silently ignored. Reject it instead of pretending it was honoured.
    if is_patch && encryption_declared {
        return Err(ConfigError::Invalid {
            message: format!("{subject}: 'encryption' is not supported with strategy 'patch'"),
        }
        .into());
    }
    match (is_patch, patch) {
        (true, None) => Err(ConfigError::Invalid {
            message: format!("{subject}: strategy 'patch' requires a 'patch' block"),
        }
        .into()),
        (false, Some(_)) => Err(ConfigError::Invalid {
            message: format!("{subject}: 'patch' is only valid when strategy is 'patch'"),
        }
        .into()),
        (true, Some(m)) => match (m.ensure.is_some(), m.script.is_some()) {
            (true, true) => Err(ConfigError::Invalid {
                message: format!(
                    "{subject}: 'patch' must set exactly one of 'ensure' or 'script', not both"
                ),
            }
            .into()),
            (false, false) => Err(ConfigError::Invalid {
                message: format!("{subject}: 'patch' must set exactly one of 'ensure' or 'script'"),
            }
            .into()),
            _ => Ok(()),
        },
        (false, None) => {
            if source_is_empty {
                Err(ConfigError::Invalid {
                    message: format!("{subject}: 'source' is required unless strategy is 'patch'"),
                }
                .into())
            } else {
                Ok(())
            }
        }
    }
}

/// Validate the `patch` strategy shape of every managed file
/// (`spec.files.managed`). See [`validate_file_patch_shape`].
pub fn validate_managed_file_specs(specs: &[ManagedFileSpec]) -> Result<()> {
    for spec in specs {
        validate_file_patch_shape(
            &format!("managed file '{}'", spec.target.posix()),
            spec.source.is_empty(),
            spec.strategy,
            spec.patch.as_ref(),
            spec.encryption.is_some(),
            spec.private,
        )?;
    }
    Ok(())
}

/// Validate that each secret has at least one delivery target (`target` or
/// `envs`), and that a `template` names a value it can wrap: it must sit on a
/// provider reference (a sops file decrypts to content, not a value) and must
/// contain the `${secret:value}` placeholder, or the resolved secret would be
/// silently dropped on the floor.
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
        if let Some(template) = &spec.template {
            if crate::providers::parse_secret_reference(&spec.source).is_none() {
                return Err(ConfigError::Invalid {
                    message: format!(
                        "secret '{}': 'template' applies only to a provider reference ({}), not to an encrypted file",
                        spec.source,
                        crate::providers::SECRET_REFERENCE_SCHEMES
                            .iter()
                            .map(|(scheme, _)| *scheme)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
                .into());
            }
            if !template.contains(crate::providers::SECRET_TEMPLATE_PLACEHOLDER) {
                return Err(ConfigError::Invalid {
                    message: format!(
                        "secret '{}': 'template' must contain {} where the value goes",
                        spec.source,
                        crate::providers::SECRET_TEMPLATE_PLACEHOLDER
                    ),
                }
                .into());
            }
        }
    }
    Ok(())
}

/// `spec.scripts`: lifecycle hooks run at specific points in the reconcile cycle.
///
/// ```yaml
/// scripts:
///   preApply: "echo starting apply"
///   postApply:
///     - run: brew cleanup
///       continueOnError: true
///   onDrift: "notify-send 'cfgd: drift detected'"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptSpec {
    /// Run once before any action in an apply.
    #[serde(default)]
    pub pre_apply: Vec<ScriptEntry>,
    /// Run once after every action in an apply completes.
    #[serde(default)]
    pub post_apply: Vec<ScriptEntry>,
    /// Run once before a daemon reconcile tick begins.
    #[serde(default)]
    pub pre_reconcile: Vec<ScriptEntry>,
    /// Run once after a daemon reconcile tick completes.
    #[serde(default)]
    pub post_reconcile: Vec<ScriptEntry>,
    /// Run when the daemon detects drift, before any auto-apply decision.
    #[serde(default)]
    pub on_drift: Vec<ScriptEntry>,
    /// Run when a watched file changes on disk (requires `daemon.reconcile.onChange`).
    #[serde(default)]
    pub on_change: Vec<ScriptEntry>,
}

impl ScriptSpec {
    /// Every lifecycle hook paired with the entries declared for it, in the
    /// canonical hook order: each context's `pre` before its `post` (apply,
    /// then reconcile), then the event hooks. An apply and a reconcile are
    /// separate runs, so no single run reaches all six.
    ///
    /// The ONE enumeration of the hook set: a surface that lists, counts or
    /// names hooks reads from here, so none of them can miss a hook the YAML
    /// accepts or disagree about the order they are reported in.
    pub fn hooks(&self) -> [(&'static str, &[ScriptEntry]); 6] {
        // Destructured, so a seventh hook field does not compile until it is
        // listed here — the mechanism behind "no surface can miss a hook".
        let Self {
            pre_apply,
            post_apply,
            pre_reconcile,
            post_reconcile,
            on_drift,
            on_change,
        } = self;
        [
            ("preApply", pre_apply),
            ("postApply", post_apply),
            ("preReconcile", pre_reconcile),
            ("postReconcile", post_reconcile),
            ("onDrift", on_drift),
            ("onChange", on_change),
        ]
    }
}

/// A declarative backup: snapshot `source` (a file or directory) into
/// `destination`, retaining the newest `retention` snapshots.
///
/// The shape is validated at parse time and run by the backup engine.
/// Schedule-less backups (no `schedule`) run automatically on every
/// `cfgd apply`; every backup — scheduled or not — can also be run directly
/// with `cfgd backup run [name]`.
//
// Every `///` line on this struct and its fields is copied verbatim into
// schemas/cfgd-profile.schema.json, which editors render as YAML completion
// help. Keep them plain prose: a rustdoc intra-doc link renders as literal
// `[`name`]` noise to a user who has no rustdoc to follow it to.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupSpec {
    /// Unique identifier for this backup within `spec.backups`, unique across
    /// the list. Keys the `destination` default, run records, and CLI
    /// selection. Becomes a directory component (`<state_dir>/backups/<name>/`)
    /// and a lock filename (`<state_dir>/locks/backup-<name>.lock`), so it must
    /// be non-empty, non-blank, a single segment (no `/` or `\`), not a
    /// directory reference (`.`, `..`), not rooted (`/daily`, `C:/daily`), and
    /// free of `:` anywhere — a drive and NTFS data-stream separator on Windows.
    /// Windows shapes are rejected on every platform so a name written on one
    /// OS stays valid on the others.
    pub name: String,
    /// File or directory to snapshot. A leading `~` expands to the home
    /// directory. Must not contain, or sit inside, the resolved `destination` —
    /// a nested pair is rejected before any copy, with symlinks resolved on both
    /// sides. Its filename is what `{filename}` interpolates, so a source whose
    /// filename contains `:` (legal on Unix, a drive and data-stream separator
    /// on Windows) needs an explicit `namePattern` that leaves `{filename}` out.
    pub source: PathBuf,
    /// Where snapshots are written. Defaults to `<state_dir>/backups/<name>/`
    /// when omitted — resolved by the backup engine, not at parse time, since
    /// the state dir depends on runtime scope/overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    /// Filename template for each snapshot. Supports `{name}`, `{filename}`,
    /// and `{timestamp}` (UTC, `%Y%m%dT%H%M%SZ`). Unknown `{var}` tokens are
    /// rejected at parse time. A literal `/` nests the snapshot in a
    /// subdirectory of the destination. At run time the rendered value must be
    /// relative and every segment must name something: `.` and `..` segments,
    /// empty segments (`a//b`, `daily/`), rooted values (`/daily`, `C:/daily`,
    /// `C:daily`, `\\server\share`), and `:` anywhere are all rejected. Windows
    /// shapes are rejected on every platform, so a pattern is valid everywhere
    /// or nowhere. A rejection names the `{filename}` it interpolated, so a
    /// colon in the source filename points at itself. Defaults to
    /// `"{filename}.{timestamp}"`.
    #[serde(default = "default_backup_name_pattern")]
    pub name_pattern: String,
    /// When to run this backup: a duration interval (e.g. `"6h"`) or a cron
    /// expression, validated at parse time. Cron expressions may be 5-field
    /// (`minute hour day month weekday`, e.g. `"0 3 * * *"`) or 6-field with a
    /// leading seconds field (`second minute hour day month weekday`, e.g.
    /// `"30 0 3 * * *"`), and are evaluated in the machine's LOCAL timezone,
    /// like a crontab entry. An interval is measured from the unit's last
    /// recorded run, so a `"1d"` backup on a machine rebooted daily still fires
    /// daily. Setting this hands the backup to the daemon's timers and takes it
    /// out of apply; omitted means "run on every apply".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Number of newest snapshots to keep for this backup; older snapshots are
    /// pruned from disk and from the run history. Must be at least 1 (`0` would
    /// keep no backups, which is a misconfiguration rather than a supported
    /// "unlimited" mode). Defaults to 10.
    #[serde(default = "default_backup_retention")]
    #[schemars(range(min = 1))]
    pub retention: u32,
    /// Scripts run before the snapshot is taken (e.g. stop a service that
    /// holds `source` open so the snapshot is consistent). A failure skips the
    /// snapshot and records a failed run; `postBackup` still runs.
    #[serde(default)]
    pub pre_backup: Vec<ScriptEntry>,
    /// Scripts run after the copy step (e.g. restart the service stopped by
    /// `preBackup`). Always attempted, including after a failed `preBackup` or
    /// a failed copy.
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

/// Validate a backup `name`.
///
/// The name is a directory component (`<state_dir>/backups/<name>/`), a lock
/// filename (`<state_dir>/locks/backup-<name>.lock`), and the key the retention
/// pass prunes by — three roots cfgd creates and later deletes wholesale — so it
/// goes through [`crate::validate_plain_name`], the shared gate for exactly that
/// class. Only the single-component rule is checked here on top: `validate_plain_name`
/// accepts a nested `daily/2026`, which a backup name must not be.
pub(crate) fn validate_backup_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(ConfigError::Invalid {
            message: "backup name must not be empty or whitespace-only".to_string(),
        }
        .into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err(ConfigError::Invalid {
            message: format!(
                "backup name '{name}' must not contain path separators ('/' or '\\'); it is used as a directory component (<state_dir>/backups/<name>/)"
            ),
        }
        .into());
    }
    if let Err(why) = crate::validate_plain_name(name) {
        return Err(ConfigError::Invalid {
            message: format!(
                "backup name '{name}' is not usable as a name: {why}; it becomes a directory component (<state_dir>/backups/<name>/) and a lock file (<state_dir>/locks/backup-<name>.lock)"
            ),
        }
        .into());
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

/// Validate `spec.backups[]`: `name` is non-empty, path-safe, and unique
/// across the list; `namePattern` references only known variables;
/// `schedule` (when set) parses as an interval or a cron expression; and
/// `retention` is at least 1.
pub fn validate_backup_specs(specs: &[BackupSpec]) -> Result<()> {
    let mut seen_names = std::collections::HashSet::new();
    for spec in specs {
        validate_backup_name(&spec.name)?;
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
        if spec.retention == 0 {
            return Err(ConfigError::Invalid {
                message: format!(
                    "{subject}: retention must be at least 1 (0 would keep no backups); omit the field to use the default of 10"
                ),
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid backup unit; tests override only the field under test.
    fn backup(name: &str) -> BackupSpec {
        BackupSpec {
            name: name.into(),
            source: PathBuf::from("/a"),
            destination: None,
            name_pattern: default_backup_name_pattern(),
            schedule: None,
            retention: default_backup_retention(),
            pre_backup: vec![],
            post_backup: vec![],
        }
    }

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
            ("patch", FileStrategy::Patch),
            ("Patch", FileStrategy::Patch),
            ("PATCH", FileStrategy::Patch),
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

    #[test]
    fn file_strategy_serializes_canonical_pascalcase() {
        let s = serde_yaml::to_string(&FileStrategy::Symlink).expect("serialize");
        assert_eq!(s.trim(), "Symlink");
        let s = serde_yaml::to_string(&FileStrategy::Patch).expect("serialize");
        assert_eq!(s.trim(), "Patch");
    }

    #[test]
    fn patch_format_parses_case_insensitively() {
        for (token, expected) in [
            ("ini", PatchFormat::Ini),
            ("INI", PatchFormat::Ini),
            ("json", PatchFormat::Json),
            ("Json", PatchFormat::Json),
            ("yaml", PatchFormat::Yaml),
            ("YAML", PatchFormat::Yaml),
            ("toml", PatchFormat::Toml),
            ("Toml", PatchFormat::Toml),
        ] {
            let parsed: PatchFormat = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse: {e}"));
            assert_eq!(parsed, expected, "token {token}");
        }
    }

    #[test]
    fn patch_format_rejects_garbage() {
        serde_yaml::from_str::<PatchFormat>("xml").expect_err("unknown PatchFormat must error");
    }

    #[test]
    fn patch_spec_rejects_unknown_field() {
        let yaml = "ensure:\n  a: b\nbogus: 1\n";
        let err = serde_yaml::from_str::<PatchSpec>(yaml)
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
name: notes-db
source: ~/.local/share/notes/notes.db
destination: ~/backups/notes
namePattern: "{filename}.{timestamp}"
schedule: "0 3 * * *"
retention: 7
preBackup:
  - run: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
postBackup:
  - run: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"
"#;
        let spec: BackupSpec = serde_yaml::from_str(yaml).expect("pinned shape should parse");
        assert_eq!(spec.name, "notes-db");
        assert_eq!(spec.source, PathBuf::from("~/.local/share/notes/notes.db"));
        assert_eq!(spec.destination, Some(PathBuf::from("~/backups/notes")));
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
    fn validate_backup_specs_rejects_empty_name() {
        let specs = vec![backup("")];
        let err = validate_backup_specs(&specs).expect_err("empty name must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("must not be empty"), "got: {msg}");
    }

    #[test]
    fn validate_backup_specs_rejects_whitespace_only_name() {
        let specs = vec![backup("   ")];
        let err = validate_backup_specs(&specs).expect_err("whitespace-only name must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("must not be empty"), "got: {msg}");
    }

    #[test]
    fn validate_backup_specs_rejects_name_with_separator() {
        for bad in ["a/b", "a\\b"] {
            let specs = vec![backup(bad)];
            let err = validate_backup_specs(&specs)
                .expect_err("name with a path separator must be rejected");
            let msg = format!("{err}");
            assert!(
                msg.contains(bad) && msg.contains("path separators"),
                "got: {msg}"
            );
        }
    }

    #[test]
    fn validate_backup_specs_rejects_traversal_name() {
        let specs = vec![backup("..")];
        let err = validate_backup_specs(&specs).expect_err("'..' name must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("directory reference"), "got: {msg}");
    }

    #[test]
    fn validate_backup_specs_rejects_colon_in_name() {
        // `:` is a drive separator and an NTFS alternate-data-stream separator
        // on Windows, so `<state_dir>/backups/db:1/` and
        // `<state_dir>/locks/backup-db:1.lock` are not the paths they read as
        // there. `validate_plain_name` refuses the shape on every host so a
        // name authored on Linux does not detonate on a Windows machine that
        // syncs the same profile.
        for bad in [
            "db:1",
            "C:daily",
            ":leading",
            "trailing:",
            "20260801T120000Z:snap",
        ] {
            let specs = vec![backup(bad)];
            let err = validate_backup_specs(&specs)
                .expect_err(&format!("a ':' in '{bad}' must be rejected"));
            let msg = format!("{err}");
            assert!(
                msg.contains(bad) && msg.contains(':'),
                "the message must name the value and the offending character, got: {msg}"
            );
        }
    }

    #[test]
    fn validate_backup_specs_keeps_ordinary_names_legal() {
        // The `validate_plain_name` convergence must reject ONLY the newly
        // unsafe shapes: every name shape that was legal before stays legal.
        for good in [
            "docs",
            "notes-db.v2",
            "a..b",
            "weekly_2026",
            "Backup 1",
            "état",
            "-leading-dash",
            "..leading-dots",
        ] {
            let specs = vec![backup(good)];
            validate_backup_specs(&specs)
                .unwrap_or_else(|e| panic!("'{good}' must stay legal, got: {e}"));
        }
    }

    #[test]
    fn validate_backup_specs_accepts_dashes_and_dots_in_name() {
        let specs = vec![backup("notes-db.v2")];
        validate_backup_specs(&specs)
            .expect("a name with internal dashes and dots should validate");
    }

    #[test]
    fn validate_backup_specs_accepts_dotdot_as_substring_in_name() {
        let specs = vec![backup("a..b")];
        validate_backup_specs(&specs).expect(
            "a name containing '..' as a substring (not the exact traversal segment) should validate",
        );
    }

    #[test]
    fn validate_backup_specs_rejects_duplicate_names() {
        let specs = vec![
            backup("db"),
            BackupSpec {
                source: PathBuf::from("/b"),
                ..backup("db")
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
            backup("db"),
            BackupSpec {
                source: PathBuf::from("/b"),
                ..backup("config")
            },
        ];
        validate_backup_specs(&specs).expect("unique names should validate");
    }

    #[test]
    fn validate_backup_specs_accepts_good_interval_schedule() {
        for schedule in ["30s", "5m", "1h", "1d", "3600"] {
            let specs = vec![BackupSpec {
                schedule: Some(schedule.to_string()),
                ..backup("db")
            }];
            validate_backup_specs(&specs)
                .unwrap_or_else(|e| panic!("interval '{schedule}' should validate: {e}"));
        }
    }

    #[test]
    fn validate_backup_specs_accepts_good_cron_schedule() {
        for schedule in [
            "0 3 * * *",
            "*/15 * * * *",
            "0 0 1 * *",
            "30 0 3 * * *", // 6-field with a leading seconds field
        ] {
            let specs = vec![BackupSpec {
                schedule: Some(schedule.to_string()),
                ..backup("db")
            }];
            validate_backup_specs(&specs)
                .unwrap_or_else(|e| panic!("cron '{schedule}' should validate: {e}"));
        }
    }

    #[test]
    fn validate_backup_specs_rejects_bad_schedule_naming_both_attempts() {
        let specs = vec![BackupSpec {
            schedule: Some("not-a-schedule".into()),
            ..backup("db")
        }];
        let err = validate_backup_specs(&specs).expect_err("garbage schedule must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("not a valid interval") && msg.contains("not a valid cron expression"),
            "expected message naming both attempted interpretations, got: {msg}"
        );
    }

    #[test]
    fn validate_backup_specs_rejects_zero_retention() {
        let specs = vec![BackupSpec {
            retention: 0,
            ..backup("db")
        }];
        let err = validate_backup_specs(&specs).expect_err("retention 0 must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("retention must be at least 1"), "got: {msg}");
    }

    #[test]
    fn validate_backup_specs_accepts_retention_of_one() {
        let specs = vec![BackupSpec {
            retention: 1,
            ..backup("db")
        }];
        validate_backup_specs(&specs).expect("retention of 1 should validate");
    }

    #[test]
    fn validate_backup_specs_rejects_unknown_name_pattern_var() {
        let specs = vec![BackupSpec {
            name_pattern: "{bogus}.bak".into(),
            ..backup("db")
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
            name_pattern: "{name}-{filename}-{timestamp}".into(),
            ..backup("db")
        }];
        validate_backup_specs(&specs).expect("known vars should validate");
    }

    #[test]
    fn render_backup_name_pattern_substitutes_all_vars() {
        let rendered = render_backup_name_pattern(
            "{name}/{filename}.{timestamp}",
            "notes-db",
            "notes.db",
            "20260801T120000Z",
        );
        assert_eq!(rendered, "notes-db/notes.db.20260801T120000Z");
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
