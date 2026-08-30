use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{
    EnvVar, ManagedFileSpec, PackagesSpec, ProfileLayer, ResolvedProfile, SecretSpec, ShellAlias,
};
use cfgd_core::output::{Doc, KvPair, Printer};

/// Build the `cfgd profile show` Doc from a resolved profile. Pure; consumes
/// nothing — the caller serializes `{name, resolved}` as the structured payload.
pub fn build_profile_show_doc(resolved: &ResolvedProfile, name: &str, config_path: &Path) -> Doc {
    // header-row-ok: the heading names the profile, the `Layers` section below
    // names every source it composed from, and the blocks under that ARE the
    // module inventory — so this header states the config file alone.
    let mut doc =
        Doc::new()
            .heading_title("Profile", name)
            .kv_rows(cfgd_core::output::config_header_rows(
                &cfgd_core::output::ConfigHeader {
                    config_path: Some(config_path),
                    sources: &[],
                    profile: None,
                    modules: &[],
                },
            ));

    doc = doc.section("Layers", |s| {
        resolved.layers.iter().fold(s, |s, layer: &ProfileLayer| {
            s.kv(
                &layer.profile_name,
                format!("source={} priority={}", layer.source, layer.priority),
            )
        })
    });

    for (name, rows) in profile_inventory_blocks(resolved) {
        if rows.is_empty() {
            continue;
        }
        doc = doc.section(name, |s| s.kv_rows(rows));
    }

    doc.with_data(serde_json::json!({
        "name": name,
        "resolved": resolved,
    }))
}

/// A profile's own inventory — Aliases, Env, Packages, Files, System, Secrets
/// — as named blocks of kv rows, aliases leading the shell pair as they do on
/// every surface that names both. A block with no rows is returned empty rather than omitted,
/// so a caller decides whether an empty block is a skipped section or an
/// empty-state one.
///
/// The ONE derivation of those rows. `cfgd profile show` renders each block as
/// a top-level section; `cfgd source show` / `cfgd source add` render the same
/// blocks as subsections under the `profile:<name>` owner of each profile the
/// source provides. Only the section DEPTH differs, so what a subscriber reads
/// before subscribing and what they read afterwards cannot say different
/// things about the same profile.
pub fn profile_inventory_blocks(resolved: &ResolvedProfile) -> Vec<(&'static str, Vec<KvPair>)> {
    let mut env_sorted: Vec<&EnvVar> = resolved.merged.env.iter().collect();
    env_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut aliases_sorted: Vec<&ShellAlias> = resolved.merged.aliases.iter().collect();
    aliases_sorted.sort_by(|a, b| a.name.cmp(&b.name));

    vec![
        (
            "Aliases",
            aliases_sorted
                .iter()
                .map(|al| KvPair::new(&al.name, &al.command))
                .collect(),
        ),
        (
            "Env",
            env_sorted
                .iter()
                .map(|ev| KvPair::new(&ev.name, &ev.value))
                .collect(),
        ),
        (
            "Packages",
            package_display_rows(&resolved.merged.packages)
                .into_iter()
                .map(|(label, value)| KvPair::new(label, value))
                .collect(),
        ),
        (
            "Files",
            resolved
                .merged
                .files
                .managed
                .iter()
                .map(|file: &ManagedFileSpec| {
                    KvPair::new(&file.source, file.target.display_posix().to_string())
                })
                .collect(),
        ),
        (
            "System",
            resolved
                .merged
                .system
                .keys()
                .map(|k| KvPair::new(k.as_str(), "(configured)"))
                .collect(),
        ),
        (
            "Secrets",
            resolved
                .merged
                .secrets
                .iter()
                .map(|secret: &SecretSpec| {
                    let value = match (&secret.target, &secret.envs) {
                        (Some(t), Some(envs)) => {
                            format!("{} (envs: {})", t.posix(), envs.join(", "))
                        }
                        (Some(t), None) => t.display_posix().to_string(),
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

pub fn cmd_profile_show(cli: &Cli, printer: &Printer, name: Option<&str>) -> anyhow::Result<()> {
    let (profile_name, resolved) = match name {
        Some(n) => {
            let mut cfg = config::load_config(&cli.config)?;
            drain_config_deprecations(printer, &mut cfg);
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
                        hints.push(format!("Available profiles: {}", available.join(", ")));
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
            let (_cfg, active, resolved) = helpers::load_config_and_profile(cli, printer)?;
            (active, resolved)
        }
    };

    printer.emit(build_profile_show_doc(
        &resolved,
        &profile_name,
        &cli.config,
    ));
    Ok(())
}
