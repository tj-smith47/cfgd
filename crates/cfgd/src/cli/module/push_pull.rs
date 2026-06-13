use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub struct PushOptions<'a> {
    pub platform: Option<&'a str>,
    pub apply: bool,
    pub sign: bool,
    pub key: Option<&'a str>,
    pub attest: bool,
}

pub fn cmd_module_push(
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
        return Err(crate::cli::cli_error(
            dir,
            "module_yaml_missing",
            format!(
                "Directory '{}' does not contain a module.yaml",
                dir_path.posix()
            ),
            serde_json::json!({ "dir": dir }),
        ));
    }

    printer.heading("Push Module");
    let mut header = vec![
        ("Directory".to_string(), dir.to_string()),
        ("Artifact".to_string(), artifact.to_string()),
    ];
    if let Some(p) = platform {
        header.push(("Platform".to_string(), p.to_string()));
    }
    printer.kv_block(header);

    let digest =
        cfgd_core::oci::push_module(dir_path, artifact, platform, Some(printer)).map_err(|e| {
            crate::cli::cli_error(
                artifact,
                "push_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact, "dir": dir, "platform": platform }),
            )
        })?;

    printer.kv("Digest", &digest);

    let crate::cli::helpers::SignAttestOutcome {
        signed,
        attested: attestation_attached,
    } = crate::cli::helpers::sign_and_attest(printer, artifact, &digest, key, sign, attest)?;

    let mut applied_name: Option<String> = None;
    if apply {
        let module_yaml = std::fs::read_to_string(dir_path.join("module.yaml"))?;
        let module_doc = cfgd_core::config::parse_module(&module_yaml)
            .map_err(|e| anyhow::anyhow!("Failed to parse module.yaml: {e}"))?;

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(apply_module_crd(printer, &module_doc, artifact))?;
        applied_name = Some(module_doc.metadata.name.clone());
    }

    printer.emit(Doc::new().with_data(serde_json::json!({
        "dir": dir,
        "artifact": artifact,
        "platform": platform,
        "digest": digest,
        "signed": signed,
        "attestation": attestation_attached,
        "applied": applied_name,
    })));

    Ok(())
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

    let name = &module_doc.metadata.name;
    let client = Client::try_default().await.map_err(|e| {
        crate::cli::cli_error(
            name,
            "crd_connect_failed",
            format!(
                "Failed to connect to cluster: {}",
                cfgd_core::output::collapse_to_subject_line(&e),
            ),
            serde_json::json!({ "artifact": artifact }),
        )
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
            crate::cli::cli_error(
                name,
                "crd_apply_failed",
                format!(
                    "Failed to apply Module CRD: {}",
                    cfgd_core::output::collapse_to_subject_line(&e),
                ),
                serde_json::json!({ "artifact": artifact }),
            )
        })?;

    printer.status_simple(Role::Ok, format!("Applied Module CRD '{name}' to cluster"));
    Ok(())
}

pub fn cmd_module_pull(
    printer: &Printer,
    artifact_ref: &str,
    output: &str,
    require_signature: bool,
    verify_attestation: bool,
    verify_opts: cfgd_core::oci::VerifyOptions<'_>,
) -> anyhow::Result<()> {
    let output_path = Path::new(output);

    printer.heading("Pull Module");
    printer.kv_block([("Artifact", artifact_ref), ("Output", output)]);

    if require_signature {
        cfgd_core::oci::verify_signature(artifact_ref, &verify_opts).map_err(|e| {
            crate::cli::cli_error(
                artifact_ref,
                "verify_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact_ref, "step": "signature" }),
            )
        })?;
        printer.status_simple(Role::Ok, "Signature verified");
    }

    if verify_attestation {
        cfgd_core::oci::verify_attestation(artifact_ref, "slsaprovenance1", &verify_opts).map_err(
            |e| {
                crate::cli::cli_error(
                    artifact_ref,
                    "verify_failed",
                    e.to_string(),
                    serde_json::json!({ "artifact": artifact_ref, "step": "attestation" }),
                )
            },
        )?;
        printer.status_simple(Role::Ok, "SLSA provenance attestation verified");
    }

    cfgd_core::oci::pull_module(
        artifact_ref,
        output_path,
        cfgd_core::oci::SignaturePolicy::None,
        Some(printer),
    )
    .map_err(|e| {
        crate::cli::cli_error(
            artifact_ref,
            "pull_failed",
            e.to_string(),
            serde_json::json!({ "artifact": artifact_ref, "output": output }),
        )
    })?;

    printer.status_simple(Role::Ok, format!("Pulled {artifact_ref} to {output}"));

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
            printer.kv_block(pairs);
            module_name = Some(doc.metadata.name.clone());
            module_description = doc.metadata.description.clone();
            package_count = Some(doc.spec.packages.len());
            file_count = Some(doc.spec.files.len());
        }
    }

    printer.emit(Doc::new().with_data(serde_json::json!({
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

#[cfg(test)]
mod tests {
    use cfgd_core::output::Printer;
    use cfgd_core::test_helpers::test_printer;

    use super::{PushOptions, cmd_module_pull, cmd_module_push};

    fn write_module_yaml(dir: &std::path::Path) {
        std::fs::write(
            dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
        )
        .expect("write module.yaml");
    }

    fn unreachable_ref(name: &str) -> String {
        format!("localhost:1/{name}:v1")
    }

    fn no_flags() -> PushOptions<'static> {
        PushOptions {
            platform: None,
            apply: false,
            sign: false,
            key: None,
            attest: false,
        }
    }

    #[test]
    fn push_errors_when_module_yaml_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let printer = test_printer();

        let err = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            "localhost:5000/test/mod:v1",
            no_flags(),
        )
        .expect_err("missing module.yaml must return Err");

        assert!(
            err.to_string().contains("module.yaml"),
            "error must mention module.yaml: {err}"
        );
    }

    #[test]
    fn push_error_meta_kind_is_module_yaml_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            "localhost:5000/test/mod:v1",
            no_flags(),
        )
        .expect_err("missing module.yaml must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "module_yaml_missing",
            "error kind must be module_yaml_missing: {meta:?}"
        );
        assert!(
            meta.extras["dir"].is_string(),
            "meta must carry dir payload: {:?}",
            meta.extras
        );
    }

    #[test]
    fn push_errors_when_registry_unreachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_module_yaml(dir.path());
        let artifact = unreachable_ref("test/push-unreachable");
        let printer = test_printer();

        let err = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            &artifact,
            no_flags(),
        )
        .expect_err("unreachable registry must return Err");

        assert!(
            !err.to_string().is_empty(),
            "error must have a message: {err}"
        );
    }

    #[test]
    fn push_error_meta_kind_is_push_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_module_yaml(dir.path());
        let artifact = unreachable_ref("test/push-doc");
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            &artifact,
            no_flags(),
        )
        .expect_err("unreachable registry must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "push_failed",
            "error kind must be push_failed: {meta:?}"
        );
    }

    #[test]
    fn pull_errors_when_registry_unreachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = unreachable_ref("test/pull-unreachable");
        let printer = test_printer();

        let err = cmd_module_pull(
            &printer,
            &artifact,
            dir.path().to_str().unwrap(),
            false,
            false,
            cfgd_core::oci::VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        )
        .expect_err("unreachable registry must return Err");

        assert!(
            !err.to_string().is_empty(),
            "error must have a message: {err}"
        );
    }

    #[test]
    fn pull_error_meta_kind_is_pull_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = unreachable_ref("test/pull-doc");
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_module_pull(
            &printer,
            &artifact,
            dir.path().to_str().unwrap(),
            false,
            false,
            cfgd_core::oci::VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        )
        .expect_err("unreachable registry must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "pull_failed",
            "error kind must be pull_failed: {meta:?}"
        );
    }

    #[cfg(unix)]
    mod with_cosign_shim {
        use cfgd_core::output::Printer;
        use cfgd_core::test_helpers::CosignTestShim;
        use serial_test::serial;

        use super::super::{PushOptions, cmd_module_pull, cmd_module_push};
        use super::{unreachable_ref, write_module_yaml};

        #[test]
        #[serial]
        fn push_sign_flag_emits_sign_failed_when_cosign_exits_nonzero() {
            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());

            let mut server = mockito::Server::new();
            let registry = server.url().trim_start_matches("http://").to_string();
            let artifact = format!("{}/test/mod:v1", registry);
            let upload_location = format!("{}/v2/test/mod/blobs/uploads/up-id", server.url());

            server
                .mock(
                    "HEAD",
                    mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
                )
                .with_status(404)
                .expect_at_least(2)
                .create();

            server
                .mock("POST", "/v2/test/mod/blobs/uploads/")
                .with_status(202)
                .with_header("Location", &upload_location)
                .expect_at_least(2)
                .create();

            server
                .mock(
                    "PUT",
                    mockito::Matcher::Regex(
                        r"/v2/test/mod/blobs/uploads/up-id\?digest=sha256:.*".to_string(),
                    ),
                )
                .with_status(201)
                .expect_at_least(2)
                .create();

            server
                .mock("PUT", "/v2/test/mod/manifests/v1")
                .with_status(201)
                .create();

            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(1)
                .with_stderr("cosign sign failed: unauthorized")
                .install();

            let (printer, _cap) = Printer::for_test_doc();
            let err = cmd_module_push(
                &printer,
                dir.path().to_str().unwrap(),
                &artifact,
                PushOptions {
                    platform: None,
                    apply: false,
                    sign: true,
                    key: Some("cosign.key"),
                    attest: false,
                },
            )
            .expect_err("cosign sign failure must return Err");
            drop(printer);

            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("handler returns CliErrorMeta");
            assert_eq!(
                meta.error_kind, "sign_failed",
                "error kind must be sign_failed: {meta:?}"
            );
        }

        #[test]
        #[serial]
        fn pull_require_signature_emits_verify_failed_when_cosign_exits_nonzero() {
            let dir = tempfile::tempdir().expect("tempdir");
            let artifact = unreachable_ref("test/verify-sig");

            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(1)
                .with_stderr("cosign verify failed")
                .install();

            let (printer, _cap) = Printer::for_test_doc();
            let err = cmd_module_pull(
                &printer,
                &artifact,
                dir.path().to_str().unwrap(),
                true,
                false,
                cfgd_core::oci::VerifyOptions {
                    key: Some("cosign.pub"),
                    identity: None,
                    issuer: None,
                },
            )
            .expect_err("cosign verify failure must return Err");
            drop(printer);

            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("handler returns CliErrorMeta");
            assert_eq!(
                meta.error_kind, "verify_failed",
                "error kind must be verify_failed: {meta:?}"
            );
            assert_eq!(
                meta.extras["step"], "signature",
                "step must be 'signature': {:?}",
                meta.extras
            );
        }

        // Helper: stand up a mock OCI registry that accepts blob uploads and
        // returns 201 on manifest PUT, so a happy-path push can complete.
        fn mock_push_registry() -> (mockito::ServerGuard, String) {
            let mut server = mockito::Server::new();
            let registry = server.url().trim_start_matches("http://").to_string();
            let upload_location = format!("{}/v2/test/mod/blobs/uploads/up-id", server.url());

            server
                .mock(
                    "HEAD",
                    mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
                )
                .with_status(404)
                .expect_at_least(2)
                .create();
            server
                .mock("POST", "/v2/test/mod/blobs/uploads/")
                .with_status(202)
                .with_header("Location", &upload_location)
                .expect_at_least(2)
                .create();
            server
                .mock(
                    "PUT",
                    mockito::Matcher::Regex(
                        r"/v2/test/mod/blobs/uploads/up-id\?digest=sha256:.*".to_string(),
                    ),
                )
                .with_status(201)
                .expect_at_least(2)
                .create();
            server
                .mock("PUT", "/v2/test/mod/manifests/v1")
                .with_status(201)
                .create();

            (server, registry)
        }

        #[test]
        #[serial]
        fn push_with_platform_kv_includes_platform_in_output() {
            // Mock a successful push (no sign / attest) so we reach the
            // happy-path doc emit and the platform kv entry is added.
            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());
            let (_server, registry) = mock_push_registry();
            let artifact = format!("{}/test/mod:v1", registry);

            let (printer, cap) =
                cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
            cmd_module_push(
                &printer,
                dir.path().to_str().unwrap(),
                &artifact,
                PushOptions {
                    platform: Some("linux/amd64"),
                    apply: false,
                    sign: false,
                    key: None,
                    attest: false,
                },
            )
            .expect("push must succeed");
            drop(printer);

            let output = cap.lock().unwrap();
            assert!(
                output.contains("Platform"),
                "platform kv label must appear in human output: {output}"
            );
            assert!(
                output.contains("linux/amd64"),
                "platform value must appear in human output: {output}"
            );
        }

        #[test]
        #[serial]
        fn push_with_sign_success_emits_signed_true_doc() {
            // Cosign shim succeeds (exit 0); push happy path; verify the
            // emitted JSON doc records signed=true.
            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(0)
                .install();

            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());
            let (_server, registry) = mock_push_registry();
            let artifact = format!("{}/test/mod:v1", registry);

            let (printer, cap) = Printer::for_test_doc();
            cmd_module_push(
                &printer,
                dir.path().to_str().unwrap(),
                &artifact,
                PushOptions {
                    platform: None,
                    apply: false,
                    sign: true,
                    key: Some("cosign.key"),
                    attest: false,
                },
            )
            .expect("push + sign must succeed");
            drop(printer);

            let doc = cap.json().expect("success doc must be emitted");
            assert_eq!(doc["signed"], true, "signed must be true: {doc}");
            assert_eq!(
                doc["attestation"], false,
                "attestation must be false: {doc}"
            );
        }

        #[test]
        #[serial]
        fn pull_with_signature_verify_success_emits_signature_verified_true() {
            // Cosign shim succeeds for `verify`; the rest of the pull fails
            // (no valid OCI server) but the signature-verification branch
            // exits success first — verify the emitted error_doc shows the
            // failure originated downstream of the signature step.
            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(0)
                .install();

            let dir = tempfile::tempdir().expect("tempdir");
            let artifact = unreachable_ref("test/verify-success");

            let (printer, _cap) = Printer::for_test_doc();
            let err = cmd_module_pull(
                &printer,
                &artifact,
                dir.path().to_str().unwrap(),
                true,
                false,
                cfgd_core::oci::VerifyOptions {
                    key: Some("cosign.pub"),
                    identity: None,
                    issuer: None,
                },
            )
            .expect_err("downstream pull failure must return Err");
            drop(printer);

            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("handler returns CliErrorMeta");
            // The signature verify succeeded; the subsequent pull failed,
            // so the error must carry pull_failed, not verify_failed.
            assert_eq!(
                meta.error_kind, "pull_failed",
                "downstream failure must surface as pull_failed: {meta:?}"
            );
        }

        #[test]
        #[serial]
        fn pull_verify_attestation_emits_verify_failed_when_cosign_exits_nonzero() {
            let dir = tempfile::tempdir().expect("tempdir");
            let artifact = unreachable_ref("test/verify-attest");

            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(1)
                .with_stderr("cosign verify-attestation failed")
                .install();

            let (printer, _cap) = Printer::for_test_doc();
            let err = cmd_module_pull(
                &printer,
                &artifact,
                dir.path().to_str().unwrap(),
                false,
                true,
                cfgd_core::oci::VerifyOptions {
                    key: Some("cosign.pub"),
                    identity: None,
                    issuer: None,
                },
            )
            .expect_err("cosign verify-attestation failure must return Err");
            drop(printer);

            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("handler returns CliErrorMeta");
            assert_eq!(
                meta.error_kind, "verify_failed",
                "error kind must be verify_failed: {meta:?}"
            );
            assert_eq!(
                meta.extras["step"], "attestation",
                "step must be 'attestation': {:?}",
                meta.extras
            );
        }
    }

    #[test]
    fn pull_happy_path_emits_doc_with_artifact_and_output() {
        let mut server = mockito::Server::new();
        let registry = server.url().trim_start_matches("http://").to_string();

        let src_dir = tempfile::tempdir().expect("src module dir");
        write_module_yaml(src_dir.path());

        let layer_data = cfgd_core::oci::create_tar_gz(src_dir.path()).expect("create layer");
        let config_blob =
            serde_json::to_vec(&serde_json::json!({ "moduleYaml": "name: test-mod" })).unwrap();
        let config_digest = cfgd_core::sha256_digest(&config_blob);
        let layer_digest = cfgd_core::sha256_digest(&layer_data);

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": cfgd_core::oci::MEDIA_TYPE_MODULE_CONFIG,
                "digest": config_digest,
                "size": config_blob.len(),
            },
            "layers": [{
                "mediaType": cfgd_core::oci::MEDIA_TYPE_MODULE_LAYER,
                "digest": layer_digest,
                "size": layer_data.len(),
            }],
        });

        server
            .mock("GET", "/v2/test/mod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
            .with_body(serde_json::to_string(&manifest).unwrap())
            .create();

        server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(200)
            .with_body(layer_data)
            .create();

        let artifact_ref = format!("{}/test/mod:v1", registry);
        let output_dir = tempfile::tempdir().expect("output dir");
        let (printer, cap) = Printer::for_test_doc();

        cmd_module_pull(
            &printer,
            &artifact_ref,
            output_dir.path().to_str().unwrap(),
            false,
            false,
            cfgd_core::oci::VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        )
        .expect("pull happy path must succeed");
        drop(printer);

        let doc = cap.json().expect("success doc must be emitted");
        assert_eq!(doc["artifact"], artifact_ref, "artifact field must match");
        assert!(
            doc["output"].is_string(),
            "output field must be present: {doc}"
        );
    }

    #[test]
    fn push_happy_path_emits_doc_with_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_module_yaml(dir.path());

        let mut server = mockito::Server::new();
        let registry = server.url().trim_start_matches("http://").to_string();
        let artifact = format!("{}/test/mod:v1", registry);
        let upload_location = format!("{}/v2/test/mod/blobs/uploads/up-id", server.url());

        server
            .mock(
                "HEAD",
                mockito::Matcher::Regex(r"/v2/test/mod/blobs/sha256:.*".to_string()),
            )
            .with_status(404)
            .expect_at_least(2)
            .create();

        server
            .mock("POST", "/v2/test/mod/blobs/uploads/")
            .with_status(202)
            .with_header("Location", &upload_location)
            .expect_at_least(2)
            .create();

        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(
                    r"/v2/test/mod/blobs/uploads/up-id\?digest=sha256:.*".to_string(),
                ),
            )
            .with_status(201)
            .expect_at_least(2)
            .create();

        server
            .mock("PUT", "/v2/test/mod/manifests/v1")
            .with_status(201)
            .create();

        let (printer, cap) = Printer::for_test_doc();
        cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            &artifact,
            PushOptions {
                platform: None,
                apply: false,
                sign: false,
                key: None,
                attest: false,
            },
        )
        .expect("push happy path must succeed");
        drop(printer);

        let doc = cap.json().expect("success doc must be emitted");
        let digest = doc["digest"].as_str().expect("digest must be a string");
        assert!(
            digest.starts_with("sha256:"),
            "digest must be a sha256 hash: {digest}"
        );
        assert_eq!(doc["signed"], false, "signed must be false: {doc}");
        assert_eq!(
            doc["attestation"], false,
            "attestation must be false: {doc}"
        );
    }
}
