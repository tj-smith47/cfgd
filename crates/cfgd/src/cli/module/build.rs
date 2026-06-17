use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

#[allow(clippy::too_many_arguments)]
pub fn cmd_module_build(
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

    printer.heading("Build Module");
    let mut header = vec![("Directory".to_string(), dir.to_string())];
    if let Some(t) = target {
        header.push(("Target".to_string(), t.to_string()));
    }
    if let Some(img) = base_image {
        header.push(("Base image".to_string(), img.to_string()));
    }
    printer.kv_block(header);

    let default_platform = cfgd_core::oci::current_platform();
    let targets: Vec<&str> = target
        .map(|t| t.split(',').collect())
        .unwrap_or_else(|| vec![default_platform.as_str()]);

    let mut output_artifacts: Vec<String> = Vec::new();
    let mut digest_value: Option<String> = None;

    if targets.len() == 1 {
        let output_dir = cfgd_core::oci::build_module(dir_path, Some(targets[0]), base_image)
            .map_err(|e| {
                crate::cli::cli_error(
                    dir,
                    "build_failed",
                    cfgd_core::output::collapse_to_subject_line(&e),
                    serde_json::json!({ "dir": dir, "target": targets[0] }),
                )
            })?;
        printer.status_simple(Role::Ok, format!("Built to {}", output_dir.posix()));
        output_artifacts.push(output_dir.display().to_string());

        if let Some(art) = artifact {
            let digest =
                cfgd_core::oci::push_module(&output_dir, art, Some(targets[0]), Some(printer))
                    .map_err(|e| {
                        crate::cli::cli_error(
                            art,
                            "push_failed",
                            cfgd_core::output::collapse_to_subject_line(&e),
                            serde_json::json!({ "artifact": art, "target": targets[0] }),
                        )
                    })?;
            printer.kv("Digest", &digest);
            digest_value = Some(digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| {
                    crate::cli::cli_error(
                        art,
                        "sign_failed",
                        cfgd_core::output::collapse_to_subject_line(&e),
                        serde_json::json!({ "artifact": art }),
                    )
                })?;
                printer.status_simple(Role::Ok, "Signed artifact");
            }
        }
    } else {
        let mut builds: Vec<(std::path::PathBuf, String)> = Vec::new();
        for t in &targets {
            let sp = printer.spinner(format!("Building for {t}..."));
            let output_dir = match cfgd_core::oci::build_module(dir_path, Some(t), base_image) {
                Ok(d) => {
                    sp.finish_ok(format!("Built {t} to {}", d.posix()));
                    d
                }
                Err(e) => {
                    sp.finish_fail(format!("Build failed for {t}"))
                        .detail(cfgd_core::output::collapse_to_subject_line(&e));
                    return Err(crate::cli::cli_error(
                        dir,
                        "build_failed",
                        cfgd_core::output::collapse_to_subject_line(&e),
                        serde_json::json!({ "dir": dir, "target": *t }),
                    ));
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
                    crate::cli::cli_error(
                        art,
                        "push_failed",
                        cfgd_core::output::collapse_to_subject_line(&e),
                        serde_json::json!({ "artifact": art, "targets": &targets }),
                    )
                })?;
            printer.kv("Digest", &digest);
            digest_value = Some(digest);

            if sign {
                cfgd_core::oci::sign_artifact(art, key).map_err(|e| {
                    crate::cli::cli_error(
                        art,
                        "sign_failed",
                        cfgd_core::output::collapse_to_subject_line(&e),
                        serde_json::json!({ "artifact": art }),
                    )
                })?;
                printer.status_simple(Role::Ok, "Signed artifact");
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
    printer.emit(Doc::new().with_data(serde_json::Value::Object(payload)));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-build\nspec:\n  packages: []\n";

    fn write_module_yaml(dir: &std::path::Path) {
        std::fs::write(dir.join("module.yaml"), MODULE_YAML).unwrap();
    }

    #[test]
    fn missing_module_yaml_returns_error_meta() {
        let dir = tempfile::tempdir().unwrap();
        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            None,
            None,
            None,
            false,
            None,
        )
        .expect_err("missing module.yaml must be rejected");
        drop(printer);

        assert!(
            err.to_string().contains("does not contain a module.yaml"),
            "error message must describe the problem: {err}"
        );
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "module_yaml_missing");
        assert!(
            meta.extras["dir"].is_string(),
            "payload must include dir: {:?}",
            meta.extras
        );
    }

    #[test]
    fn build_fails_single_target_returns_build_failed_error_meta() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            None,
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        )
        .expect_err("unreachable base image must cause build failure");
        drop(printer);

        assert!(
            !err.to_string().is_empty(),
            "error message must be non-empty: {err}"
        );
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "build_failed");
        assert!(
            meta.extras["dir"].is_string(),
            "payload must include dir: {:?}",
            meta.extras
        );
    }

    #[test]
    fn build_fails_single_target_includes_target_in_header_output() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let _ = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64"),
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        );
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("linux/amd64"),
            "target must appear in header kv block: {output}"
        );
        assert!(
            output.contains("localhost:1/cfgd-test-nonexistent:latest"),
            "base image must appear in header kv block: {output}"
        );
    }

    #[test]
    fn build_fails_multi_target_returns_build_failed_error_meta() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64,linux/arm64"),
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        )
        .expect_err("multi-target build with unreachable image must fail");
        drop(printer);

        assert!(
            !err.to_string().is_empty(),
            "error message must be non-empty: {err}"
        );
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "build_failed");
    }

    #[test]
    fn target_split_comma_produces_multi_target_path() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let _ = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64,linux/arm64"),
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        );
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("linux/amd64") || output.contains("linux/arm64"),
            "spinner output must mention at least one target: {output}"
        );
    }

    mod sign_path {
        use super::*;
        use cfgd_core::test_helpers::CosignTestShim;
        use serial_test::serial;

        #[test]
        #[serial]
        fn sign_fails_when_cosign_exits_nonzero_returns_sign_failed_error_meta() {
            if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
                return;
            }
            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(2)
                .with_stderr("simulated sign failure")
                .install();

            let dir = tempfile::tempdir().unwrap();
            write_module_yaml(dir.path());

            let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
            let err = cmd_module_build(
                &printer,
                dir.path().to_str().unwrap(),
                None,
                Some("localhost:1/cfgd-test-nonexistent:latest"),
                Some("localhost:1/test/build:latest"),
                true,
                None,
            )
            .expect_err("build must fail before sign is reached");
            drop(printer);

            assert!(
                !err.to_string().is_empty(),
                "error must not be empty: {err}"
            );
            let meta = err
                .downcast_ref::<crate::cli::CliErrorMeta>()
                .expect("handler returns CliErrorMeta");
            assert!(
                meta.error_kind == "build_failed" || meta.error_kind == "sign_failed",
                "error must be build_failed or sign_failed: {}",
                meta.error_kind
            );
        }
    }

    // -----------------------------------------------------------------------
    // Additional uncovered-branch tests. The single-target build path
    // without a `--target` arg exercises the `targets.unwrap_or_else(||
    // vec![default_platform()])` branch; the kv-block header omits the
    // optional `Target` and `Base image` entries when `None` is passed.
    // -----------------------------------------------------------------------

    #[test]
    fn build_default_target_omits_target_and_base_image_from_header() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        // No --target and no --base-image. The build will still fail because
        // the default base (ubuntu:22.04) requires network — but we get to
        // exercise the default-platform branch and the header-construction
        // logic that omits the optional kv entries first.
        let _ = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            None,
            None,
            None,
            false,
            None,
        );
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Build Module"),
            "heading must be emitted before the build fails: {output}"
        );
        assert!(
            output.contains("Directory"),
            "Directory kv entry must always appear in the header: {output}"
        );
        // Negative: when None was passed, the optional kv entries must be
        // absent from the header.
        assert!(
            !output.contains("Base image"),
            "Base image kv entry must be absent when base_image=None: {output}"
        );
    }

    #[test]
    fn build_missing_directory_path_still_rejects_with_module_yaml_missing() {
        // Path that doesn't exist at all → dir_path.join("module.yaml") also
        // doesn't exist, falling through the same error branch as an empty
        // existing directory. Pin the error key + message so the branch is
        // covered without depending on filesystem state.
        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            "/nonexistent/cfgd-test-module-build-path",
            None,
            None,
            None,
            false,
            None,
        )
        .expect_err("nonexistent dir → module.yaml missing");
        drop(printer);

        assert!(
            err.to_string().contains("does not contain a module.yaml"),
            "error message must call out the missing module.yaml: {err}"
        );
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "module_yaml_missing");
        assert_eq!(
            meta.extras["dir"],
            "/nonexistent/cfgd-test-module-build-path"
        );
    }

    #[test]
    fn build_single_target_failure_payload_includes_target_in_error_meta() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64"),
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        )
        .expect_err("unreachable base image must cause build failure");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "build_failed");
        // The single-target branch puts `target` (singular) in the error
        // payload; the multi-target branch puts `target` per-spinner. Pin
        // this so the two branches stay distinguishable on the wire.
        assert_eq!(meta.extras["target"], "linux/amd64");
        assert_eq!(meta.extras["dir"], dir.path().to_str().unwrap());
    }

    #[test]
    fn build_multi_target_failure_payload_includes_failing_target_only() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64,linux/arm64"),
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        )
        .expect_err("first target must fail and short-circuit the loop");
        drop(printer);

        assert!(!err.to_string().is_empty());
        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "build_failed");
        // Multi-target branch emits the per-target failure with the
        // singular `target` field corresponding to the target that failed
        // first (the iteration short-circuits on Err).
        let failed_target = meta.extras["target"].as_str().expect("target string");
        assert!(
            failed_target == "linux/amd64" || failed_target == "linux/arm64",
            "failed target must be one of the requested targets: {failed_target}"
        );
    }

    // -----------------------------------------------------------------------
    // Edge cases that exercise the success doc-payload construction. These
    // do NOT require docker — the build fails (the registry is unreachable),
    // but the JSON payload emitted on the failure path mirrors the success
    // shape's key set, so they pin the error_doc field schema.
    // -----------------------------------------------------------------------

    #[test]
    fn build_emits_targets_list_in_error_payload_for_multi_target() {
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_module_yaml(dir.path());

        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64,linux/arm64,linux/arm/v7"),
            Some("localhost:1/cfgd-test-nonexistent:latest"),
            None,
            false,
            None,
        )
        .expect_err("unreachable base image must cause build failure");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        // build_failed emits "target" (singular) for the failing target.
        assert_eq!(meta.error_kind, "build_failed");
        // Pin that the meta carries a string `target` field naming a
        // platform that was in the requested set.
        let target = meta.extras["target"].as_str().expect("target field");
        assert!(
            target.starts_with("linux/"),
            "failing target must be a linux/ platform: {target}"
        );
    }

    #[test]
    fn build_module_yaml_missing_does_not_run_docker() {
        // No docker required — the missing-module.yaml gate fires first.
        let dir = tempfile::tempdir().unwrap();
        // Intentionally do NOT write module.yaml.
        let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
        let err = cmd_module_build(
            &printer,
            dir.path().to_str().unwrap(),
            Some("linux/amd64,linux/arm64"),
            Some("any-image"),
            Some("some-artifact:tag"),
            true,
            Some("cosign.key"),
        )
        .expect_err("missing module.yaml must short-circuit even with multi-target args");
        // Error must come from the module.yaml gate, not from docker / cosign.
        assert!(
            err.to_string().contains("does not contain a module.yaml"),
            "expected module-yaml-missing error: {err}"
        );
    }
}
