use super::*;

pub(crate) fn cmd_profile_show(
    cli: &Cli,
    printer: &Printer,
    name: Option<&str>,
) -> anyhow::Result<()> {
    let (_cfg, resolved) = match name {
        Some(n) => {
            let cfg = config::load_config(&cli.config)?;
            printer.key_value("Config", &cli.config.display().to_string());
            printer.key_value("Profile", n);
            let resolved = config::resolve_profile(n, &profiles_dir(cli))?;
            (cfg, resolved)
        }
        None => load_config_and_profile(cli, printer)?,
    };

    if printer.write_structured(&resolved) {
        return Ok(());
    }

    printer.header("Resolved Profile");
    printer.newline();
    printer.subheader("Layers");
    for layer in &resolved.layers {
        printer.key_value(
            &layer.profile_name,
            &format!("source={} priority={}", layer.source, layer.priority),
        );
    }

    printer.newline();
    printer.subheader("Env");
    if resolved.merged.env.is_empty() {
        printer.info("(none)");
    } else {
        let mut env: Vec<_> = resolved.merged.env.iter().collect();
        env.sort_by(|a, b| a.name.cmp(&b.name));
        for ev in env {
            printer.key_value(&ev.name, &ev.value);
        }
    }

    printer.newline();
    printer.subheader("Packages");
    let pkgs = &resolved.merged.packages;
    let mut has_packages = false;
    if let Some(ref brew) = pkgs.brew {
        if !brew.taps.is_empty() {
            printer.key_value("brew taps", &brew.taps.join(", "));
            has_packages = true;
        }
        if !brew.formulae.is_empty() {
            printer.key_value("brew formulae", &brew.formulae.join(", "));
            has_packages = true;
        }
        if !brew.casks.is_empty() {
            printer.key_value("brew casks", &brew.casks.join(", "));
            has_packages = true;
        }
    }
    if let Some(ref apt) = pkgs.apt
        && !apt.packages.is_empty()
    {
        printer.key_value("apt", &apt.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref cargo) = pkgs.cargo
        && !cargo.packages.is_empty()
    {
        printer.key_value("cargo", &cargo.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref npm) = pkgs.npm
        && !npm.global.is_empty()
    {
        printer.key_value("npm", &npm.global.join(", "));
        has_packages = true;
    }
    for (name, list) in pkgs.non_empty_simple_lists() {
        printer.key_value(name, &list.join(", "));
        has_packages = true;
    }
    if let Some(ref snap) = pkgs.snap
        && !snap.packages.is_empty()
    {
        printer.key_value("snap", &snap.packages.join(", "));
        has_packages = true;
    }
    if let Some(ref flatpak) = pkgs.flatpak
        && !flatpak.packages.is_empty()
    {
        printer.key_value("flatpak", &flatpak.packages.join(", "));
        has_packages = true;
    }
    if !has_packages {
        printer.info("(none)");
    }

    printer.newline();
    printer.subheader("Files");
    if resolved.merged.files.managed.is_empty() {
        printer.info("(none)");
    } else {
        for file in &resolved.merged.files.managed {
            printer.key_value(&file.source, &file.target.display().to_string());
        }
    }

    if !resolved.merged.system.is_empty() {
        printer.newline();
        printer.subheader("System");
        for key in resolved.merged.system.keys() {
            printer.key_value(key, "(configured)");
        }
    }

    if !resolved.merged.secrets.is_empty() {
        printer.newline();
        printer.subheader("Secrets");
        for secret in &resolved.merged.secrets {
            let value = match (&secret.target, &secret.envs) {
                (Some(t), Some(envs)) => {
                    format!("{} (envs: {})", t.display(), envs.join(", "))
                }
                (Some(t), None) => t.display().to_string(),
                (None, Some(envs)) => format!("envs: {}", envs.join(", ")),
                (None, None) => "(invalid)".to_string(),
            };
            printer.key_value(&secret.source, &value);
        }
    }

    Ok(())
}
