use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{
    EnvVar, FilesSpec, ManagedFileSpec, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    SecretSpec, ShellAlias,
};
use cfgd_core::output::{Doc, KvPair, Printer};

/// Build the `cfgd profile show` Doc.
///
/// The default view is the profile's OWN document: the `inherits:` it names
/// and the entries it declares itself, every `platforms:` gate annotated
/// rather than applied. `--resolved` renders what this host folds that chain
/// into — the layers that contributed and the merged inventories.
pub fn build_profile_show_doc(
    resolved: &ResolvedProfile,
    name: &str,
    config_path: &Path,
    sources: &[cfgd_core::reconciler::ComposedSource],
    arrow: &str,
    detail: crate::cli::InventoryDetail,
    show_resolved: bool,
) -> Doc {
    // header-row-ok: the heading names the profile and the blocks below ARE the
    // module inventory, so this header states the config file and what it
    // subscribes to. The `Layers` section is not that fact: it lists only the
    // sources that CONTRIBUTED a layer, so on a machine that has never synced
    // it names none while `spec.sources[]` names two.
    let mut doc =
        Doc::new()
            .heading_title("Profile", name)
            .kv_rows(cfgd_core::output::config_header_rows(
                &cfgd_core::output::ConfigHeader {
                    config_path: Some(config_path),
                    sources,
                    profile: None,
                    profile_inherits: &[],
                    modules: &[],
                    arrow,
                },
            ));

    doc = if show_resolved {
        build_profile_show_resolved_sections(doc, resolved, detail)
    } else {
        let own = own_profile_spec(resolved);
        let inherits: Vec<String> = own.map(|s| s.inherits.clone()).unwrap_or_default();
        let mut doc = if inherits.is_empty() {
            doc
        } else {
            doc.kv_rows(vec![KvPair::new("Inherits", inherits.join(", "))])
        };
        for (block, rows) in profile_inventory_blocks(own, detail) {
            if rows.is_empty() {
                continue;
            }
            doc = doc.section(block, |s| s.kv_rows(rows));
        }
        doc
    };

    doc.with_data(serde_json::json!({
        "name": name,
        "resolved": resolved,
    }))
}

/// The `--resolved` half of `cfgd profile show`: the layers that contributed
/// to this host's fold of the chain, and the merged inventories that fold
/// produced.
fn build_profile_show_resolved_sections(
    doc: Doc,
    resolved: &ResolvedProfile,
    detail: crate::cli::InventoryDetail,
) -> Doc {
    let mut doc = doc.section("Layers", |s| {
        resolved.layers.iter().fold(s, |s, layer: &ProfileLayer| {
            s.kv(
                &layer.profile_name,
                format!("source={} priority={}", layer.source, layer.priority),
            )
        })
    });

    let merged = &resolved.merged;
    let blocks = inventory_blocks(
        &merged.env,
        &merged.aliases,
        Some(&merged.packages),
        Some(&merged.files),
        &merged.system,
        &merged.secrets,
        detail,
    );
    for (block, rows) in blocks {
        if rows.is_empty() {
            continue;
        }
        doc = doc.section(block, |s| s.kv_rows(rows));
    }
    doc
}

/// The profile's OWN declared spec: the last layer the operator wrote, which
/// is the profile the invocation named. A resolution with no local layer at
/// all (a synthesized one) has no document to show.
pub fn own_profile_spec(resolved: &ResolvedProfile) -> Option<&ProfileSpec> {
    resolved
        .layers
        .iter()
        .rfind(|layer| layer.source == cfgd_core::config::LOCAL_LAYER)
        .map(|layer| &layer.spec)
}

/// A profile's DECLARED inventory — Aliases, Env, Packages, Files, System,
/// Secrets — as named blocks of kv rows, aliases leading the shell pair as
/// they do on every surface that names both. A block with no rows is returned
/// empty rather than omitted, so a caller decides whether an empty block is a
/// skipped section or an empty-state one.
///
/// The ONE derivation of those rows. `cfgd profile show` renders each block as
/// a top-level section; `cfgd source show` renders the same blocks as
/// subsections under the `profile:<name>` owner of each profile the source
/// provides. Only the section DEPTH differs, so what a subscriber reads
/// before subscribing and what they read afterwards cannot say different
/// things about the same profile.
///
/// Declared means the document's own words: a `platforms:`-gated entry is
/// listed with its annotation on every host, and every env value masks unless
/// the invocation asked to see it.
pub fn profile_inventory_blocks(
    spec: Option<&ProfileSpec>,
    detail: crate::cli::InventoryDetail,
) -> Vec<(&'static str, Vec<KvPair>)> {
    let Some(spec) = spec else {
        return inventory_blocks(&[], &[], None, None, &Default::default(), &[], detail);
    };
    inventory_blocks(
        &spec.env,
        &spec.aliases,
        spec.packages.as_ref(),
        spec.files.as_ref(),
        &spec.system,
        &spec.secrets,
        detail,
    )
}

/// The six inventory blocks over whichever set of entries the caller holds —
/// a profile's own declaration, or the merge of its whole chain. The two views
/// differ in what they carry, never in how a row reads.
fn inventory_blocks(
    env: &[EnvVar],
    aliases: &[ShellAlias],
    packages: Option<&PackagesSpec>,
    files: Option<&FilesSpec>,
    system: &cfgd_core::config::SystemSettings,
    secrets: &[SecretSpec],
    detail: crate::cli::InventoryDetail,
) -> Vec<(&'static str, Vec<KvPair>)> {
    let mut env_sorted: Vec<&EnvVar> = env.iter().collect();
    env_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut aliases_sorted: Vec<&ShellAlias> = aliases.iter().collect();
    aliases_sorted.sort_by(|a, b| a.name.cmp(&b.name));

    vec![
        (
            "Aliases",
            aliases_sorted
                .iter()
                .map(|al| {
                    KvPair::new(
                        &al.name,
                        crate::cli::module::list_show::gated_value(al.command.clone(), *al),
                    )
                })
                .collect(),
        ),
        (
            "Env",
            env_sorted
                .iter()
                .map(|ev| {
                    let value = if detail.values {
                        ev.value.clone()
                    } else {
                        crate::cli::module::keys::mask_value(&ev.value)
                    };
                    KvPair::new(
                        &ev.name,
                        crate::cli::module::list_show::gated_value(value, *ev),
                    )
                })
                .collect(),
        ),
        (
            "Packages",
            packages
                .map(package_display_rows)
                .unwrap_or_default()
                .into_iter()
                .map(|(label, value)| KvPair::new(label, value))
                .collect(),
        ),
        (
            "Files",
            files
                .map(|f| f.managed.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|file: &ManagedFileSpec| {
                    KvPair::new(
                        &file.source,
                        cfgd_core::fold_home_in_text(&file.target.display_posix()),
                    )
                })
                .collect(),
        ),
        (
            "System",
            system
                .keys()
                .map(|k| KvPair::new(k.as_str(), "(configured)"))
                .collect(),
        ),
        (
            "Secrets",
            secrets
                .iter()
                .map(|secret: &SecretSpec| {
                    let value = match (&secret.target, &secret.envs) {
                        (Some(t), Some(envs)) => {
                            format!(
                                "{} (envs: {})",
                                cfgd_core::fold_home_in_text(&t.display_posix()),
                                envs.join(", ")
                            )
                        }
                        (Some(t), None) => cfgd_core::fold_home_in_text(&t.display_posix()),
                        (None, Some(envs)) => format!("envs: {}", envs.join(", ")),
                        (None, None) => "(invalid)".to_string(),
                    };
                    KvPair::new(&secret.source, value)
                })
                .collect(),
        ),
    ]
}

/// Flatten a `PackagesSpec` into `(label, value)` rows in the same order the
/// pre-Doc handler printed them, so empty profiles produce zero rows (skipping
/// the section entirely) without an aggregated `has_packages` flag.
// name-row-ok: every key here is the `spec.packages` path the user wrote, so it
// stays in the config's own spelling rather than being Title Cased into a key
// no cfgd.yaml contains
fn package_display_rows(pkgs: &PackagesSpec) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if let Some(brew) = &pkgs.brew {
        if !brew.taps.is_empty() {
            rows.push(("brew taps".to_string(), brew.taps.join(", ")));
        }
        if !brew.formulae.is_empty() {
            rows.push(("brew formulae".to_string(), brew.formulae.join(", ")));
        }
        if !brew.casks.is_empty() {
            rows.push(("brew casks".to_string(), brew.casks.join(", ")));
        }
    }
    if let Some(apt) = &pkgs.apt
        && !apt.packages.is_empty()
    {
        rows.push(("apt".to_string(), apt.packages.join(", ")));
    }
    if let Some(cargo) = &pkgs.cargo
        && !cargo.packages.is_empty()
    {
        rows.push(("cargo".to_string(), cargo.packages.join(", ")));
    }
    if let Some(npm) = &pkgs.npm
        && !npm.global.is_empty()
    {
        rows.push(("npm".to_string(), npm.global.join(", ")));
    }
    for (name, list) in pkgs.non_empty_simple_lists() {
        rows.push((name.to_string(), list.join(", ")));
    }
    if let Some(snap) = &pkgs.snap
        && !snap.packages.is_empty()
    {
        rows.push(("snap".to_string(), snap.packages.join(", ")));
    }
    if let Some(flatpak) = &pkgs.flatpak
        && !flatpak.packages.is_empty()
    {
        rows.push(("flatpak".to_string(), flatpak.packages.join(", ")));
    }
    rows
}

pub fn cmd_profile_show(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
    resolved_view: bool,
    detail: crate::cli::InventoryDetail,
) -> anyhow::Result<()> {
    let declared;
    let (profile_name, resolved) = match name {
        Some(n) => {
            let mut cfg = config::load_config(&cli.config)?;
            drain_config_deprecations(printer, &mut cfg);
            declared = cfgd_core::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
            let dir = profiles_dir(cli);
            // resolve_profile already returns a typed ProfileNotFound (→ exit 6);
            // wrap the missing case with a `not_found` CliErrorMeta so structured
            // consumers get the stable `{"error":"not_found",...}` payload instead
            // of the generic synthesized fallback. The typed error stays in the
            // chain, so the exit code is unaffected.
            let resolved = config::resolve_profile(n, &dir).map_err(|e| {
                if matches!(
                    &e,
                    cfgd_core::errors::CfgdError::Config(
                        cfgd_core::errors::ConfigError::ProfileNotFound { .. }
                    )
                ) {
                    let available = super::available_profile_names(&dir);
                    let mut hints = Vec::new();
                    if !available.is_empty() {
                        hints.push(cfgd_core::output::HintCommands::from(format!(
                            "Available profiles: {}",
                            available.join(", ")
                        )));
                    }
                    crate::cli::cli_error_ctx_with_hints(
                        e.into(),
                        n,
                        "not_found",
                        format!("Profile '{}' not found", n),
                        serde_json::json!({ "available": available }),
                        hints,
                    )
                } else {
                    e.into()
                }
            })?;
            (n.to_string(), resolved)
        }
        None => {
            let (cfg, active, resolved) = helpers::load_config_and_profile(cli, printer)?;
            declared = cfgd_core::reconciler::ComposedSource::from_declared(&cfg.spec.sources);
            (active, resolved)
        }
    };

    printer.emit(build_profile_show_doc(
        &resolved,
        &profile_name,
        &cli.config,
        &declared,
        printer.arrow(),
        detail,
        resolved_view,
    ));
    Ok(())
}
