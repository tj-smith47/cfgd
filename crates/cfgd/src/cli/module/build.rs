use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

#[allow(clippy::too_many_arguments)]
pub fn cmd_module_build(
    printer: &Printer,
    v2_printer: &PrinterV2,
    dir: &str,
    target: Option<&str>,
    base_image: Option<&str>,
    artifact: Option<&str>,
    sign: bool,
    key: Option<&str>,
) -> anyhow::Result<()> {
    let dir_path = Path::new(dir);
    if !dir_path.join("module.yaml").exists() {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            dir,
            "module_yaml_missing",
            format!(
                "Directory '{}' does not contain a module.yaml",
                dir_path.display()
            ),
            serde_json::json!({ "dir": dir }),
        ));
        anyhow::bail!(
            "Directory '{}' does not contain a module.yaml",
            dir_path.display()
        );
    }

    v2_printer.heading("Build Module");
    let mut header = vec![("Directory".to_string(), dir.to_string())];
    if let Some(t) = target {
        header.push(("Target".to_string(), t.to_string()));
    }
    if let Some(img) = base_image {
        header.push(("Base image".to_string(), img.to_string()));
    }
    v2_printer.kv_block(header);

    let default_platform = cfgd_core::oci::current_platform();
    let targets: Vec<&str> = target
        .map(|t| t.split(',').collect())
        .unwrap_or_else(|| vec![default_platform.as_str()]);

    let mut output_artifacts: Vec<String> = Vec::new();
    let mut digest_value: Option<String> = None;

    if targets.len() == 1 {
        let output_dir = cfgd_core::oci::build_module(dir_path, Some(targets[0]), base_image)
            .map_err(|e| {
                v2_printer.emit(cfgd_core::output_v2::error_doc(
                    dir,
                    "build_failed",
                    e.to_string(),
                    serde_json::json!({ "dir": dir, "target": targets[0] }),
                ));
                anyhow::anyhow!("{e}")
            })?;
        v2_printer.status_simple(Role::Ok, format!("Built to {}", output_dir.display()));
        output_artifacts.push(output_dir.display().to_string());

        if let Some(art) = artifact {
            let digest =
                cfgd_core::oci::push_module(&output_dir, art, Some(targets[0]), Some(printer))
                    .map_err(|e| {
                        v2_printer.emit(cfgd_core::output_v2::error_doc(
                            art,
                            "push_failed",
                            e.to_string(),
                            serde_json::json!({ "artifact": art, "target": targets[0] }),
                        ));
                        anyhow::anyhow!("{e}")
                    })?;
            v2_printer.status_simple(Role::Ok, format!("Pushed {art}"));
            v2_printer.kv("Digest", &digest);
            digest_value = Some(digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| {
                    v2_printer.emit(cfgd_core::output_v2::error_doc(
                        art,
                        "sign_failed",
                        e.to_string(),
                        serde_json::json!({ "artifact": art }),
                    ));
                    anyhow::anyhow!("{e}")
                })?;
                v2_printer.status_simple(Role::Ok, "Signed artifact");
            }
        }
    } else {
        let mut builds: Vec<(std::path::PathBuf, String)> = Vec::new();
        for t in &targets {
            let sp = v2_printer.spinner(format!("Building for {t}..."));
            let output_dir = match cfgd_core::oci::build_module(dir_path, Some(t), base_image) {
                Ok(d) => {
                    sp.finish_ok(format!("Built {t} to {}", d.display()));
                    d
                }
                Err(e) => {
                    sp.finish_fail(format!("Build failed for {t}"))
                        .detail(e.to_string());
                    v2_printer.emit(cfgd_core::output_v2::error_doc(
                        dir,
                        "build_failed",
                        e.to_string(),
                        serde_json::json!({ "dir": dir, "target": *t }),
                    ));
                    return Err(anyhow::anyhow!("{e}"));
                }
            };
            output_artifacts.push(output_dir.display().to_string());
            builds.push((output_dir, t.to_string()));
        }

        if let Some(art) = artifact {
            let build_refs: Vec<(&Path, &str)> = builds
                .iter()
                .map(|(dir, plat)| (dir.as_path(), plat.as_str()))
                .collect();
            let digest = cfgd_core::oci::push_module_multiplatform(&build_refs, art, Some(printer))
                .map_err(|e| {
                    v2_printer.emit(cfgd_core::output_v2::error_doc(
                        art,
                        "push_failed",
                        e.to_string(),
                        serde_json::json!({ "artifact": art, "targets": &targets }),
                    ));
                    anyhow::anyhow!("{e}")
                })?;
            v2_printer.status_simple(Role::Ok, format!("Pushed multi-platform index {art}"));
            v2_printer.kv("Digest", &digest);
            digest_value = Some(digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| {
                    v2_printer.emit(cfgd_core::output_v2::error_doc(
                        art,
                        "sign_failed",
                        e.to_string(),
                        serde_json::json!({ "artifact": art }),
                    ));
                    anyhow::anyhow!("{e}")
                })?;
                v2_printer.status_simple(Role::Ok, "Signed artifact");
            }
        }
    }

    let mut payload = serde_json::Map::new();
    payload.insert("dir".into(), serde_json::Value::String(dir.to_string()));
    payload.insert(
        "targets".into(),
        serde_json::Value::Array(
            targets
                .iter()
                .map(|t| serde_json::Value::String((*t).to_string()))
                .collect(),
        ),
    );
    payload.insert(
        "outputArtifacts".into(),
        serde_json::Value::Array(
            output_artifacts
                .iter()
                .map(|p| serde_json::Value::String(p.clone()))
                .collect(),
        ),
    );
    if let Some(art) = artifact {
        payload.insert(
            "artifact".into(),
            serde_json::Value::String(art.to_string()),
        );
    }
    if let Some(d) = digest_value {
        payload.insert("digest".into(), serde_json::Value::String(d));
    }
    payload.insert("signed".into(), serde_json::Value::Bool(sign));
    v2_printer.emit(Doc::new().with_data(serde_json::Value::Object(payload)));

    Ok(())
}
