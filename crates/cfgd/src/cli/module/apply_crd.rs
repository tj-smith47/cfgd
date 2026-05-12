use super::*;

// --- OCI Push / Pull ---

pub(crate) struct PushOptions<'a> {
    pub platform: Option<&'a str>,
    pub apply: bool,
    pub sign: bool,
    pub key: Option<&'a str>,
    pub attest: bool,
}

pub(crate) fn cmd_module_push(
    printer: &Printer,
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
        anyhow::bail!(
            "Directory '{}' does not contain a module.yaml",
            dir_path.display()
        );
    }

    printer.header("Push Module");
    printer.key_value("Directory", dir);
    printer.key_value("Artifact", artifact);
    if let Some(p) = platform {
        printer.key_value("Platform", p);
    }

    let digest = cfgd_core::oci::push_module(dir_path, artifact, platform, Some(printer))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    printer.success(&format!("Pushed {artifact}"));
    printer.key_value("Digest", &digest);

    // Sign if requested
    if sign {
        cfgd_core::oci::sign_artifact(artifact, key).map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Signed artifact with cosign");
    }

    // Attach SLSA provenance attestation if requested
    if attest {
        let repo = detect_git_remote().unwrap_or_else(|| "unknown".to_string());
        let commit = detect_git_head().unwrap_or_else(|| "unknown".to_string());

        let provenance =
            cfgd_core::oci::generate_slsa_provenance(artifact, &digest, &repo, &commit)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        let tmp = tempfile::NamedTempFile::new()?;
        std::fs::write(tmp.path(), &provenance)?;
        cfgd_core::oci::attach_attestation(artifact, &tmp.path().display().to_string(), key)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Attached SLSA provenance attestation");
    }

    if apply {
        let module_yaml = std::fs::read_to_string(dir_path.join("module.yaml"))?;
        let module_doc = cfgd_core::config::parse_module(&module_yaml)
            .map_err(|e| anyhow::anyhow!("Failed to parse module.yaml: {e}"))?;

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(apply_module_crd(printer, &module_doc, artifact))?;
    }

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
    printer: &Printer,
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
) -> anyhow::Result<()> {
    use kube::Client;
    use kube::api::{Api, Patch, PatchParams};

    let client = Client::try_default()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to cluster: {e}"))?;

    let name = &module_doc.metadata.name;
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
        .map_err(|e| anyhow::anyhow!("Failed to apply Module CRD: {e}"))?;

    printer.success(&format!("Applied Module CRD '{name}' to cluster"));
    Ok(())
}

pub(crate) fn cmd_module_pull(
    printer: &Printer,
    artifact_ref: &str,
    output: &str,
    require_signature: bool,
    verify_attestation: bool,
    verify_opts: cfgd_core::oci::VerifyOptions<'_>,
) -> anyhow::Result<()> {
    let output_path = Path::new(output);

    printer.header("Pull Module");
    printer.key_value("Artifact", artifact_ref);
    printer.key_value("Output", output);

    // Verify signature if requested (uses cosign, not the old tag-check)
    if require_signature {
        cfgd_core::oci::verify_signature(artifact_ref, &verify_opts)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("Signature verified");
    }

    // Verify attestation if requested
    if verify_attestation {
        cfgd_core::oci::verify_attestation(artifact_ref, "slsaprovenance", &verify_opts)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        printer.success("SLSA provenance attestation verified");
    }

    // Pull uses the existing require_signature=false since we've already verified above
    cfgd_core::oci::pull_module(artifact_ref, output_path, false, Some(printer))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    printer.success(&format!("Pulled {artifact_ref} to {output}"));

    // Check if module.yaml exists in extracted content
    if output_path.join("module.yaml").exists() {
        let contents = std::fs::read_to_string(output_path.join("module.yaml"))?;
        if let Ok(doc) = cfgd_core::config::parse_module(&contents) {
            printer.key_value("Module", &doc.metadata.name);
            if let Some(desc) = &doc.metadata.description {
                printer.key_value("Description", desc);
            }
            printer.key_value("Packages", &doc.spec.packages.len().to_string());
            printer.key_value("Files", &doc.spec.files.len().to_string());
        }
    }

    Ok(())
}
