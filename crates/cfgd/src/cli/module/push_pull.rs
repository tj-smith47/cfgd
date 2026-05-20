use super::*;
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

// --- OCI Push / Pull ---

pub struct PushOptions<'a> {
    pub platform: Option<&'a str>,
    pub apply: bool,
    pub sign: bool,
    pub key: Option<&'a str>,
    pub attest: bool,
}

pub fn cmd_module_push(
    v2_printer: &PrinterV2,
    dir: &str,
    artifact: &str,
    opts: PushOptions<'_>,
) -> anyhow::Result<()> {
    let PushOptions {
        platform,
        apply,
        sign,
        key,
        attest,
    } = opts;
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

    v2_printer.heading("Push Module");
    let mut header = vec![
        ("Directory".to_string(), dir.to_string()),
        ("Artifact".to_string(), artifact.to_string()),
    ];
    if let Some(p) = platform {
        header.push(("Platform".to_string(), p.to_string()));
    }
    v2_printer.kv_block(header);

    let digest = cfgd_core::oci::push_module(dir_path, artifact, platform, Some(v2_printer))
        .map_err(|e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                artifact,
                "push_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact, "dir": dir, "platform": platform }),
            ));
            anyhow::anyhow!("{e}")
        })?;

    v2_printer.kv("Digest", &digest);

    // Sign if requested
    if sign {
        cfgd_core::oci::sign_artifact(artifact, key).map_err(|e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                artifact,
                "sign_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact }),
            ));
            anyhow::anyhow!("{e}")
        })?;
        v2_printer.status_simple(Role::Ok, "Signed artifact with cosign");
    }

    let mut attestation_attached = false;

    // Attach SLSA provenance attestation if requested
    if attest {
        let repo = detect_git_remote().unwrap_or_else(|| "unknown".to_string());
        let commit = detect_git_head().unwrap_or_else(|| "unknown".to_string());

        let provenance = cfgd_core::oci::generate_slsa_provenance(
            artifact, &digest, &repo, &commit,
        )
        .map_err(|e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                artifact,
                "attest_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact, "digest": digest, "step": "provenance" }),
            ));
            anyhow::anyhow!("{e}")
        })?;
        let tmp = tempfile::NamedTempFile::new()?;
        std::fs::write(tmp.path(), &provenance)?;
        cfgd_core::oci::attach_attestation(artifact, &tmp.path().display().to_string(), key)
            .map_err(|e| {
                v2_printer.emit(cfgd_core::output_v2::error_doc(
                    artifact,
                    "attest_failed",
                    e.to_string(),
                    serde_json::json!({ "artifact": artifact, "step": "attach" }),
                ));
                anyhow::anyhow!("{e}")
            })?;
        v2_printer.status_simple(Role::Ok, "Attached SLSA provenance attestation");
        attestation_attached = true;
    }

    let mut applied_name: Option<String> = None;
    if apply {
        let module_yaml = std::fs::read_to_string(dir_path.join("module.yaml"))?;
        let module_doc = cfgd_core::config::parse_module(&module_yaml)
            .map_err(|e| anyhow::anyhow!("Failed to parse module.yaml: {e}"))?;

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(apply_module_crd(v2_printer, &module_doc, artifact))?;
        applied_name = Some(module_doc.metadata.name.clone());
    }

    v2_printer.emit(Doc::new().with_data(serde_json::json!({
        "dir": dir,
        "artifact": artifact,
        "platform": platform,
        "digest": digest,
        "signed": sign,
        "attestation": attestation_attached,
        "applied": applied_name,
    })));

    Ok(())
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = cfgd_core::git_cmd_local().args(args).output().ok()?;
    if output.status.success() {
        Some(cfgd_core::stdout_lossy_trimmed(&output))
    } else {
        None
    }
}

fn detect_git_remote() -> Option<String> {
    git_output(&["remote", "get-url", "origin"])
}

fn detect_git_head() -> Option<String> {
    git_output(&["rev-parse", "HEAD"])
}

#[cfg(test)]
pub(super) fn detect_git_remote_for_tests() -> Option<String> {
    detect_git_remote()
}

#[cfg(test)]
pub(super) fn detect_git_head_for_tests() -> Option<String> {
    detect_git_head()
}

pub(super) fn build_module_crd_json(
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": cfgd_core::API_VERSION,
        "kind": "Module",
        "metadata": {
            "name": &module_doc.metadata.name,
        },
        "spec": {
            "ociArtifact": artifact,
            "packages": module_doc.spec.packages.iter().map(|p| {
                serde_json::json!({ "name": p.name })
            }).collect::<Vec<_>>(),
            "files": module_doc.spec.files.iter().map(|f| {
                serde_json::json!({
                    "source": f.source,
                    "target": f.target,
                })
            }).collect::<Vec<_>>(),
            "depends": module_doc.spec.depends,
        }
    })
}

async fn apply_module_crd(
    v2_printer: &PrinterV2,
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
) -> anyhow::Result<()> {
    use kube::Client;
    use kube::api::{Api, Patch, PatchParams};

    let name = &module_doc.metadata.name;
    let client = Client::try_default().await.map_err(|e| {
        v2_printer.emit(cfgd_core::output_v2::error_doc(
            name,
            "crd_connect_failed",
            format!("Failed to connect to cluster: {e}"),
            serde_json::json!({ "artifact": artifact }),
        ));
        anyhow::anyhow!("Failed to connect to cluster: {e}")
    })?;

    let module_json = build_module_crd_json(module_doc, artifact);

    let modules: Api<kube::core::DynamicObject> = Api::all_with(
        client,
        &kube::discovery::ApiResource {
            group: "cfgd.io".into(),
            version: "v1alpha1".into(),
            api_version: cfgd_core::API_VERSION.into(),
            kind: "Module".into(),
            plural: "modules".into(),
        },
    );

    modules
        .patch(
            name,
            &PatchParams::apply("cfgd"),
            &Patch::Apply(module_json),
        )
        .await
        .map_err(|e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                name,
                "crd_apply_failed",
                format!("Failed to apply Module CRD: {e}"),
                serde_json::json!({ "artifact": artifact }),
            ));
            anyhow::anyhow!("Failed to apply Module CRD: {e}")
        })?;

    v2_printer.status_simple(Role::Ok, format!("Applied Module CRD '{name}' to cluster"));
    Ok(())
}

pub fn cmd_module_pull(
    v2_printer: &PrinterV2,
    artifact_ref: &str,
    output: &str,
    require_signature: bool,
    verify_attestation: bool,
    verify_opts: cfgd_core::oci::VerifyOptions<'_>,
) -> anyhow::Result<()> {
    let output_path = Path::new(output);

    v2_printer.heading("Pull Module");
    v2_printer.kv_block([("Artifact", artifact_ref), ("Output", output)]);

    // Verify signature if requested (uses cosign, not the old tag-check)
    if require_signature {
        cfgd_core::oci::verify_signature(artifact_ref, &verify_opts).map_err(|e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                artifact_ref,
                "verify_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact_ref, "step": "signature" }),
            ));
            anyhow::anyhow!("{e}")
        })?;
        v2_printer.status_simple(Role::Ok, "Signature verified");
    }

    // Verify attestation if requested
    if verify_attestation {
        cfgd_core::oci::verify_attestation(artifact_ref, "slsaprovenance", &verify_opts).map_err(
            |e| {
                v2_printer.emit(cfgd_core::output_v2::error_doc(
                    artifact_ref,
                    "verify_failed",
                    e.to_string(),
                    serde_json::json!({ "artifact": artifact_ref, "step": "attestation" }),
                ));
                anyhow::anyhow!("{e}")
            },
        )?;
        v2_printer.status_simple(Role::Ok, "SLSA provenance attestation verified");
    }

    // Pull uses the existing require_signature=false since we've already verified above
    cfgd_core::oci::pull_module(artifact_ref, output_path, false, Some(v2_printer)).map_err(
        |e| {
            v2_printer.emit(cfgd_core::output_v2::error_doc(
                artifact_ref,
                "pull_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact_ref, "output": output }),
            ));
            anyhow::anyhow!("{e}")
        },
    )?;

    v2_printer.status_simple(Role::Ok, format!("Pulled {artifact_ref} to {output}"));

    // Check if module.yaml exists in extracted content
    let mut module_name: Option<String> = None;
    let mut module_description: Option<String> = None;
    let mut package_count: Option<usize> = None;
    let mut file_count: Option<usize> = None;
    if output_path.join("module.yaml").exists() {
        let contents = std::fs::read_to_string(output_path.join("module.yaml"))?;
        if let Ok(doc) = cfgd_core::config::parse_module(&contents) {
            let mut pairs = vec![("Module".to_string(), doc.metadata.name.clone())];
            if let Some(desc) = &doc.metadata.description {
                pairs.push(("Description".to_string(), desc.clone()));
            }
            pairs.push(("Packages".to_string(), doc.spec.packages.len().to_string()));
            pairs.push(("Files".to_string(), doc.spec.files.len().to_string()));
            v2_printer.kv_block(pairs);
            module_name = Some(doc.metadata.name.clone());
            module_description = doc.metadata.description.clone();
            package_count = Some(doc.spec.packages.len());
            file_count = Some(doc.spec.files.len());
        }
    }

    v2_printer.emit(Doc::new().with_data(serde_json::json!({
        "artifact": artifact_ref,
        "output": output,
        "signatureVerified": require_signature,
        "attestationVerified": verify_attestation,
        "moduleName": module_name,
        "moduleDescription": module_description,
        "packageCount": package_count,
        "fileCount": file_count,
    })));

    Ok(())
}
