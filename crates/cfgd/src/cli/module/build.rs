use super::*;

pub(crate) fn cmd_module_build(
    printer: &Printer,
    dir: &str,
    target: Option<&str>,
    base_image: Option<&str>,
    artifact: Option<&str>,
    sign: bool,
    key: Option<&str>,
) -> anyhow::Result<()> {
    let dir_path = Path::new(dir);
    if !dir_path.join("module.yaml").exists() {
        anyhow::bail!(
            "Directory '{}' does not contain a module.yaml",
            dir_path.display()
        );
    }

    printer.header("Build Module");
    printer.key_value("Directory", dir);
    if let Some(t) = target {
        printer.key_value("Target", t);
    }
    if let Some(img) = base_image {
        printer.key_value("Base image", img);
    }

    let default_platform = cfgd_core::oci::current_platform();
    let targets: Vec<&str> = target
        .map(|t| t.split(',').collect())
        .unwrap_or_else(|| vec![default_platform.as_str()]);

    if targets.len() == 1 {
        let output_dir = cfgd_core::oci::build_module(dir_path, Some(targets[0]), base_image)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success(&format!("Built to {}", output_dir.display()));

        if let Some(art) = artifact {
            let digest =
                cfgd_core::oci::push_module(&output_dir, art, Some(targets[0]), Some(printer))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Pushed {art}"));
            printer.key_value("Digest", &digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| anyhow::anyhow!("{e}"))?;
                printer.success("Signed artifact");
            }
        }
    } else {
        let mut builds: Vec<(std::path::PathBuf, String)> = Vec::new();
        for t in &targets {
            printer.info(&format!("Building for {t}..."));
            let output_dir = cfgd_core::oci::build_module(dir_path, Some(t), base_image)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Built {t} to {}", output_dir.display()));
            builds.push((output_dir, t.to_string()));
        }

        if let Some(art) = artifact {
            let build_refs: Vec<(&Path, &str)> = builds
                .iter()
                .map(|(dir, plat)| (dir.as_path(), plat.as_str()))
                .collect();
            let digest = cfgd_core::oci::push_module_multiplatform(&build_refs, art, Some(printer))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            printer.success(&format!("Pushed multi-platform index {art}"));
            printer.key_value("Digest", &digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| anyhow::anyhow!("{e}"))?;
                printer.success("Signed artifact");
            }
        }
    }

    Ok(())
}
