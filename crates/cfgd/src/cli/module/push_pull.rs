use super::*;
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
        printer.emit(cfgd_core::output::error_doc(
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
            printer.emit(cfgd_core::output::error_doc(
                artifact,
                "push_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact, "dir": dir, "platform": platform }),
            ));
            anyhow::anyhow!("{e}")
        })?;

    printer.kv("Digest", &digest);

    if sign {
        cfgd_core::oci::sign_artifact(artifact, key).map_err(|e| {
            printer.emit(cfgd_core::output::error_doc(
                artifact,
                "sign_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact }),
            ));
            anyhow::anyhow!("{e}")
        })?;
        printer.status_simple(Role::Ok, "Signed artifact with cosign");
    }

    let mut attestation_attached = false;

    if attest {
        let repo = detect_git_remote().unwrap_or_else(|| "unknown".to_string());
        let commit = detect_git_head().unwrap_or_else(|| "unknown".to_string());

        let provenance = cfgd_core::oci::generate_slsa_provenance(
            artifact, &digest, &repo, &commit,
        )
        .map_err(|e| {
            printer.emit(cfgd_core::output::error_doc(
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
                printer.emit(cfgd_core::output::error_doc(
                    artifact,
                    "attest_failed",
                    e.to_string(),
                    serde_json::json!({ "artifact": artifact, "step": "attach" }),
                ));
                anyhow::anyhow!("{e}")
            })?;
        printer.status_simple(Role::Ok, "Attached SLSA provenance attestation");
        attestation_attached = true;
    }

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
    printer: &Printer,
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
) -> anyhow::Result<()> {
    use kube::Client;
    use kube::api::{Api, Patch, PatchParams};

    let name = &module_doc.metadata.name;
    let client = Client::try_default().await.map_err(|e| {
        printer.emit(cfgd_core::output::error_doc(
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
            printer.emit(cfgd_core::output::error_doc(
                name,
                "crd_apply_failed",
                format!("Failed to apply Module CRD: {e}"),
                serde_json::json!({ "artifact": artifact }),
            ));
            anyhow::anyhow!("Failed to apply Module CRD: {e}")
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
            printer.emit(cfgd_core::output::error_doc(
                artifact_ref,
                "verify_failed",
                e.to_string(),
                serde_json::json!({ "artifact": artifact_ref, "step": "signature" }),
            ));
            anyhow::anyhow!("{e}")
        })?;
        printer.status_simple(Role::Ok, "Signature verified");
    }

    if verify_attestation {
        cfgd_core::oci::verify_attestation(artifact_ref, "slsaprovenance", &verify_opts).map_err(
            |e| {
                printer.emit(cfgd_core::output::error_doc(
                    artifact_ref,
                    "verify_failed",
                    e.to_string(),
                    serde_json::json!({ "artifact": artifact_ref, "step": "attestation" }),
                ));
                anyhow::anyhow!("{e}")
            },
        )?;
        printer.status_simple(Role::Ok, "SLSA provenance attestation verified");
    }

    cfgd_core::oci::pull_module(artifact_ref, output_path, false, Some(printer)).map_err(|e| {
        printer.emit(cfgd_core::output::error_doc(
            artifact_ref,
            "pull_failed",
            e.to_string(),
            serde_json::json!({ "artifact": artifact_ref, "output": output }),
        ));
        anyhow::anyhow!("{e}")
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
    fn push_error_doc_kind_is_module_yaml_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (printer, cap) = Printer::for_test_doc();

        let _ = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            "localhost:5000/test/mod:v1",
            no_flags(),
        );
        drop(printer);

        let doc = cap.json().expect("error_doc must be emitted");
        assert_eq!(
            doc["error"], "module_yaml_missing",
            "emitted doc error must be module_yaml_missing: {doc}"
        );
        assert!(
            doc["dir"].is_string(),
            "emitted doc must carry dir payload: {doc}"
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
    fn push_error_doc_kind_is_push_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_module_yaml(dir.path());
        let artifact = unreachable_ref("test/push-doc");
        let (printer, cap) = Printer::for_test_doc();

        let _ = cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            &artifact,
            no_flags(),
        );
        drop(printer);

        let doc = cap.json().expect("error_doc must be emitted");
        assert_eq!(
            doc["error"], "push_failed",
            "emitted doc error must be push_failed: {doc}"
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
    fn pull_error_doc_kind_is_pull_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = unreachable_ref("test/pull-doc");
        let (printer, cap) = Printer::for_test_doc();

        let _ = cmd_module_pull(
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
        );
        drop(printer);

        let doc = cap.json().expect("error_doc must be emitted");
        assert_eq!(
            doc["error"], "pull_failed",
            "emitted doc error must be pull_failed: {doc}"
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

            let (printer, cap) = Printer::for_test_doc();
            let _ = cmd_module_push(
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
            );
            drop(printer);

            let doc = cap.json().expect("error_doc must be emitted");
            assert_eq!(
                doc["error"], "sign_failed",
                "emitted doc error must be sign_failed: {doc}"
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

            let (printer, cap) = Printer::for_test_doc();
            let _ = cmd_module_pull(
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
            );
            drop(printer);

            let doc = cap.json().expect("error_doc must be emitted");
            assert_eq!(
                doc["error"], "verify_failed",
                "emitted doc error must be verify_failed: {doc}"
            );
            assert_eq!(doc["step"], "signature", "step must be 'signature': {doc}");
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

            let (printer, cap) = Printer::for_test_doc();
            let _ = cmd_module_pull(
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
            );
            drop(printer);

            let doc = cap.json().expect("error_doc must be emitted");
            assert_eq!(
                doc["error"], "verify_failed",
                "emitted doc error must be verify_failed: {doc}"
            );
            assert_eq!(
                doc["step"], "attestation",
                "step must be 'attestation': {doc}"
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

    mod git_helpers {
        use cfgd_core::test_helpers::CwdGuard;
        use serial_test::serial;

        use super::super::{detect_git_head_for_tests, detect_git_remote_for_tests};

        fn git(dir: &std::path::Path, args: &[&str]) {
            let status = cfgd_core::git_cmd_local()
                .args(args)
                .current_dir(dir)
                .status()
                .expect("git command");
            assert!(status.success(), "git {:?} failed", args);
        }

        #[test]
        #[serial]
        fn detect_git_remote_returns_none_in_fresh_repo() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            let result = detect_git_remote_for_tests();
            assert!(
                result.is_none(),
                "fresh repo with no remote must return None, got: {result:?}"
            );
        }

        #[test]
        #[serial]
        fn detect_git_head_returns_some_after_initial_commit() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            git(dir.path(), &["config", "user.email", "test@example.com"]);
            git(dir.path(), &["config", "user.name", "Test"]);
            std::fs::write(dir.path().join("f.txt"), b"hello").expect("write file");
            git(dir.path(), &["add", "."]);
            git(dir.path(), &["commit", "-m", "init"]);

            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            let result = detect_git_head_for_tests();

            let sha = result.expect("HEAD must be Some after initial commit");
            assert_eq!(sha.len(), 40, "HEAD SHA must be 40 hex chars: {sha}");
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit()),
                "HEAD SHA must be hex: {sha}"
            );
        }

        #[test]
        #[serial]
        fn detect_git_head_returns_none_in_empty_repo() {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init"]);
            let _cwd = CwdGuard::set(dir.path()).expect("cwd guard");
            let result = detect_git_head_for_tests();
            assert!(
                result.is_none(),
                "empty repo has no HEAD, must return None: {result:?}"
            );
        }
    }
}
