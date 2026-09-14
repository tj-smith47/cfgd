use std::path::Path;

use serde::Serialize;

use cfgd_schema::{BackupSpec, ScriptSpec};

use super::parse::{find_profile_path, load_profile};
use super::profile_spec::{
    EnvScope, FilesSpec, PackagesSpec, ProfileDocument, ProfileSpec, SecretSpec, SystemSettings,
    validate_backup_specs, validate_managed_file_specs, validate_package_specs,
    validate_secret_specs,
};
use super::source::{EnvVar, ShellAlias};
use crate::errors::{ConfigError, Result};
use crate::{deep_merge_yaml, union_extend};

// --- Profile Resolution ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum LayerPolicy {
    Local,
    Required,
    Recommended,
    Optional,
}

/// The `ProfileLayer::source` value every layer of the subscriber's OWN profile
/// carries. Composition tags a source's layers with the source name, so this is
/// what distinguishes "the operator wrote it" from "a subscription delivered
/// it" — one definition, because a mismatched literal silently reclassifies a
/// layer's owner.
pub const LOCAL_LAYER: &str = "local";

#[derive(Debug, Clone, Serialize)]
pub struct ProfileLayer {
    pub source: String,
    pub profile_name: String,
    pub priority: u32,
    pub policy: LayerPolicy,
    pub spec: ProfileSpec,
}

impl ProfileLayer {
    /// The `kind:name` owner token this layer's entries carry. A layer the
    /// operator wrote is their profile; every other layer arrived from the
    /// subscription named on it. Built through [`crate::reconciler::Owner`] so
    /// the token is spelled exactly as an apply header spells it.
    pub fn owner_token(&self) -> String {
        if self.source == LOCAL_LAYER {
            crate::reconciler::Owner::profile(&self.profile_name).token()
        } else {
            crate::reconciler::Owner::source(&self.source).token()
        }
    }
}

/// The one env var whose declarations concatenate, and so whose surviving
/// value can have several owners.
const PATH_VAR: &str = "PATH";

/// Which layer declared each env var and alias that SURVIVED the layer merge.
///
/// Recorded by the merge rather than re-derived from the layer list, because
/// last-writer-wins is the merge's own rule: a second walk applying it again is
/// a second implementation, and the moment the two disagree the comment beside
/// a generated line names a layer whose value is not there.
///
/// Names only — the value is whatever survived, and the token says who put it
/// there. Display-only: `#[serde(skip)]` where it hangs off [`MergedProfile`],
/// and never persisted or matched. `PATH` is the one name that can carry
/// SEVERAL owner tokens, space-separated in fold order, because its
/// declarations concatenate instead of displacing one another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryOwners {
    pub env: std::collections::HashMap<String, String>,
    pub aliases: std::collections::HashMap<String, String>,
}

impl EntryOwners {
    /// Record `owner` against every entry it declares, overwriting an earlier
    /// claim exactly as the merge overwrites its value.
    pub fn claim(&mut self, owner: &str, env: &[EnvVar], aliases: &[ShellAlias]) {
        self.claim_env_names(owner, env.iter().map(|ev| ev.name.as_str()));
        for alias in aliases {
            self.aliases.insert(alias.name.clone(), owner.to_string());
        }
    }

    /// The same claim for entries a layer declares by NAME alone — a
    /// `spec.secrets[].envs` export, whose value only exists once a backend has
    /// resolved it but whose line in the generated file is the declaring
    /// layer's exactly as a plain `spec.env` entry is.
    pub fn claim_env_names<'a>(&mut self, owner: &str, names: impl IntoIterator<Item = &'a str>) {
        for name in names {
            match self.env.get_mut(name) {
                // `PATH` is the one name whose declarations CONCATENATE
                // (`fold_env_layer`), so its surviving value has as many
                // authors as contributed to it and a single claim would name
                // one of them over directories the others put there.
                Some(existing) if name == PATH_VAR => {
                    if !existing.split_whitespace().any(|t| t == owner) {
                        existing.push(' ');
                        existing.push_str(owner);
                    }
                }
                _ => {
                    self.env.insert(name.to_string(), owner.to_string());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedProfile {
    pub layers: Vec<ProfileLayer>,
    pub merged: MergedProfile,
}

impl ResolvedProfile {
    /// Name of the profile this resolution is *for*: the last layer in the
    /// chain (bases are resolved first, the requested profile last). Falls back
    /// to `"unknown"` for a synthesized layer-free profile so callers that stamp
    /// the name into script metadata always have a value.
    pub fn profile_name(&self) -> &str {
        self.layers
            .last()
            .map(|l| l.profile_name.as_str())
            .unwrap_or("unknown")
    }

    /// The requested profile's resolved `inherits:` chain, nearest parent
    /// first (`base` → `core` → `shared` renders `["core", "shared"]`).
    ///
    /// Filtered to `LOCAL_LAYER` layers, never the full (possibly
    /// composed) list: [`compose`](crate::composition::compose)
    /// appends source layers and re-sorts by priority, but every local
    /// layer keeps priority 1000 and a stable sort preserves their
    /// ancestor-first relative order among themselves — so this reads the
    /// same chain whether `self` came from a bare local resolve or a
    /// composed one. Empty for a profile that declares no `inherits:`, or
    /// a synthesized layer-free profile.
    pub fn inherits_chain(&self) -> Vec<String> {
        let mut local_names: Vec<&str> = self
            .layers
            .iter()
            .filter(|layer| layer.source == LOCAL_LAYER)
            .map(|layer| layer.profile_name.as_str())
            .collect();
        // The last local layer is the requested profile itself, not a parent.
        local_names.pop();
        local_names.into_iter().rev().map(String::from).collect()
    }

    /// Every env var name a declared secret exports, across every layer of the
    /// chain and the merge they fold into.
    ///
    /// The ONE derivation of the set `MaskEnvValues::Secrets` masks by: a
    /// declared value is a secret's value exactly when a `spec.secrets[].envs`
    /// entry names it. Both halves are read because a layer's secret can be
    /// dropped or rewritten by the fold, and a name the operator wrote in ANY
    /// layer of the chain they are looking at is still a secret's name.
    pub fn secret_env_names(&self) -> std::collections::BTreeSet<String> {
        let per_layer = self.layers.iter().flat_map(|layer| &layer.spec.secrets);
        per_layer
            .chain(self.merged.secrets.iter())
            .filter_map(|secret| secret.envs.as_ref())
            .flatten()
            .cloned()
            .collect()
    }
}

/// Which layer delivered each declared resource that is NOT an env var or an
/// alias: the packages, system settings, secrets and scripts a subscription can
/// put on the machine.
///
/// The sibling of [`EntryOwners`], and recorded by the merge for the same
/// reason: last-writer-wins is the merge's own rule, so a second walk applying
/// it again is a second implementation, and the row `cfgd source remove` looks
/// a subscription's resources up by would name a layer whose declaration is not
/// the one that survived. The value is [`ProfileLayer::source`] rather than the
/// `kind:name` token [`EntryOwners`] holds, because `managed_resources.source`
/// is what the removal selects on.
///
/// Display-free and never persisted: `#[serde(skip)]` where it hangs off
/// [`MergedProfile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerSources {
    /// `<manager>/<declared entry>` ([`crate::state::package_resource_id`]) to
    /// the layer that declared it. Keyed on the DECLARED entry, which is what a
    /// layer holds; a reader matching the row a manager's
    /// `package_identity` composes folds this key through the same function.
    pub packages: std::collections::HashMap<String, String>,
    /// `<configurator>.<key>`
    /// ([`crate::reconciler::system_resource_key`]) to the layer that declared
    /// that leaf, plus the bare `<configurator>` a whole withheld block records
    /// under.
    pub system: std::collections::HashMap<String, String>,
    /// `spec.secrets[].source`, the key the merge itself deduplicates a secret
    /// by, so one declaration answers for every action it mints.
    pub secrets: std::collections::HashMap<String, String>,
    /// A script's `run` string ([`cfgd_schema::ScriptEntry::run_str`]), the id
    /// its action records under.
    pub scripts: std::collections::HashMap<String, String>,
}

impl LayerSources {
    /// Record `source` against every package, system key, secret and script
    /// `spec` declares, overwriting an earlier claim exactly as the merge
    /// overwrites the declaration itself.
    pub fn claim(&mut self, source: &str, spec: &ProfileSpec) {
        if let Some(packages) = &spec.packages {
            for manager in packages.manager_names() {
                for package in desired_packages_for_spec(&manager, packages) {
                    self.packages.insert(
                        crate::state::package_resource_id(&manager, &package),
                        source.to_string(),
                    );
                }
            }
        }
        for (configurator, value) in &spec.system {
            // A block withheld whole records under the configurator alone, so
            // the last layer to declare anything under it answers for that row.
            self.system.insert(configurator.clone(), source.to_string());
            self.claim_system_keys(source, configurator, "", value);
        }
        for secret in &spec.secrets {
            self.secrets
                .insert(secret.source.clone(), source.to_string());
        }
        if let Some(scripts) = &spec.scripts {
            for entry in scripts.hooks().into_iter().flat_map(|(_, entries)| entries) {
                self.scripts
                    .insert(entry.run_str().to_string(), source.to_string());
            }
        }
    }

    /// Claim every leaf a configurator's declared mapping reaches, under the
    /// key its own drift row composes: one level for a flat mapping, two for a
    /// nested one (a `defaults` domain, a gsettings schema), which is as deep
    /// as `diff_nested_mapping` goes.
    pub(crate) fn claim_system_keys(
        &mut self,
        source: &str,
        configurator: &str,
        key_prefix: &str,
        value: &serde_yaml::Value,
    ) {
        let Some(mapping) = value.as_mapping() else {
            return;
        };
        for (key, inner) in mapping {
            let Some(key) = key.as_str() else { continue };
            let key_path = if key_prefix.is_empty() {
                key.to_string()
            } else {
                format!("{key_prefix}.{key}")
            };
            // A declared key repeating its own configurator's name is malformed
            // and composes no row; claiming it would trip the composer's own
            // assertion.
            if crate::reconciler::system_key_doubling_error(configurator, &key_path).is_none() {
                self.system.insert(
                    crate::reconciler::system_resource_key(configurator, &key_path),
                    source.to_string(),
                );
            }
            self.claim_system_keys(source, configurator, &key_path, inner);
        }
    }

    /// The layer to record `id` under for a resource of `kind`, as
    /// `action_resource_info` spells the pair. `fallback` covers an id no layer
    /// declares, which stays whatever the caller already had.
    pub fn recording_layer<'a>(&'a self, kind: &str, id: &str, fallback: &'a str) -> &'a str {
        let claimed = match kind {
            "package" => self.packages.get(id),
            "system" => self.system.get(id),
            "secret" => self.secrets.get(id),
            "script" => self.scripts.get(id),
            _ => None,
        };
        match claimed {
            Some(source) if !source.is_empty() => source,
            _ => fallback,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MergedProfile {
    pub modules: Vec<String>,
    pub env: Vec<EnvVar>,
    pub env_scope: EnvScope,
    pub aliases: Vec<ShellAlias>,
    pub packages: PackagesSpec,
    pub files: FilesSpec,
    pub system: SystemSettings,
    pub secrets: Vec<SecretSpec>,
    pub scripts: ScriptSpec,
    pub backups: Vec<BackupSpec>,
    /// Which layer declared each surviving `env`/`aliases` entry. Display-only,
    /// so a `-o json` reader sees the payload it always saw.
    #[serde(skip)]
    pub entry_owners: EntryOwners,
    /// Which layer delivered each surviving package, system key, secret and
    /// script. Never serialized, for the same reason.
    #[serde(skip)]
    pub layer_sources: LayerSources,
}

/// Resolve a profile by loading it and its full inheritance chain, then merging.
pub fn resolve_profile(profile_name: &str, profiles_dir: &Path) -> Result<ResolvedProfile> {
    // The order pass already parsed every manifest — consume its documents
    // instead of re-statting and re-parsing each one.
    let resolution_order = resolve_inheritance_order(profile_name, profiles_dir, &mut vec![])?;

    let layers: Vec<ProfileLayer> = resolution_order
        .into_iter()
        .map(|(name, doc)| ProfileLayer {
            source: LOCAL_LAYER.to_string(),
            profile_name: name,
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: doc.spec,
        })
        .collect();

    let merged = merge_layers(&layers);

    validate_secret_specs(&merged.secrets)?;
    validate_managed_file_specs(&merged.files.managed)?;
    validate_package_specs(&merged.packages)?;
    validate_backup_specs(&merged.backups)?;
    cfgd_schema::validate_script_bodies("profile", &merged.scripts)
        .map_err(|e| crate::errors::ConfigError::Invalid { message: e.0 })?;

    Ok(ResolvedProfile { layers, merged })
}

/// Recursively resolve the inheritance order (depth-first, left-to-right).
/// Returns `(name, parsed document)` pairs in resolution order: earliest
/// ancestor first, active profile last. Carrying the documents lets the
/// caller build layers without a second parse of every manifest.
fn resolve_inheritance_order(
    profile_name: &str,
    profiles_dir: &Path,
    visited: &mut Vec<String>,
) -> Result<Vec<(String, ProfileDocument)>> {
    if visited.contains(&profile_name.to_string()) {
        let mut chain = visited.clone();
        chain.push(profile_name.to_string());
        return Err(ConfigError::CircularInheritance { chain }.into());
    }

    visited.push(profile_name.to_string());

    let path = find_profile_path(profiles_dir, profile_name)?;
    let doc = load_profile(&path).map_err(|e| match e {
        crate::errors::CfgdError::Config(ConfigError::NotFound { .. }) => {
            crate::errors::CfgdError::Config(ConfigError::ProfileNotFound {
                name: profile_name.to_string(),
            })
        }
        other => other,
    })?;

    let mut order: Vec<(String, ProfileDocument)> = Vec::new();
    for parent in &doc.spec.inherits {
        let parent_order = resolve_inheritance_order(parent, profiles_dir, visited)?;
        for (name, parent_doc) in parent_order {
            if !order.iter().any(|(n, _)| *n == name) {
                order.push((name, parent_doc));
            }
        }
    }

    order.push((profile_name.to_string(), doc));
    visited.pop();

    Ok(order)
}

/// Merge profile layers according to merge rules:
/// - packages: union
/// - files: overlay (later overrides earlier for same target)
/// - env: override (later replaces earlier for same name)
/// - secrets: append (deduplicated by target)
/// - scripts: append in order
/// - system: deep merge (later overrides at leaf level)
/// - backups: append (deduplicated by name, later overrides)
pub fn merge_layers(layers: &[ProfileLayer]) -> MergedProfile {
    let mut merged = MergedProfile::default();

    for layer in layers {
        // Destructured with no `..`: a field added to `ProfileSpec` must fail
        // to compile here (and in `composition::merge_with_policy`) until
        // someone says what it merges to. Dropping one silently is not a
        // theoretical risk — `env_scope` was missing from the composing merge
        // and every machine with a subscription lost the scope it declared.
        let ProfileSpec {
            // Consumed by `resolve_inheritance_order` to BUILD the layer list;
            // by the time a layer exists its parents are already layers of
            // their own, so merging it again would be double-counting.
            inherits: _,
            modules,
            env,
            env_scope,
            aliases,
            packages,
            files,
            system,
            secrets,
            scripts,
            backups,
        } = &layer.spec;

        // Modules: union
        union_extend(&mut merged.modules, modules);

        let layer_owner = layer.owner_token();
        // Platform-gated entries are filtered BEFORE the fold: an entry this
        // host is not part of the desired state of must never reach a
        // last-writer-wins merge, where it would displace the value that does
        // apply and then have to be un-displaced.
        let platform = crate::platform::Platform::current();
        let env: Vec<EnvVar> = crate::platform::applicable_here(env, platform)
            .cloned()
            .collect();
        let aliases: Vec<ShellAlias> = crate::platform::applicable_here(aliases, platform)
            .cloned()
            .collect();
        // Env: later layer overrides earlier by name; `PATH` concatenates.
        crate::fold_env_layer(&mut merged.env, &env, crate::PATH_LIST_SEPARATOR);
        merged.entry_owners.claim(&layer_owner, &env, &aliases);
        merged.layer_sources.claim(&layer.source, &layer.spec);
        for secret in secrets {
            merged.entry_owners.claim_env_names(
                &layer_owner,
                secret.envs.iter().flatten().map(String::as_str),
            );
        }

        // EnvScope: last layer that *specifies* it wins; an omitting layer
        // inherits the value resolved so far (defaults to All if none set it).
        if let Some(scope) = env_scope {
            merged.env_scope = *scope;
        }

        // Aliases: later layer overrides earlier by name
        crate::merge_aliases(&mut merged.aliases, &aliases);

        // Packages: union (delegated to composition::merge_packages)
        if let Some(pkgs) = packages {
            crate::composition::merge_packages(&mut merged.packages, pkgs);
        }

        // Files: overlay (later layer overrides earlier for same target)
        if let Some(files) = files {
            // Destructured for the same reason `ProfileSpec` is: the guard has
            // to reach the nested specs too, or a field added to `FilesSpec`
            // is dropped by both merges with nothing failing to compile.
            let FilesSpec {
                managed: layer_managed,
                permissions,
            } = files;
            for managed in layer_managed {
                if let Some(existing) = merged
                    .files
                    .managed
                    .iter_mut()
                    .find(|m| m.target == managed.target)
                {
                    *existing = managed.clone();
                } else {
                    merged.files.managed.push(managed.clone());
                }
            }
            for (path, mode) in permissions {
                merged.files.permissions.insert(path.clone(), mode.clone());
            }
        }

        // System: deep merge at leaf level
        for (key, value) in system {
            deep_merge_yaml(
                merged
                    .system
                    .entry(key.clone())
                    .or_insert(serde_yaml::Value::Null),
                value,
            );
        }

        // Secrets: append, deduplicate by source (later layer overrides)
        for secret in secrets {
            if let Some(existing) = merged
                .secrets
                .iter_mut()
                .find(|s| s.source == secret.source)
            {
                *existing = secret.clone();
            } else {
                merged.secrets.push(secret.clone());
            }
        }

        // Scripts: append in order
        if let Some(scripts) = scripts {
            // Six hook vectors, and a seventh would otherwise be silently
            // dropped by both merges — every script a source or a parent
            // profile declared for the new hook would simply never run.
            let ScriptSpec {
                pre_apply,
                post_apply,
                pre_reconcile,
                post_reconcile,
                on_drift,
                on_change,
            } = scripts;
            merged.scripts.pre_apply.extend(pre_apply.clone());
            merged.scripts.post_apply.extend(post_apply.clone());
            merged.scripts.pre_reconcile.extend(pre_reconcile.clone());
            merged.scripts.post_reconcile.extend(post_reconcile.clone());
            merged.scripts.on_drift.extend(on_drift.clone());
            merged.scripts.on_change.extend(on_change.clone());
        }

        // Backups: append, deduplicate by name (later layer overrides)
        crate::merge_backups(&mut merged.backups, backups);
    }

    merged
}

/// Every built-in package-manager name [`desired_packages_for_spec`] resolves.
///
/// This is the single canonical list of built-in managers: it must stay in sync
/// with the match arms of [`desired_packages_for_spec`] (a co-located test
/// enforces that), and is the enumeration source for callers that need to walk
/// "every configured manager" without re-encoding which field each one reads.
/// Custom managers are not listed here — they are discovered from
/// [`PackagesSpec::custom`] by name.
pub const ALL_MANAGER_NAMES: &[&str] = &[
    "brew",
    "brew-tap",
    "brew-cask",
    "apt",
    "cargo",
    "npm",
    "pipx",
    "dnf",
    "apk",
    "pacman",
    "zypper",
    "yum",
    "pkg",
    "snap",
    "flatpak",
    "nix",
    "go",
    "winget",
    "chocolatey",
    "scoop",
];

/// One `--package` prefix a user may write, and what it resolves to.
///
/// `path` is the schema spelling (`brew.taps`), which is what a confirmation
/// line echoes back and what a "known:" list spells out. `slot` is the key
/// [`crate`]'s CLI writes the entry through, which is the REGISTERED manager
/// name wherever one exists and a sub-list key where the schema splits one
/// manager's list in two (`snap.classic`). `manager` is the registered manager
/// that actually installs it — the name a module entry's `prefer` may carry,
/// and the only one of the three that is ever persisted as a manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageSchemaPath {
    pub path: &'static str,
    pub slot: &'static str,
    pub manager: &'static str,
}

/// What a `--package` token names when the schema path says nothing more
/// specific.
pub const DEFAULT_PACKAGE_NOUN: &str = "package";

impl PackageSchemaPath {
    /// The word a confirmation line calls this path's entries — `tap`, `cask`,
    /// or plain `package`.
    ///
    /// Decided by the sub-list the path names, so a new sub-list picks its noun
    /// by being spelled after what it holds; `package_schema_paths_choose_a_noun`
    /// walks the whole table. `✓ Added package: charmbracelet/tap (brew.taps)`
    /// told the user cfgd had installed something it had merely made available.
    pub fn noun(&self) -> &'static str {
        match self.path.rsplit('.').next() {
            Some("taps") => "tap",
            Some("casks") => "cask",
            _ => DEFAULT_PACKAGE_NOUN,
        }
    }
}

/// Every `<manager>[.<list>]` path under `spec.packages` that holds package
/// NAMES, in the order a "known:" list spells them.
///
/// The user writes the SCHEMA path (`--package brew.taps:charmbracelet/tap`)
/// because that is the path the value is written to and read back from; the
/// registered manager name beside it (`brew-tap`) stays the wire spelling on
/// every persisted surface. A `file:` field is not here — it names a manifest,
/// not a package — and neither is `flatpak.remote`, which names a remote.
/// `custom[]` entries are discovered by name at parse time, exactly as
/// [`desired_packages_for_spec`] discovers them.
///
/// `package_schema_paths_cover_every_package_list_in_the_spec` walks a
/// fully-populated [`PackagesSpec`] against this table, so a sub-list added to
/// the schema fails until it is listed here.
pub const PACKAGE_SCHEMA_PATHS: &[PackageSchemaPath] = &[
    PackageSchemaPath {
        path: "apt",
        slot: "apt",
        manager: "apt",
    },
    PackageSchemaPath {
        path: "apt.packages",
        slot: "apt",
        manager: "apt",
    },
    PackageSchemaPath {
        path: "brew",
        slot: "brew",
        manager: "brew",
    },
    PackageSchemaPath {
        path: "brew.casks",
        slot: "brew-cask",
        manager: "brew-cask",
    },
    PackageSchemaPath {
        path: "brew.formulae",
        slot: "brew",
        manager: "brew",
    },
    PackageSchemaPath {
        path: "brew.taps",
        slot: "brew-tap",
        manager: "brew-tap",
    },
    PackageSchemaPath {
        path: "cargo",
        slot: "cargo",
        manager: "cargo",
    },
    PackageSchemaPath {
        path: "cargo.packages",
        slot: "cargo",
        manager: "cargo",
    },
    PackageSchemaPath {
        path: "npm",
        slot: "npm",
        manager: "npm",
    },
    PackageSchemaPath {
        path: "npm.global",
        slot: "npm",
        manager: "npm",
    },
    PackageSchemaPath {
        path: "snap",
        slot: "snap",
        manager: "snap",
    },
    PackageSchemaPath {
        path: "snap.classic",
        slot: "snap-classic",
        manager: "snap",
    },
    PackageSchemaPath {
        path: "snap.packages",
        slot: "snap",
        manager: "snap",
    },
    PackageSchemaPath {
        path: "flatpak",
        slot: "flatpak",
        manager: "flatpak",
    },
    PackageSchemaPath {
        path: "flatpak.packages",
        slot: "flatpak",
        manager: "flatpak",
    },
    PackageSchemaPath {
        path: "pipx",
        slot: "pipx",
        manager: "pipx",
    },
    PackageSchemaPath {
        path: "dnf",
        slot: "dnf",
        manager: "dnf",
    },
    PackageSchemaPath {
        path: "apk",
        slot: "apk",
        manager: "apk",
    },
    PackageSchemaPath {
        path: "pacman",
        slot: "pacman",
        manager: "pacman",
    },
    PackageSchemaPath {
        path: "zypper",
        slot: "zypper",
        manager: "zypper",
    },
    PackageSchemaPath {
        path: "yum",
        slot: "yum",
        manager: "yum",
    },
    PackageSchemaPath {
        path: "pkg",
        slot: "pkg",
        manager: "pkg",
    },
    PackageSchemaPath {
        path: "nix",
        slot: "nix",
        manager: "nix",
    },
    PackageSchemaPath {
        path: "go",
        slot: "go",
        manager: "go",
    },
    PackageSchemaPath {
        path: "winget",
        slot: "winget",
        manager: "winget",
    },
    PackageSchemaPath {
        path: "chocolatey",
        slot: "chocolatey",
        manager: "chocolatey",
    },
    PackageSchemaPath {
        path: "scoop",
        slot: "scoop",
        manager: "scoop",
    },
];

/// The schema path a user wrote, or `None` when nothing in the schema is
/// spelled that way.
pub fn package_schema_path(path: &str) -> Option<&'static PackageSchemaPath> {
    PACKAGE_SCHEMA_PATHS.iter().find(|p| p.path == path)
}

/// Get the list of desired packages for a specific package manager from a merged profile.
pub fn desired_packages_for(manager_name: &str, profile: &MergedProfile) -> Vec<String> {
    desired_packages_for_spec(manager_name, &profile.packages)
}

pub fn desired_packages_for_spec(manager_name: &str, packages: &PackagesSpec) -> Vec<String> {
    match manager_name {
        "brew" => packages
            .brew
            .as_ref()
            .map(|b| b.formulae.clone())
            .unwrap_or_default(),
        "brew-tap" => packages
            .brew
            .as_ref()
            .map(|b| b.taps.clone())
            .unwrap_or_default(),
        "brew-cask" => packages
            .brew
            .as_ref()
            .map(|b| b.casks.clone())
            .unwrap_or_default(),
        "apt" => packages
            .apt
            .as_ref()
            .map(|a| a.packages.clone())
            .unwrap_or_default(),
        "cargo" => packages
            .cargo
            .as_ref()
            .map(|c| c.packages.clone())
            .unwrap_or_default(),
        "npm" => packages
            .npm
            .as_ref()
            .map(|n| n.global.clone())
            .unwrap_or_default(),
        "pipx" => packages.pipx.clone(),
        "dnf" => packages.dnf.clone(),
        "apk" => packages.apk.clone(),
        "pacman" => packages.pacman.clone(),
        "zypper" => packages.zypper.clone(),
        "yum" => packages.yum.clone(),
        "pkg" => packages.pkg.clone(),
        "snap" => packages
            .snap
            .as_ref()
            .map(|s| {
                let mut all = s.packages.clone();
                for p in &s.classic {
                    if !all.contains(p) {
                        all.push(p.clone());
                    }
                }
                all
            })
            .unwrap_or_default(),
        "flatpak" => packages
            .flatpak
            .as_ref()
            .map(|f| f.packages.clone())
            .unwrap_or_default(),
        "nix" => packages.nix.clone(),
        "go" => packages.go.clone(),
        "winget" => packages.winget.clone(),
        "chocolatey" => packages.chocolatey.clone(),
        "scoop" => packages.scoop.clone(),
        _ => {
            // Check custom managers
            for custom in &packages.custom {
                if custom.name == manager_name {
                    return custom.packages.clone();
                }
            }
            Vec::new()
        }
    }
}

/// Cross-scope package-claiming primitive shared by every path that combines
/// module and profile packages.
///
/// Module packages are offered first, in module order, via [`Self::claim_module`]:
/// the first occurrence of a `(manager, name)` claims it and wins; later
/// duplicates (in the same or a later module) are dropped. The `script` manager
/// is exempt — a custom inline script is not package-manager-idempotent, so two
/// same-named scripts may differ and both must run. Profile packages are then
/// tested with [`Self::is_claimed`]; an already-claimed `(manager, name)` is
/// dropped so it installs only once (the module install wins).
#[derive(Debug, Default)]
pub struct PackageClaim {
    claimed: std::collections::HashSet<(String, String)>,
}

impl PackageClaim {
    /// Create an empty claim set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a claim from an already-computed set of `(manager, name)` keys.
    pub fn from_claimed(claimed: std::collections::HashSet<(String, String)>) -> Self {
        Self { claimed }
    }

    /// Offer a module package. Returns `true` if it is kept (claimed now, or a
    /// `script`-manager package which is always kept) and `false` if it
    /// duplicates an already-claimed `(manager, name)` and must be dropped.
    pub fn claim_module(&mut self, manager: &str, name: &str) -> bool {
        if manager == crate::SCRIPT_SENTINEL {
            return true;
        }
        self.claimed.insert((manager.to_string(), name.to_string()))
    }

    /// Whether a profile `(manager, name)` was already claimed by a module and
    /// must therefore be dropped from the profile scope.
    pub fn is_claimed(&self, manager: &str, name: &str) -> bool {
        self.claimed
            .contains(&(manager.to_string(), name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry in the table answers with the noun its own sub-list holds,
    /// so a confirmation line never calls a tap or a cask a "package". Walked
    /// over the whole table rather than spot-checked: a path added later picks
    /// its noun here or trips this test.
    #[test]
    fn package_schema_paths_choose_a_noun() {
        for path in PACKAGE_SCHEMA_PATHS {
            let expected = match path.path.rsplit('.').next() {
                Some("taps") => "tap",
                Some("casks") => "cask",
                _ => DEFAULT_PACKAGE_NOUN,
            };
            assert_eq!(
                path.noun(),
                expected,
                "{} names its entries {:?}",
                path.path,
                path.noun()
            );
        }
        // The two the sweep above derives from the same rule it is testing, as
        // literals: the sub-lists whose entries are NOT packages.
        assert_eq!(
            package_schema_path("brew.taps").map(|p| p.noun()),
            Some("tap")
        );
        assert_eq!(
            package_schema_path("brew.casks").map(|p| p.noun()),
            Some("cask")
        );
        assert_eq!(
            package_schema_path("brew.formulae").map(|p| p.noun()),
            Some("package")
        );
    }

    /// Walk a fully-populated `PackagesSpec` and require every list of package
    /// NAMES in it to be reachable by a `--package` prefix. Derived from the
    /// serialized schema rather than from a second hand-written list, so a
    /// sub-list added to `BrewSpec`/`SnapSpec`/… fails here until
    /// `PACKAGE_SCHEMA_PATHS` names it — which is the only thing standing
    /// between a new sub-list and a `--package` token silently landing in the
    /// platform's native manager.
    #[test]
    fn package_schema_paths_cover_every_package_list_in_the_spec() {
        use super::super::profile_spec::{
            AptSpec, BrewSpec, CargoSpec, FlatpakSpec, NpmSpec, SnapSpec,
        };
        // Every struct-form manager has to be PRESENT for its sub-lists to
        // serialize at all; an absent `Option` would hide them from the walk.
        let spec = PackagesSpec {
            brew: Some(BrewSpec::default()),
            apt: Some(AptSpec::default()),
            cargo: Some(CargoSpec::default()),
            npm: Some(NpmSpec::default()),
            snap: Some(SnapSpec::default()),
            flatpak: Some(FlatpakSpec::default()),
            ..Default::default()
        };
        let value = serde_json::to_value(&spec).expect("PackagesSpec serializes");
        let map = value.as_object().expect("a mapping");

        let mut expected: Vec<String> = Vec::new();
        for (key, field) in map {
            // Discovered by NAME at parse time, exactly as
            // `desired_packages_for_spec` discovers them.
            if key == "custom" {
                continue;
            }
            match field {
                serde_json::Value::Array(_) => expected.push(key.clone()),
                serde_json::Value::Object(inner) => {
                    // The bare form (`--package brew:x`) reaches the sub-list
                    // `FromPackageList` maps a bare YAML list onto.
                    expected.push(key.clone());
                    for (sub, sub_field) in inner {
                        // A `file:` names a manifest and `flatpak.remote` names
                        // a remote — neither holds package names.
                        if sub_field.is_array() {
                            expected.push(format!("{key}.{sub}"));
                        }
                    }
                }
                // An absent `Option<…Spec>` serializes as null and hides its
                // whole sub-list population from this walk, so the fixture
                // has to name it rather than the walk skipping it.
                serde_json::Value::Null => panic!(
                    "`spec.packages.{key}` is absent from this test's fixture; \
                     populate it so its sub-lists serialize and can be checked"
                ),
                // A scalar field of `PackagesSpec` itself is not a manager.
                _ => {}
            }
        }

        for path in &expected {
            assert!(
                package_schema_path(path).is_some(),
                "`spec.packages.{path}` holds package names but no `--package` \
                 prefix reaches it; add it to PACKAGE_SCHEMA_PATHS"
            );
        }
        for entry in PACKAGE_SCHEMA_PATHS {
            assert!(
                expected.iter().any(|p| p == entry.path),
                "`{}` is offered as a --package prefix but names no list in \
                 the schema",
                entry.path
            );
            assert!(
                ALL_MANAGER_NAMES.contains(&entry.manager),
                "`{}` resolves to `{}`, which is not a registered manager",
                entry.path,
                entry.manager
            );
        }
    }

    /// Plant a name a command line would read as syntax into every package
    /// list the schema offers, through the real deserializer, and require the
    /// parse-boundary validator to refuse each one under its own field path.
    ///
    /// Walked over `PACKAGE_SCHEMA_PATHS` rather than over a hand-written list
    /// of managers, so a manager or sub-list added to the schema is checked
    /// here the day it joins the table.
    #[test]
    fn every_package_list_the_schema_offers_refuses_a_name_that_carries_a_metacharacter() {
        for entry in PACKAGE_SCHEMA_PATHS {
            let yaml = match entry.path.split_once('.') {
                Some((manager, sublist)) => format!("{manager}:\n  {sublist}: [\"foo&calc\"]\n"),
                None => format!("{}: [\"foo&calc\"]\n", entry.path),
            };
            let spec: PackagesSpec = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("`{}` parses as a package list: {e}", entry.path));
            let why = validate_package_specs(&spec)
                .expect_err(&format!("`{}` refuses a name carrying '&'", entry.path))
                .to_string();
            assert!(
                why.contains("foo&calc") && why.contains("spec.packages."),
                "`{}` names the offending entry and its field path: {why}",
                entry.path
            );
        }
    }

    /// A custom manager's own list is reached by the same walk, even though it
    /// is discovered by name rather than named in the schema-path table.
    #[test]
    fn a_custom_managers_package_list_refuses_a_name_that_carries_a_metacharacter() {
        let spec: PackagesSpec = serde_yaml::from_str(
            "custom:\n  - name: asdf\n    check: 'command -v asdf'\n    listInstalled: 'asdf list'\n    install: 'asdf install {package}'\n    uninstall: 'asdf uninstall {package}'\n    packages: [\"nodejs&calc\"]\n",
        )
        .expect("a custom manager parses");
        let why = validate_package_specs(&spec)
            .expect_err("a custom manager's list is judged too")
            .to_string();
        assert!(
            why.contains("spec.packages.custom[0].packages[0]") && why.contains("nodejs&calc"),
            "the refusal names the custom entry's own path: {why}"
        );
    }

    /// The spellings real ecosystems use survive every list form, so the
    /// refusal cannot quietly widen into a legitimate profile.
    #[test]
    fn the_package_name_gate_admits_the_spellings_real_ecosystems_use() {
        let spec: PackagesSpec = serde_yaml::from_str(
            "brew:\n  taps: [charmbracelet/tap]\n  formulae: [foo+bar, foo~bar]\n  casks: [foo_bar]\napt: [\"libfoo-dev:amd64\"]\nnpm:\n  global: [\"@scope/pkg\"]\ncargo: [\"foo@1.2\"]\ngo: [\"github.com/x/y@latest\"]\npkg: [devel/py-pipx]\nwinget: [Microsoft.VisualStudio.2022.Community]\npipx: [\"foo[extra]\"]\n",
        )
        .expect("a profile of real package spellings parses");
        validate_package_specs(&spec).expect("every spelling a real ecosystem uses is admitted");
    }

    fn layer(name: &str, env_scope: Option<EnvScope>) -> ProfileLayer {
        ProfileLayer {
            source: "local".to_string(),
            profile_name: name.to_string(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env_scope,
                ..Default::default()
            },
        }
    }

    #[test]
    fn env_scope_defaults_to_all_when_no_layer_sets_it() {
        let merged = merge_layers(&[layer("base", None), layer("child", None)]);
        assert_eq!(merged.env_scope, EnvScope::All);
    }

    #[test]
    fn env_scope_child_omitting_inherits_parent_value() {
        // base sets Interactive; child omits — the parent's choice must survive.
        let merged = merge_layers(&[
            layer("base", Some(EnvScope::Interactive)),
            layer("child", None),
        ]);
        assert_eq!(merged.env_scope, EnvScope::Interactive);
    }

    #[test]
    fn env_scope_child_specifying_overrides_parent() {
        let merged = merge_layers(&[
            layer("base", Some(EnvScope::Interactive)),
            layer("child", Some(EnvScope::Login)),
        ]);
        assert_eq!(merged.env_scope, EnvScope::Login);
    }

    fn secret_layer(name: &str, envs: &[&str]) -> ProfileLayer {
        ProfileLayer {
            spec: ProfileSpec {
                secrets: vec![crate::config::SecretSpec {
                    source: format!("{name}-secret"),
                    target: None,
                    template: None,
                    backend: None,
                    envs: Some(envs.iter().map(|e| (*e).to_string()).collect()),
                }],
                ..Default::default()
            },
            ..layer(name, None)
        }
    }

    /// The set `MaskEnvValues::Secrets` masks by is the union over the WHOLE
    /// chain and the merge it folds into: a parent's secret still names a
    /// secret's value on the screen the operator is looking at, even where the
    /// child's fold no longer carries that secret. A secret declaring no
    /// `envs:` exports no name and contributes nothing.
    #[test]
    fn the_secret_env_names_of_a_chain_are_the_union_of_every_layer_and_the_merge() {
        let layers = vec![
            secret_layer("base", &["AWS_SECRET_ACCESS_KEY"]),
            secret_layer("child", &["GH_TOKEN"]),
            ProfileLayer {
                spec: ProfileSpec {
                    secrets: vec![crate::config::SecretSpec {
                        source: "no-envs".to_string(),
                        target: None,
                        template: None,
                        backend: None,
                        envs: None,
                    }],
                    ..Default::default()
                },
                ..layer("leaf", None)
            },
        ];
        // The two halves are read separately, because neither implies the
        // other: a layer's secret the fold dropped still named a secret on the
        // screen the operator is looking at, and a secret composition added is
        // in no layer of the chain at all.
        let mut merged = merge_layers(&layers);
        merged.secrets.clear();
        let layers_only = ResolvedProfile {
            layers: layers.clone(),
            merged,
        };
        let names = layers_only.secret_env_names();
        assert!(names.contains("AWS_SECRET_ACCESS_KEY"), "{names:?}");
        assert!(names.contains("GH_TOKEN"), "{names:?}");
        assert!(
            !names.contains("EDITOR"),
            "a name no secret exports is not in the set: {names:?}"
        );
        assert_eq!(names.len(), 2, "{names:?}");

        let merged_only = ResolvedProfile {
            layers: Vec::new(),
            merged: merge_layers(&layers),
        };
        let names = merged_only.secret_env_names();
        assert!(names.contains("AWS_SECRET_ACCESS_KEY"), "{names:?}");
        assert!(names.contains("GH_TOKEN"), "{names:?}");
        assert_eq!(names.len(), 2, "{names:?}");
    }
}
