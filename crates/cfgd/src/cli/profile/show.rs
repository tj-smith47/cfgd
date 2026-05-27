use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{
    EnvVar, ManagedFileSpec, PackagesSpec, ProfileLayer, ResolvedProfile, SecretSpec,
};
use cfgd_core::output::{Doc, Printer};

/// Build the `cfgd profile show` Doc from a resolved profile. Pure; consumes
/// nothing — the caller serializes `{name, resolved}` as the structured payload.
pub fn build_profile_show_doc(resolved: &ResolvedProfile, name: &str, config_path: &Path) -> Doc {
    let mut doc = Doc::new()
        .heading(format!("Profile: {}", name))
        .kv("Config", config_path.display_posix())
        .kv("Profile", name);

    doc = doc.section("Layers", |s| {
        resolved.layers.iter().fold(s, |s, layer: &ProfileLayer| {
            s.kv(
                &layer.profile_name,
                format!("source={} priority={}", layer.source, layer.priority),
            )
        })
    });

    let mut env_sorted: Vec<&EnvVar> = resolved.merged.env.iter().collect();
    env_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    doc = doc.section_if_nonempty("Env", &env_sorted, |s, items| {
        items.iter().fold(s, |s, ev| s.kv(&ev.name, &ev.value))
    });

    let package_rows = package_display_rows(&resolved.merged.packages);
    doc = doc.section_if_nonempty("Packages", &package_rows, |s, rows| {
        rows.iter().fold(s, |s, (label, value)| s.kv(label, value))
    });

    doc = doc.section_if_nonempty("Files", &resolved.merged.files.managed, |s, files| {
        files.iter().fold(s, |s, file: &ManagedFileSpec| {
            s.kv(&file.source, file.target.display_posix())
        })
    });

    let mut system_keys: Vec<&String> = resolved.merged.system.keys().collect();
    system_keys.sort();
    doc = doc.section_if_nonempty("System", &system_keys, |s, keys| {
        keys.iter().fold(s, |s, k| s.kv(k.as_str(), "(configured)"))
    });

    doc = doc.section_if_nonempty("Secrets", &resolved.merged.secrets, |s, secrets| {
        secrets.iter().fold(s, |s, secret: &SecretSpec| {
            let value = match (&secret.target, &secret.envs) {
                (Some(t), Some(envs)) => format!("{} (envs: {})", t.posix(), envs.join(", ")),
                (Some(t), None) => t.display_posix(),
                (None, Some(envs)) => format!("envs: {}", envs.join(", ")),
                (None, None) => "(invalid)".to_string(),
            };
            s.kv(&secret.source, value)
        })
    });

    doc.with_data(serde_json::json!({
        "name": name,
        "resolved": resolved,
    }))
}

/// Flatten a `PackagesSpec` into `(label, value)` rows in the same order the
/// pre-Doc handler printed them, so empty profiles produce zero rows (skipping
/// the section entirely) without an aggregated `has_packages` flag.
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
            config::load_config(&cli.config)?;
            let resolved = config::resolve_profile(n, &profiles_dir(cli))?;
            (n.to_string(), resolved)
        }
        None => {
            let (_cfg, active, resolved) = helpers::load_config_and_profile(cli)?;
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
