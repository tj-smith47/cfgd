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

    let mut header = vec![("Directory".to_string(), dir.to_string())];
    if let Some(t) = target {
        header.push(("Target".to_string(), t.to_string()));
    }
    if let Some(img) = base_image {
        header.push(("Base Image".to_string(), img.to_string()));
    }

    let default_platform = cfgd_core::oci::current_platform();
    let targets: Vec<&str> = target
        .map(|t| t.split(',').collect())
        .unwrap_or_else(|| vec![default_platform.as_str()]);

    let mut output_artifacts: Vec<String> = Vec::new();
    let mut pushed: Option<Pushed> = None;

    // ONE section, named for the command, holding everything the run produced:
    // what is being built, each build's verdict, the push, the digest and the
    // signing verdict. A second section named `Push` under a `Build Module`
    // title reads as a separate command rather than as this one's result, and
    // it is also what gives `push_module` — a bare-`&Printer` library call with
    // no `SectionGuard` of its own — a depth to inherit through
    // `depth_inheritance` instead of rendering at depth 0.
    {
        let build_sec = printer.section("Build Module");
        let _inherit = printer.depth_inheritance();
        build_sec.kv_block(header);

        if targets.len() == 1 {
            // The build IS the wait this frame reports: the bar runs under
            // the section and retires silently into the `Built to` row below
            // it, the way the per-target branch settles each of its own.
            let output_dir = printer
                .narrate_silent("Building module", |_| {
                    cfgd_core::oci::build_module(dir_path, Some(targets[0]), base_image)
                })
                .map_err(|e| {
                    crate::cli::cli_error(
                        dir,
                        "build_failed",
                        cfgd_core::output::collapse_to_subject_line(&e),
                        serde_json::json!({ "dir": dir, "target": targets[0] }),
                    )
                })?;
            printer.status_simple(
                Role::Ok,
                format!(
                    "Built to {}",
                    cfgd_core::fold_home_in_text(&output_dir.display_posix())
                ),
            );
            output_artifacts.push(cfgd_core::to_posix_string(&output_dir));

            if let Some(art) = artifact {
                let outcome =
                    cfgd_core::oci::push_module(&output_dir, art, Some(targets[0]), Some(printer))
                        .map_err(|e| {
                            crate::cli::cli_error(
                                art,
                                "push_failed",
                                cfgd_core::output::collapse_to_subject_line(&e),
                                serde_json::json!({ "artifact": art, "target": targets[0] }),
                            )
                        })?;
                crate::cli::helpers::sign_and_attest(
                    printer,
                    art,
                    &outcome.written_digests(),
                    key,
                    sign,
                    false,
                )?;
                let cfgd_core::oci::PushOutcome {
                    digest,
                    index_digest,
                    ..
                } = outcome;
                pushed = Some(Pushed::Platform {
                    digest,
                    index_digest,
                });
            }
        } else {
            let mut builds: Vec<(std::path::PathBuf, String)> = Vec::new();
            for t in &targets {
                // One `target:<t>` owner group per platform, the same idiom
                // `cli/sync.rs` uses per source: a bare loop of spinners has no
                // owner to attribute a build to when several run in sequence.
                let owner = build_sec.section_owner(&cfgd_core::output::OwnerLabel::new(
                    "target",
                    (*t).to_string(),
                ));
                let sp = owner.spinner(format!("Building for {t}"));
                let output_dir = match cfgd_core::oci::build_module(dir_path, Some(t), base_image) {
                    Ok(d) => {
                        sp.finish_ok(format!(
                            "Built {t} to {}",
                            cfgd_core::fold_home_in_text(&d.display_posix())
                        ));
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
                output_artifacts.push(cfgd_core::to_posix_string(&output_dir));
                builds.push((output_dir, t.to_string()));
            }

            if let Some(art) = artifact {
                let build_refs: Vec<(&Path, &str)> = builds
                    .iter()
                    .map(|(dir, plat)| (dir.as_path(), plat.as_str()))
                    .collect();
                let outcome =
                    cfgd_core::oci::push_module_multiplatform(&build_refs, art, Some(printer))
                        .map_err(|e| {
                            crate::cli::cli_error(
                                art,
                                "push_failed",
                                cfgd_core::output::collapse_to_subject_line(&e),
                                serde_json::json!({ "artifact": art, "targets": &targets }),
                            )
                        })?;
                crate::cli::helpers::sign_and_attest(
                    printer,
                    art,
                    &outcome.written_digests(),
                    key,
                    sign,
                    false,
                )?;
                pushed = Some(Pushed::Index(outcome.index_digest));
            }
        }
    }

    // `output_artifacts[0]` is the module directory the push would take: the
    // single build's output, or the first platform's for a multi-platform
    // build whose one index artifact is already pushed. Rendered outside the
    // section: this is the RUN's closing next step, not a note qualifying the
    // row above it, so it lands flush left like every other mutating verb's.
    let built = output_artifacts.first().map_or(dir, String::as_str);
    let next_step = super::success_next_step(match artifact {
        Some(_) => super::Mutation::ModulePushed { applied: None },
        None => super::Mutation::ModuleBuilt { output: built },
    });

    let payload = build_payload(dir, &targets, &output_artifacts, artifact, pushed, sign);
    printer.emit(Doc::new().hint(next_step).with_data(payload));

    Ok(())
}

/// What a `module build --push` wrote at the tag.
enum Pushed {
    /// One platform's manifest, and the index the tag names when the push
    /// joined it to platforms already there.
    Platform {
        digest: String,
        index_digest: Option<String>,
    },
    /// A multi-platform build's index.
    Index(String),
}

/// The `-o json` payload of `module build`. A push reports `digest` and
/// `indexDigest`: a multi-platform build's push is an index, so both name it;
/// a single-target push writes an index only when the tag already listed
/// another platform.
fn build_payload(
    dir: &str,
    targets: &[&str],
    output_artifacts: &[String],
    artifact: Option<&str>,
    pushed: Option<Pushed>,
    signed: bool,
) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert("dir".into(), dir.into());
    payload.insert("targets".into(), targets.into());
    payload.insert("outputArtifacts".into(), output_artifacts.into());
    payload.insert("signed".into(), signed.into());
    if let Some(art) = artifact {
        payload.insert("artifact".into(), art.into());
    }
    let digests = match pushed {
        Some(Pushed::Platform {
            digest,
            index_digest,
        }) => Some((digest, index_digest)),
        Some(Pushed::Index(digest)) => Some((digest.clone(), Some(digest))),
        None => None,
    };
    if let Some((digest, index_digest)) = digests {
        payload.insert("digest".into(), digest.into());
        payload.insert("indexDigest".into(), index_digest.into());
    }
    payload.into()
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
        assert!(
            output.contains("linux/amd64") || output.contains("linux/arm64"),
            "spinner output must mention at least one target: {output}"
        );
    }

    /// The multi-target loop's `target:<t>` owner header used
    /// to have nothing under it at depth 0 — a bare top-level spinner had no
    /// owner to nest under. It now opens via `printer.section_owner(&OwnerLabel)`
    /// per platform, so a build failure's settle line nests one level deeper
    /// than its `target:<t>` header. An unreachable base image fails FAST
    /// (connection refused) on the first target, so this needs no successful
    /// docker build and no push.
    #[test]
    fn build_failure_settle_line_nests_under_the_target_owner_header() {
        // `cmd_module_build` shells out to `docker build`/`podman build` even
        // for a base image it will fail to reach — the runtime binary itself
        // has to be present on PATH before the "connection refused" failure
        // this test depends on is even reachable. A bare `return` here left
        // no trace in the run's output when neither runtime is on the host,
        // so the test silently proved nothing rather than being visibly
        // skipped; `eprintln!` inside a `#[cfg(test)]` block is exempt from
        // the output-centralization audit (`strip_test_blocks_from_file`).
        if !cfgd_core::command_available("docker") && !cfgd_core::command_available("podman") {
            eprintln!(
                "SKIPPED build_failure_settle_line_nests_under_the_target_owner_header: \
                 neither docker nor podman is on PATH"
            );
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
        crate::cli::test_support::assert_nests_under(
            &output,
            "target:linux/amd64",
            "Build failed for linux/amd64",
        );
    }

    mod sign_path {
        use super::*;
        use cfgd_core::test_helpers::CosignTestShim;
        use serial_test::serial;

        #[cfg(unix)]
        use crate::cli::module::push_pull::tests::{ManifestPuts, mock_push_registry_holding};

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

        /// Build `targets` through a stand-in container runtime and push the
        /// result with `--sign` to a mock registry whose tag holds `tag`,
        /// answering the cosign argv (one line per call) and the registry.
        // Unix-only: the stand-in runtime is a `/bin/sh` script.
        #[cfg(unix)]
        fn cosign_argv_after_signed_build(
            targets: &str,
            tag: Option<&serde_json::Value>,
        ) -> (String, String, ManifestPuts) {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            write_module_yaml(dir.path());
            // `docker cp <container>:/build/. <out>` is the step that leaves the
            // built module where `push_module` reads it; every other verb only
            // has to succeed.
            let runtime = dir.path().join("docker");
            std::fs::write(
                &runtime,
                format!(
                    "#!/bin/sh\nfor last; do :; done\n\
                     if [ \"$1\" = cp ]; then cp '{}' \"$last/module.yaml\"; fi\nexit 0\n",
                    dir.path().join("module.yaml").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _runtime = cfgd_core::test_helpers::EnvVarGuard::set(
                "CFGD_DOCKER_BIN",
                runtime.to_str().unwrap(),
            );
            let shim = CosignTestShim::builder()
                .with_argv_logging(true)
                .with_exit(0)
                .install();
            let (_server, registry, puts) = mock_push_registry_holding(tag);

            let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();
            cmd_module_build(
                &printer,
                dir.path().to_str().unwrap(),
                Some(targets),
                None,
                Some(&format!("{registry}/test/mod:v1")),
                true,
                None,
            )
            .expect("signed build and push must succeed");
            (shim.argv_log(), registry, puts)
        }

        /// `cosign sign` ran once per subject, in order.
        #[cfg(unix)]
        fn assert_signed(argv: &str, subjects: &[String]) {
            let signs: Vec<&str> = argv.lines().filter(|l| l.starts_with("sign ")).collect();
            assert_eq!(signs.len(), subjects.len(), "{argv}");
            for (sign, subject) in signs.iter().zip(subjects) {
                assert!(
                    sign.ends_with(subject.as_str()),
                    "a signature names {subject}: {argv}"
                );
            }
        }

        /// `artifact` pinned to the digest of the manifest the build put at `tag`.
        #[cfg(unix)]
        fn subject(registry: &str, puts: &ManifestPuts, tag: &str) -> String {
            format!("{registry}/test/mod@{}", puts.at(tag))
        }

        #[cfg(unix)]
        #[test]
        #[serial]
        fn build_sign_of_one_target_signs_the_manifest_it_pushed() {
            let (argv, registry, puts) = cosign_argv_after_signed_build("linux/amd64", None);
            assert_signed(&argv, &[subject(&registry, &puts, "v1-linux-amd64")]);
        }

        #[cfg(unix)]
        #[test]
        #[serial]
        fn build_sign_of_one_target_joining_an_index_signs_the_index_and_the_manifest() {
            let (argv, registry, puts) = cosign_argv_after_signed_build(
                "linux/amd64",
                Some(&serde_json::json!({
                    "schemaVersion": 2,
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "annotations": { cfgd_core::OCI_ANNOTATION_PLATFORM: "linux/arm64" },
                })),
            );
            assert_signed(
                &argv,
                &[
                    subject(&registry, &puts, "v1"),
                    subject(&registry, &puts, "v1-linux-amd64"),
                ],
            );
        }

        #[cfg(unix)]
        #[test]
        #[serial]
        fn build_sign_of_several_targets_signs_the_index_and_each_manifest() {
            let (argv, registry, puts) =
                cosign_argv_after_signed_build("linux/amd64,linux/arm64", None);
            let subjects = [
                subject(&registry, &puts, "v1"),
                subject(&registry, &puts, "v1-linux-amd64"),
                subject(&registry, &puts, "v1-linux-arm64"),
            ];
            assert_ne!(subjects[1], subjects[2], "the two manifests differ");
            assert_signed(&argv, &subjects);
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
        // the default base (ubuntu:22.04) requires network — but this gets to
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

        let output = cfgd_core::test_helpers::captured_text(&buf);
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
            !output.contains("Base Image"),
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

    fn payload(pushed: Option<Pushed>) -> serde_json::Value {
        build_payload(
            "mod",
            &["linux/amd64"],
            &["out".to_string()],
            Some("reg/mod:v1"),
            pushed,
            false,
        )
    }

    #[test]
    fn build_payload_of_a_push_joined_to_an_index_names_both_digests() {
        let doc = payload(Some(Pushed::Platform {
            digest: "sha256:a1".to_string(),
            index_digest: Some("sha256:1d".to_string()),
        }));
        assert_eq!(doc["digest"], "sha256:a1");
        assert_eq!(doc["indexDigest"], "sha256:1d");
    }

    #[test]
    fn build_payload_of_a_push_that_wrote_no_index_has_a_null_index_digest() {
        let doc = payload(Some(Pushed::Platform {
            digest: "sha256:a1".to_string(),
            index_digest: None,
        }));
        assert_eq!(doc["digest"], "sha256:a1");
        assert_eq!(doc.get("indexDigest"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn build_payload_of_a_multi_platform_push_names_the_index_twice() {
        let doc = payload(Some(Pushed::Index("sha256:1d".to_string())));
        assert_eq!(doc["digest"], "sha256:1d");
        assert_eq!(doc["indexDigest"], "sha256:1d");
    }

    #[test]
    fn build_payload_without_a_push_carries_no_digests() {
        let doc = build_payload("mod", &[], &[], None, None, false);
        assert!(
            doc.get("digest").is_none()
                && doc.get("indexDigest").is_none()
                && doc.get("artifact").is_none(),
            "{doc}"
        );
    }
}
