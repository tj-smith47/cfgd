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

    // No `Platform` row: `resolve_platform` is the flag or this host, so
    // whenever a row keyed on the flag would render, its value IS the string
    // the settled row's detail already carries — the same fact twice in one
    // five-line block. The detail states the resolved platform whether or not
    // `--platform` was given, so the block reads the same either way.
    let header = vec![
        ("Directory".to_string(), dir.to_string()),
        ("Artifact".to_string(), artifact.to_string()),
    ];

    // ONE section, named for the command, holding everything the run produced:
    // what is being pushed, the push verdict (carrying the digest as its
    // detail), the signing verdict and the CRD apply. A second section named `Push` under a `Push
    // Module` title spends the word twice on one screen for two different things.
    // `push_module` keeps its `&Printer` signature (it has non-CLI callers
    // too), so the section is opened and scoped here rather than threaded into
    // the library call, and `depth_inheritance` is what settles its spinner at
    // the section's depth instead of depth 0.
    let mut applied_name: Option<String> = None;
    let (digest, resolved_platform, index_digest, signed, attestation_attached) = {
        // heading-first-ok: push_module is handed this printer and narrates its
        // bar at the section's depth, so the frame reports its own wait
        let push_sec = printer.section("Push Module");
        let _inherit = printer.depth_inheritance();
        push_sec.kv_block(header);
        let cfgd_core::oci::PushOutcome {
            digest,
            platform: resolved_platform,
            index_digest,
        } = cfgd_core::oci::push_module(dir_path, artifact, platform, Some(printer)).map_err(
            |e| {
                crate::cli::cli_error(
                    artifact,
                    "push_failed",
                    e.to_string(),
                    // No `platform`: the key names the platform an artifact
                    // WAS pushed for, and this branch pushed none. Echoing the
                    // flag here answers `null` for the defaulted case, which
                    // reads as "no platform" rather than "no artifact".
                    serde_json::json!({ "artifact": artifact, "dir": dir }),
                )
            },
        )?;
        let resolved = index_digest.as_deref().unwrap_or(&digest);
        let crate::cli::helpers::SignAttestOutcome { signed, attested } =
            crate::cli::helpers::sign_and_attest(printer, artifact, resolved, key, sign, attest)?;

        if apply {
            let module_yaml = std::fs::read_to_string(dir_path.join("module.yaml"))?;
            let module_doc = cfgd_core::config::parse_module(&module_yaml).map_err(|e| {
                crate::cli::cli_error(
                    artifact,
                    "parse_failed",
                    format!("Failed to parse module.yaml: {e}"),
                    serde_json::json!({ "artifact": artifact, "dir": dir }),
                )
            })?;

            let signature = build_module_signature(printer, signed, key);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(apply_module_crd(printer, &module_doc, artifact, signature))?;
            applied_name = Some(module_doc.metadata.name.clone());
        }
        (digest, resolved_platform, index_digest, signed, attested)
    };

    // The RESOLVED platform, never the `--platform` flag: this push stamped it
    // into the manifest, and the operator reads it back into a Module's
    // `PLATFORMS` column, so a payload key naming the artifact's platform
    // answering `null` for a defaulted one is a wrong value, not a silence.
    // Outside the section on purpose: this is the RUN's closing next step, not
    // a note qualifying the row above it, so it lands flush left like every
    // other mutating verb's.
    printer.emit(
        Doc::new()
            .hint(super::success_next_step(super::Mutation::ModulePushed {
                applied: applied_name.as_deref(),
            }))
            .with_data(serde_json::json!({
                "dir": dir,
                "artifact": artifact,
                "platform": resolved_platform,
                "digest": digest,
                "indexDigest": index_digest,
                "signed": signed,
                "attestation": attestation_attached,
                "applied": applied_name,
            })),
    );

    Ok(())
}

/// Derive the CRD-facing signature configuration threaded from `--sign`.
///
/// Keyless signing (`--sign` without `--key`) maps directly to
/// `cosign.keyless = true`. Key-based signing carries no in-memory signature
/// payload to thread — `sign_and_attest` only echoes whether signing was
/// requested, since the actual cosign signature lives in the OCI
/// registry/Rekor, not in process memory — so the verification-facing public
/// key is read from the `cosign.pub` sibling file next to the private key
/// path, the convention `cfgd module keys generate`/`keys rotate` establish.
/// A `--key` naming a KMS URI (`k8s://`, `awskms://`, `azurekms://`,
/// `gcpkms://`, `hashivault://`, …) or a PKCS#11 URI (`pkcs11:token=...;...`,
/// RFC 7512 — HSM-backed keys) has no filesystem sibling to read, so that case
/// is detected up front rather than guessed at via a nonsense path. When no
/// public key can be derived, the caller is warned that the applied CRD will
/// fail the operator's `disallowUnsigned` admission check.
fn build_module_signature(
    printer: &Printer,
    signed: bool,
    key: Option<&str>,
) -> Option<cfgd_crd::ModuleSignature> {
    if !signed {
        return None;
    }

    let unsigned_cosign = || cfgd_crd::CosignSignature {
        public_key: None,
        keyless: false,
        certificate_identity: None,
        certificate_oidc_issuer: None,
    };

    // PKCS#11 URIs (RFC 7512) use a single-colon `pkcs11:` scheme with no
    // `//` authority, so they don't match the `://` KMS-URI check below —
    // they need their own prefix check to avoid being mistaken for a path.
    let is_non_filesystem_key_ref =
        |key_ref: &str| key_ref.contains("://") || key_ref.starts_with("pkcs11:");

    let cosign = match key {
        None => cfgd_crd::CosignSignature {
            public_key: None,
            keyless: true,
            certificate_identity: None,
            certificate_oidc_issuer: None,
        },
        // A KMS or PKCS#11 reference names no file to read a sibling
        // `cosign.pub` beside, but the run that just signed with it holds
        // cosign and the reference, which is everything cosign needs to hand
        // back the public half.
        Some(key_ref) if is_non_filesystem_key_ref(key_ref) => {
            match cfgd_core::oci::public_key_of(key_ref) {
                Ok(public_key) => cfgd_crd::CosignSignature {
                    public_key: Some(public_key),
                    keyless: false,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                },
                Err(e) => {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Could not read the public key of '{key_ref}' from cosign: \
                             {e}; the applied CRD will fail the disallowUnsigned admission \
                             check until spec.signature.cosign.publicKey is set"
                        ),
                    );
                    unsigned_cosign()
                }
            }
        }
        Some(key_path) => {
            let pub_key_path = Path::new(key_path).with_file_name("cosign.pub");
            match std::fs::read_to_string(&pub_key_path) {
                Ok(public_key) => cfgd_crd::CosignSignature {
                    public_key: Some(public_key),
                    keyless: false,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                },
                Err(_) => {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "No sibling public key found at '{}'; the applied CRD will fail the disallowUnsigned admission check",
                            cfgd_core::fold_home_in_text(&pub_key_path.display_posix())
                        ),
                    );
                    unsigned_cosign()
                }
            }
        }
    };

    Some(cfgd_crd::ModuleSignature {
        cosign: Some(cosign),
    })
}

pub(super) fn build_module_crd_json(
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
    signature: Option<cfgd_crd::ModuleSignature>,
) -> anyhow::Result<serde_json::Value> {
    let cfgd_core::config::ModuleSpec {
        depends,
        platforms,
        packages,
        files,
        env,
        aliases,
        scripts: scripts_spec,
        system,
    } = &module_doc.spec;

    let packages: Vec<cfgd_crd::PackageEntry> = packages
        .iter()
        .map(|entry| {
            let cfgd_core::config::ModulePackageEntry {
                name,
                min_version,
                prefer,
                deny,
                aliases,
                platforms,
                // The four script-install knobs steer a `prefer: [script]`
                // install on a machine the agent is reconciling — a shell body
                // and the guards that decide whether to run it. Nothing
                // cluster-side executes a package install, so carrying them
                // onto the CRD would publish a shell payload no reader of the
                // resource can run.
                script: _script,   // no CRD counterpart
                only_if: _only_if, // no CRD counterpart
                unless: _unless,   // no CRD counterpart
                creates: _creates, // no CRD counterpart
            } = entry;
            cfgd_crd::PackageEntry {
                name: name.clone(),
                aliases: aliases
                    .iter()
                    .map(|(manager, override_name)| (manager.clone(), override_name.clone()))
                    .collect(),
                min_version: min_version.clone(),
                prefer: prefer.clone(),
                deny: deny.clone(),
                platforms: platforms.clone(),
            }
        })
        .collect();

    let files: Vec<cfgd_crd::ModuleFileSpec> = files
        .iter()
        .map(|entry| {
            let cfgd_core::config::ModuleFileEntry {
                source,
                target,
                strategy,
                private,
                encryption,
                permissions,
                patch,
            } = entry;
            cfgd_crd::ModuleFileSpec {
                source: source.clone(),
                target: target.clone(),
                strategy: *strategy,
                private: *private,
                encryption: encryption.clone(),
                permissions: permissions.clone(),
                patch: patch.clone(),
            }
        })
        .collect();

    let env: Vec<cfgd_crd::ModuleEnvVar> = env
        .iter()
        .map(|entry| {
            let cfgd_core::config::EnvVar {
                name,
                value,
                platforms,
            } = entry;
            cfgd_crd::ModuleEnvVar {
                name: name.clone(),
                value: value.clone(),
                append: false, // the local EnvVar carries no append concept
                // Carried so a module round-trips through the registry
                // unchanged, and so the pod-mutating webhook can honour the
                // gate instead of injecting an entry the module gated off.
                platforms: platforms.clone(),
            }
        })
        .collect();

    let aliases: Vec<cfgd_crd::ModuleAlias> = aliases
        .iter()
        .map(|entry| {
            let cfgd_core::config::ShellAlias {
                name,
                command,
                platforms,
            } = entry;
            cfgd_crd::ModuleAlias {
                name: name.clone(),
                command: command.clone(),
                platforms: platforms.clone(),
            }
        })
        .collect();

    // `SystemSettings` is a YAML value map and the CRD field a JSON one; the
    // configurator settings themselves are plain scalars, lists and maps, so
    // the re-serialization is total.
    let system = system
        .iter()
        .map(|(configurator, settings)| Ok((configurator.clone(), serde_json::to_value(settings)?)))
        .collect::<Result<std::collections::BTreeMap<_, _>, serde_json::Error>>()?;

    // The CRD's `scripts.postApply` is a relative script PATH the operator's
    // mutating webhook joins verbatim into an init container command
    // (`sh -c "/cfgd-modules/{name}/{postApply}"`), and a module directory
    // declares no such path. The local hook set is a different thing entirely —
    // inline bodies the cfgd AGENT runs on a machine — so it travels as
    // `spec.hooks`, its own field, rather than being flattened into a path the
    // webhook would run as an unquoted shell command with every guard discarded.
    let scripts = cfgd_crd::ModuleScripts { post_apply: None };

    let spec = cfgd_crd::ModuleSpec {
        packages,
        files,
        scripts,
        env,
        depends: depends.clone(),
        oci_artifact: Some(artifact.to_string()),
        signature,
        mount_policy: cfgd_crd::MountPolicy::default(),
        platforms: platforms.clone(),
        aliases,
        system,
        hooks: scripts_spec.clone(),
    };

    let mut spec_json = serde_json::to_value(&spec)?;
    // `mountPolicy` is cluster-side state: the operator's mutating webhook is
    // the only writer that ever sets it to `Debug` (a ConfigPolicy/
    // ClusterConfigPolicy debug-module entry), and `apply_module_crd` applies
    // with server-side apply and no `force`. Serializing the struct's default
    // `Always` here would make every `push --apply` claim ownership of the
    // field, either SSA-conflicting with the webhook's field manager or
    // silently reverting an operator-set `Debug` back to `Always`. The CLI has
    // no basis to assert this field at all, so it is stripped from the
    // emitted object post-serialization rather than omitted from the struct
    // literal (which would defeat the compiler's total-field-coverage check).
    if let Some(obj) = spec_json.as_object_mut() {
        obj.remove("mountPolicy");
    }
    Ok(serde_json::json!({
        "apiVersion": cfgd_core::API_VERSION,
        "kind": "Module",
        "metadata": {
            "name": &module_doc.metadata.name,
        },
        "spec": spec_json,
    }))
}

async fn apply_module_crd(
    printer: &Printer,
    module_doc: &cfgd_core::config::ModuleDocument,
    artifact: &str,
    signature: Option<cfgd_crd::ModuleSignature>,
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

    let module_json = build_module_crd_json(module_doc, artifact, signature)?;

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
    platform: Option<&str>,
    require_signature: bool,
    verify_attestation: bool,
    verify_opts: cfgd_core::oci::VerifyOptions<'_>,
) -> anyhow::Result<()> {
    let output_path = Path::new(output);

    let mut module_name: Option<String> = None;
    let mut module_description: Option<String> = None;
    let mut package_count: Option<usize> = None;
    let mut file_count: Option<usize> = None;
    let pulled: cfgd_core::oci::PullOutcome;

    // Same shape as `cmd_module_push`: ONE section named for the command,
    // holding what is being pulled, the verifications the pull gated on, the
    // pull verdict and what the pulled module turned out to contain.
    // `pull_module` keeps its `&Printer` signature (it has non-CLI callers
    // too), so the section is opened and scoped here rather than threaded into
    // the library call, and `depth_inheritance` settles its spinner at the
    // section's depth instead of depth 0.
    {
        let pull_sec = printer.section("Pull Module");
        let _inherit = printer.depth_inheritance();
        pull_sec.kv_block([("Artifact", artifact_ref), ("Output", output)]);

        // The checks run inside the pull, against the digest of the one tag
        // read it extracts from, so a tag moved mid-pull cannot slip an
        // unverified document past them.
        let checks = cfgd_core::oci::PullChecks {
            signature: match (require_signature, verify_opts.key) {
                (false, _) => cfgd_core::oci::SignaturePolicy::None,
                (true, Some(path)) => cfgd_core::oci::SignaturePolicy::RequireKey { path },
                (true, None) => cfgd_core::oci::SignaturePolicy::RequireKeyless {
                    identity: verify_opts.identity,
                    issuer: verify_opts.issuer,
                },
            },
            attestation: verify_attestation.then_some(("slsaprovenance1", verify_opts)),
        };
        pulled =
            cfgd_core::oci::pull_module(artifact_ref, output_path, checks, platform, Some(printer))
                .map_err(|e| {
                    let step = match &e {
                        cfgd_core::errors::OciError::VerificationFailed { .. } => "signature",
                        cfgd_core::errors::OciError::AttestationError { .. } => "attestation",
                        _ => {
                            return crate::cli::cli_error(
                                artifact_ref,
                                "pull_failed",
                                e.to_string(),
                                serde_json::json!({ "artifact": artifact_ref, "output": output }),
                            );
                        }
                    };
                    crate::cli::cli_error(
                        artifact_ref,
                        "verify_failed",
                        e.to_string(),
                        serde_json::json!({ "artifact": artifact_ref, "step": step }),
                    )
                })?;
        if require_signature {
            printer.status_simple(Role::Ok, "Verified signature");
        }
        if verify_attestation {
            printer.status_simple(Role::Ok, "Verified SLSA provenance attestation");
        }

        if output_path.join("module.yaml").exists() {
            let contents = std::fs::read_to_string(output_path.join("module.yaml"))?;
            if let Ok(doc) = cfgd_core::config::parse_module(&contents) {
                let mut pairs = vec![("Module".to_string(), doc.metadata.name.clone())];
                if let Some(desc) = &doc.metadata.description {
                    pairs.push(("Description".to_string(), desc.clone()));
                }
                pairs.push(("Packages".to_string(), doc.spec.packages.len().to_string()));
                pairs.push(("Files".to_string(), doc.spec.files.len().to_string()));
                pull_sec.kv_block(pairs);
                module_name = Some(doc.metadata.name.clone());
                module_description = doc.metadata.description.clone();
                package_count = Some(doc.spec.packages.len());
                file_count = Some(doc.spec.files.len());
            }
        }
    }

    // The run's closing next step, at the run's own depth — see `cmd_module_push`.
    printer.emit(
        Doc::new()
            .hint(super::success_next_step(super::Mutation::ModulePulled {
                name: module_name.as_deref(),
            }))
            .with_data(serde_json::json!({
                "artifact": artifact_ref,
                "output": output,
                "digest": pulled.digest,
                "indexDigest": pulled.index_digest,
                "signatureVerified": require_signature,
                "attestationVerified": verify_attestation,
                "moduleName": module_name,
                "moduleDescription": module_description,
                "packageCount": package_count,
                "fileCount": file_count,
            })),
    );

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
            None,
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
            None,
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

    /// A mock registry whose `test/mod:v1` names a manifest whose layer blob
    /// is missing: a pull reads the tag, runs its checks against it and then
    /// fails fetching the layer. Answers the artifact and the manifest digest.
    fn mock_tag_without_blob() -> (mockito::ServerGuard, String, String) {
        let mut server = mockito::Server::new();
        let artifact = format!("{}/test/mod:v1", server.url().trim_start_matches("http://"));
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "layers": [{
                "mediaType": cfgd_core::oci::MEDIA_TYPE_MODULE_LAYER,
                "digest": format!("sha256:{}", "0".repeat(64)),
                "size": 1,
            }],
        })
        .to_string();
        let digest = cfgd_core::sha256_digest(manifest.as_bytes());
        server
            .mock("GET", "/v2/test/mod/manifests/v1")
            .with_status(200)
            .with_body(manifest)
            .expect(1)
            .create();
        (server, artifact, digest)
    }

    /// Digest the mock registry answers for a manifest put at a platform tag.
    const PLATFORM_TAG_DIGEST: &str = "sha256:3a1";
    /// Digest the mock registry answers for whatever is put at `v1`.
    const TAG_DIGEST: &str = "sha256:1d3";

    /// A mock registry taking a push of `test/mod:v1` while the tag is absent.
    fn mock_push_registry() -> (mockito::ServerGuard, String) {
        mock_push_registry_holding(None)
    }

    /// A mock registry taking a push of `test/mod:v1`, whose tag holds `tag`
    /// before the push (absent when `None`). The two manifest PUTs answer with
    /// different digests so a payload or cosign argv shows which one it names.
    fn mock_push_registry_holding(
        tag: Option<&serde_json::Value>,
    ) -> (mockito::ServerGuard, String) {
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
        let tag_read = server.mock("GET", "/v2/test/mod/manifests/v1");
        match tag {
            Some(body) => tag_read.with_status(200).with_body(body.to_string()),
            None => tag_read.with_status(404),
        }
        .create();
        server
            .mock(
                "PUT",
                mockito::Matcher::Regex(r"^/v2/test/mod/manifests/v1-".to_string()),
            )
            .with_status(201)
            .with_header("Docker-Content-Digest", PLATFORM_TAG_DIGEST)
            .create();
        server
            .mock("PUT", "/v2/test/mod/manifests/v1")
            .with_status(201)
            .with_header("Docker-Content-Digest", TAG_DIGEST)
            .create();
        (server, registry)
    }

    mod with_cosign_shim {
        use cfgd_core::output::Printer;
        use cfgd_core::test_helpers::CosignTestShim;
        use serial_test::serial;

        use super::super::{PushOptions, cmd_module_pull, cmd_module_push};
        use super::{
            PLATFORM_TAG_DIGEST, TAG_DIGEST, mock_push_registry, mock_push_registry_holding,
            mock_tag_without_blob, write_module_yaml,
        };

        #[test]
        #[serial]
        fn push_sign_flag_emits_sign_failed_when_cosign_exits_nonzero() {
            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());

            let (_server, registry) = mock_push_registry();
            let artifact = format!("{}/test/mod:v1", registry);

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
            let (_server, artifact, _) = mock_tag_without_blob();

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
                None,
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
        /// `cmd_module_push`'s push spinner used to render at
        /// depth 0 unconditionally (a bare `printer.spinner()` call inside
        /// `push_module`, a library fn with no `SectionGuard` of its own).
        /// It now runs inside the command's one `printer.section("Push
        /// Module")` plus `depth_inheritance()`, so the settled line nests one
        /// level deeper than the section header instead of sitting flush with
        /// it.
        #[test]
        #[serial]
        fn push_settle_line_nests_under_the_push_section_header() {
            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());
            let (_server, registry) = mock_push_registry();
            let artifact = format!("{}/test/mod:v1", registry);

            let (printer, buf) =
                cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
            cmd_module_push(
                &printer,
                dir.path().to_str().unwrap(),
                &artifact,
                super::super::PushOptions {
                    platform: None,
                    apply: false,
                    sign: false,
                    key: None,
                    attest: false,
                },
            )
            .expect("push must succeed");
            drop(printer);

            let output = cfgd_core::test_helpers::captured_text(&buf);
            crate::cli::test_support::assert_nests_under(&output, "Push Module", "Pushed module");
        }

        /// A `--platform` push states the platform ONCE, in the settled row's
        /// detail. The conditional `Platform` header row spelled the same
        /// value a second time in the same five-line block, and the two could
        /// never differ: `resolve_platform` is the flag or this host, so
        /// whenever the row rendered its value WAS the parenthetical.
        #[test]
        #[serial]
        fn push_states_a_named_platform_once_in_the_settled_row() {
            // Mock a successful push (no sign / attest) to reach the
            // happy-path doc emit and the settled row's detail.
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
                !output.contains("Platform"),
                "a `Platform` header row restates the settled row's detail: {output}"
            );
            assert_eq!(
                output.matches("linux/amd64").count(),
                1,
                "the resolved platform is stated once, in the row that produced it: {output}"
            );
        }

        /// The platform a push RESOLVED is the artifact's whether or not a
        /// flag named it: the settled row carries it beside the digest, and
        /// the payload's `platform` key holds it. Filled from the `Option`
        /// flag, that key answered `null` for an artifact whose manifest — and
        /// whose Module `PLATFORMS` column, read back off that manifest by the
        /// operator — said `linux/amd64`.
        #[test]
        #[serial]
        fn push_without_a_platform_flag_reports_the_host_platform_in_both_renders() {
            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());
            let (_server, registry) = mock_push_registry();
            let artifact = format!("{}/test/mod:v1", registry);

            let (printer, cap) = Printer::for_test_doc();
            cmd_module_push(
                &printer,
                dir.path().to_str().unwrap(),
                &artifact,
                super::no_flags(),
            )
            .expect("push must succeed");
            drop(printer);

            let host = cfgd_core::oci::current_platform();
            let human = cap.human();
            assert!(
                human.contains(&host),
                "the pushed row reports the platform it resolved ({host}): {human}"
            );
            let doc = cap.json().expect("success doc must be emitted");
            assert_eq!(
                doc["platform"],
                serde_json::Value::String(host),
                "the payload carries the resolved platform, never the flag: {doc}"
            );
        }

        /// Push with `--sign --attest` to a tag holding `tag`, answering the
        /// cosign argv (one line per call) and the registry.
        fn cosign_argv_after_push(tag: Option<&serde_json::Value>) -> (String, String) {
            let shim = CosignTestShim::builder()
                .with_argv_logging(true)
                .with_exit(0)
                .install();
            let dir = tempfile::tempdir().expect("tempdir");
            write_module_yaml(dir.path());
            let (_server, registry) = mock_push_registry_holding(tag);
            let (printer, _cap) = Printer::for_test_doc();
            cmd_module_push(
                &printer,
                dir.path().to_str().unwrap(),
                &format!("{registry}/test/mod:v1"),
                PushOptions {
                    platform: Some("linux/arm64"),
                    apply: false,
                    sign: true,
                    key: None,
                    attest: true,
                },
            )
            .expect("signed and attested push must succeed");
            (shim.argv_log(), registry)
        }

        #[test]
        #[serial]
        fn push_signs_and_attests_the_index_the_tag_resolves_to_after_a_join() {
            let (argv, registry) = cosign_argv_after_push(Some(&serde_json::json!({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "annotations": { cfgd_core::OCI_ANNOTATION_PLATFORM: "linux/amd64" },
            })));
            assert_signed_and_attested(&argv, &format!("{registry}/test/mod@{TAG_DIGEST}"));
        }

        #[test]
        #[serial]
        fn push_signs_and_attests_the_manifest_when_no_index_was_written() {
            let (argv, registry) = cosign_argv_after_push(None);
            assert_signed_and_attested(
                &argv,
                &format!("{registry}/test/mod@{PLATFORM_TAG_DIGEST}"),
            );
        }

        /// `cosign sign` and `cosign attest` each ran once, both naming `subject`.
        fn assert_signed_and_attested(argv: &str, subject: &str) {
            let calls: Vec<&str> = argv
                .lines()
                .filter(|l| l.starts_with("sign ") || l.starts_with("attest "))
                .collect();
            assert_eq!(calls.len(), 2, "{argv}");
            assert!(
                calls[0].starts_with("sign ") && calls[0].ends_with(subject),
                "the signature names {subject}: {argv}"
            );
            assert!(
                calls[1].starts_with("attest ") && calls[1].ends_with(subject),
                "the attestation names {subject}: {argv}"
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
            let (_server, artifact, _) = mock_tag_without_blob();

            let (printer, _cap) = Printer::for_test_doc();
            let err = cmd_module_pull(
                &printer,
                &artifact,
                dir.path().to_str().unwrap(),
                None,
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
        fn pull_checks_signature_and_attestation_against_the_digest_the_tag_read_answered() {
            let shim = CosignTestShim::builder()
                .with_argv_logging(true)
                .with_exit(0)
                .install();
            let dir = tempfile::tempdir().expect("tempdir");
            let (_server, artifact, digest) = mock_tag_without_blob();

            let (printer, _cap) = Printer::for_test_doc();
            let _ = cmd_module_pull(
                &printer,
                &artifact,
                dir.path().to_str().unwrap(),
                None,
                true,
                true,
                cfgd_core::oci::VerifyOptions {
                    key: Some("cosign.pub"),
                    identity: None,
                    issuer: None,
                },
            );
            drop(printer);

            let subject = format!("{}@{digest}", artifact.trim_end_matches(":v1"));
            let argv = shim.argv_log();
            let calls: Vec<&str> = argv.lines().collect();
            assert_eq!(
                calls.len(),
                2,
                "one signature and one attestation check: {argv}"
            );
            assert!(
                calls[0].starts_with("verify ") && calls[0].ends_with(&subject),
                "{argv}"
            );
            assert!(
                calls[1].starts_with("verify-attestation ") && calls[1].ends_with(&subject),
                "{argv}"
            );
        }

        #[test]
        #[serial]
        fn pull_verify_attestation_emits_verify_failed_when_cosign_exits_nonzero() {
            let dir = tempfile::tempdir().expect("tempdir");
            let (_server, artifact, _) = mock_tag_without_blob();

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
                None,
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

    mod typed_crd_construction {
        use cfgd_core::config::parse_module;
        use cfgd_core::output::{Printer, Verbosity};
        use cfgd_crd::check_unsigned_policy;

        use super::super::{build_module_crd_json, build_module_signature};

        const MINIMAL_MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n";

        /// Every field of `ModuleSpec`, `ModulePackageEntry` and
        /// `ModuleFileEntry` set to a distinguishable value — the fixture the
        /// round-trip guard below prices, so a local field that stops reaching
        /// the CRD changes this document's rendering rather than passing
        /// unnoticed.
        const FULL_MODULE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: full-mod
spec:
  depends:
    - base
  platforms:
    - linux
  packages:
    - name: sed
      minVersion: "4.8"
      prefer:
        - brew
        - apt
      aliases:
        brew: gnu-sed
        apt: sed
      script: curl -fsSL https://example.invalid/sed.sh | sh
      onlyIf: command -v curl
      unless: command -v gsed
      creates: ~/.local/bin/gsed
      deny:
        - snap
      platforms:
        - linux
  files:
    - source: files/init.lua
      target: ~/.config/nvim/init.lua
      strategy: Copy
      private: true
      encryption:
        backend: sops
        mode: Always
      permissions: "600"
    - target: ~/.gitconfig
      strategy: Patch
      patch:
        format: Ini
        ensure:
          user:
            name: Ada
  env:
    - name: FOO
      value: bar
    - name: MAC_ONLY
      value: yes
      platforms:
        - macos
  aliases:
    - name: ll
      command: ls -la
    - name: mac-ls
      command: ls -G
      platforms:
        - macos
  scripts:
    preApply:
      - echo pre
    postApply:
      - echo one
      - echo two
    preReconcile:
      - run: echo pre-reconcile
        timeout: 30s
    postReconcile:
      - echo post-reconcile
    onDrift:
      - echo drift
    onChange:
      - echo change
  system:
    shell:
      defaultShell: zsh
"#;

        fn crd_spec(crd_json: &serde_json::Value) -> cfgd_crd::ModuleSpec {
            serde_json::from_value(crd_json["spec"].clone())
                .expect("crd spec must deserialize into cfgd_crd::ModuleSpec")
        }

        #[test]
        fn unsigned_crd_is_rejected_by_disallow_unsigned_admission() {
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let crd_json = build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", None)
                .expect("build crd json");
            let spec = crd_spec(&crd_json);

            let result = check_unsigned_policy(&spec, true);

            assert!(
                result.is_err(),
                "an unsigned module must fail the real disallowUnsigned admission rule"
            );
        }

        #[test]
        fn sign_apply_keyless_crd_satisfies_disallow_unsigned_admission() {
            let printer = Printer::for_test().0;
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let signature = build_module_signature(&printer, true, None);
            let crd_json =
                build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", signature)
                    .expect("build crd json");
            let spec = crd_spec(&crd_json);

            let result = check_unsigned_policy(&spec, true);

            assert!(
                result.is_ok(),
                "keyless --sign must satisfy the real disallowUnsigned admission rule: {result:?}"
            );
        }

        #[test]
        fn sign_apply_with_key_and_sibling_pub_satisfies_disallow_unsigned_admission() {
            let dir = tempfile::tempdir().expect("tempdir");
            let key_path = dir.path().join("cosign.key");
            std::fs::write(&key_path, "fake-private-key").expect("write key");
            std::fs::write(dir.path().join("cosign.pub"), "fake-public-key-pem")
                .expect("write pub");

            let printer = Printer::for_test().0;
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let signature =
                build_module_signature(&printer, true, Some(key_path.to_str().expect("utf8 path")));
            let crd_json =
                build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", signature)
                    .expect("build crd json");
            let spec = crd_spec(&crd_json);

            let result = check_unsigned_policy(&spec, true);

            assert!(
                result.is_ok(),
                "key-based --sign with a sibling cosign.pub must satisfy the real disallowUnsigned admission rule: {result:?}"
            );
        }

        #[test]
        fn sign_with_key_and_no_sibling_pub_still_fails_disallow_unsigned_admission() {
            let dir = tempfile::tempdir().expect("tempdir");
            let key_path = dir.path().join("cosign.key");
            std::fs::write(&key_path, "fake-private-key").expect("write key");

            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let signature =
                build_module_signature(&printer, true, Some(key_path.to_str().expect("utf8 path")));
            let crd_json =
                build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", signature)
                    .expect("build crd json");
            let spec = crd_spec(&crd_json);

            let result = check_unsigned_policy(&spec, true);

            assert!(
                result.is_err(),
                "key-based --sign with no sibling cosign.pub must still fail the real disallowUnsigned admission rule"
            );
            let warning = cfgd_core::test_helpers::captured_text(&buf);
            assert!(
                warning.contains("No sibling public key found"),
                "missing sibling key must be surfaced to the user: {warning:?}"
            );
        }

        /// A KMS key has no sibling `cosign.pub`, and the run signing with it
        /// already holds cosign, so the public half is read from cosign
        /// rather than left to the reader.
        #[test]
        #[serial_test::serial]
        fn sign_with_kms_key_reference_reads_the_public_key_from_cosign() {
            const PEM: &str = "-----BEGIN PUBLIC KEY-----\nMFk=\n-----END PUBLIC KEY-----";
            let shim =
                cfgd_core::test_helpers::ToolShim::install(cfgd_core::COSIGN_BIN_ENV, 0, PEM, "");
            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let signature =
                build_module_signature(&printer, true, Some("awskms://alias/cfgd-signing-key"));
            let crd_json =
                build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", signature)
                    .expect("build crd json");
            let spec = crd_spec(&crd_json);

            let argv = shim.argv_log();
            assert!(
                argv.lines()
                    .any(|l| l == "public-key --key awskms://alias/cfgd-signing-key"),
                "the reference cfgd signed with is the one it asks the public half of: {argv}"
            );
            assert_eq!(
                crd_json["spec"]["signature"]["cosign"]["publicKey"], PEM,
                "and the answer is recorded on the module cfgd applies"
            );
            let result = check_unsigned_policy(&spec, true);
            assert!(
                result.is_ok(),
                "a module carrying its public key passes the real disallowUnsigned \
                 admission rule: {result:?}"
            );
            let output = cfgd_core::test_helpers::captured_text(&buf);
            assert!(
                !output.contains("disallowUnsigned"),
                "and nothing warns about a key that was derived: {output:?}"
            );
        }

        /// The same reference when cosign cannot answer: the module goes out
        /// unsigned, and the warning says which step failed.
        #[test]
        #[serial_test::serial]
        fn sign_with_pkcs11_key_cosign_cannot_read_warns_and_fails_disallow_unsigned_admission() {
            let _shim = cfgd_core::test_helpers::ToolShim::install(
                cfgd_core::COSIGN_BIN_ENV,
                1,
                "",
                "no such token",
            );
            let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let signature = build_module_signature(
                &printer,
                true,
                Some("pkcs11:token=cfgd-signing;object=cosign-key;type=private"),
            );
            let crd_json =
                build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", signature)
                    .expect("build crd json");
            let spec = crd_spec(&crd_json);

            let result = check_unsigned_policy(&spec, true);

            assert!(
                result.is_err(),
                "a PKCS#11 key reference with no derivable public key must still fail the real disallowUnsigned admission rule"
            );
            let warning = cfgd_core::test_helpers::captured_text(&buf);
            assert!(
                warning.contains("Could not read the public key of 'pkcs11:"),
                "a pkcs11: --key is recognized as a non-filesystem reference rather than \
                 mistaken for a path (it has no `://`, which `contains(\"://\")` alone \
                 would miss), and the warning names the step that failed: {warning:?}"
            );
        }

        /// The round-trip / completeness guard. `build_module_crd_json`
        /// destructures every local type field by field, so a field added to
        /// the local kind fails to compile until it is mapped or explicitly
        /// dropped; this document is what pins the VALUES that mapping
        /// produces, for every field at once.
        #[test]
        fn a_module_exercising_every_local_field_builds_the_full_crd_spec() {
            let module_doc = parse_module(FULL_MODULE_YAML).expect("parse module.yaml");
            let crd_json = build_module_crd_json(&module_doc, "localhost:5000/test/full:v1", None)
                .expect("build crd json");

            assert_eq!(
                crd_json["spec"],
                serde_json::json!({
                    "ociArtifact": "localhost:5000/test/full:v1",
                    "depends": ["base"],
                    "platforms": ["linux"],
                    "packages": [{
                        "name": "sed",
                        "aliases": { "apt": "sed", "brew": "gnu-sed" },
                        "minVersion": "4.8",
                        "prefer": ["brew", "apt"],
                        "deny": ["snap"],
                        "platforms": ["linux"],
                    }],
                    "files": [
                        {
                            "source": "files/init.lua",
                            "target": "~/.config/nvim/init.lua",
                            "strategy": "Copy",
                            "private": true,
                            "encryption": { "backend": "sops", "mode": "Always" },
                            "permissions": "600",
                        },
                        {
                            "source": "",
                            "target": "~/.gitconfig",
                            "strategy": "Patch",
                            "patch": {
                                "format": "Ini",
                                "ensure": { "user": { "name": "Ada" } },
                            },
                        },
                    ],
                    "env": [
                        { "name": "FOO", "value": "bar", "append": false },
                        { "name": "MAC_ONLY", "value": "yes", "append": false, "platforms": ["macos"] },
                    ],
                    "aliases": [
                        { "name": "ll", "command": "ls -la" },
                        { "name": "mac-ls", "command": "ls -G", "platforms": ["macos"] },
                    ],
                    "system": { "shell": { "defaultShell": "zsh" } },
                    "scripts": {},
                    "hooks": {
                        "preApply": ["echo pre"],
                        "postApply": ["echo one", "echo two"],
                        "preReconcile": [{ "run": "echo pre-reconcile", "timeout": "30s" }],
                        "postReconcile": ["echo post-reconcile"],
                        "onDrift": ["echo drift"],
                        "onChange": ["echo change"],
                    },
                }),
                "every field the local module kind declares reaches the Module CRD, so a \
                 module published to a registry and read back out of the cluster still \
                 declares what its author wrote"
            );
        }

        #[test]
        fn crd_post_apply_stays_the_init_container_path_while_hooks_carry_the_inline_bodies() {
            let module_doc = parse_module(FULL_MODULE_YAML).expect("parse module.yaml");
            let crd_json = build_module_crd_json(&module_doc, "localhost:5000/test/full:v1", None)
                .expect("build crd json");
            let spec = &crd_json["spec"];

            assert!(
                spec["scripts"].get("postApply").is_none(),
                "the CRD's scripts.postApply is a relative script PATH the operator's \
                 mutating webhook runs verbatim as an unquoted shell command in an init \
                 container; a module directory declares no such path, so nothing may be \
                 written there: {spec:?}"
            );
            assert_eq!(
                spec["hooks"]["postApply"],
                serde_json::json!(["echo one", "echo two"]),
                "the agent's inline hook bodies travel as spec.hooks, their own field, \
                 where every guard and timeout survives instead of being flattened into \
                 a path: {spec:?}"
            );
        }

        #[test]
        fn applied_crd_never_claims_mount_policy_field() {
            let module_doc = parse_module(MINIMAL_MODULE_YAML).expect("parse module.yaml");
            let crd_json = build_module_crd_json(&module_doc, "localhost:5000/test/mod:v1", None)
                .expect("build crd json");

            assert!(
                crd_json["spec"].get("mountPolicy").is_none(),
                "mountPolicy is cluster-side state; the CLI's server-side apply must not \
                 claim it or an operator-set Debug policy would be reverted/conflicted: {:?}",
                crd_json["spec"]
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
            None,
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
        assert_eq!(
            doc["digest"],
            cfgd_core::sha256_digest(&serde_json::to_vec(&manifest).unwrap()),
            "{doc}"
        );
        assert_eq!(
            doc.get("indexDigest"),
            Some(&serde_json::Value::Null),
            "a tag naming one manifest has no index: {doc}"
        );
    }

    #[test]
    fn pull_of_a_platform_the_index_lacks_fails_naming_both() {
        let mut server = mockito::Server::new();
        let registry = server.url().trim_start_matches("http://").to_string();
        server
            .mock("GET", "/v2/test/mod/manifests/v1")
            .with_status(200)
            .with_header("Content-Type", "application/vnd.oci.image.index.v1+json")
            .with_body(
                serde_json::json!({
                    "schemaVersion": 2,
                    "mediaType": "application/vnd.oci.image.index.v1+json",
                    "manifests": [{
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                        "digest": "sha256:a1",
                        "size": 1,
                        "platform": { "os": "linux", "architecture": "amd64" },
                    }],
                })
                .to_string(),
            )
            .create();
        let output_dir = tempfile::tempdir().expect("output dir");
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_module_pull(
            &printer,
            &format!("{registry}/test/mod:v1"),
            output_dir.path().to_str().unwrap(),
            Some("plan9/mips"),
            false,
            false,
            cfgd_core::oci::VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        )
        .expect_err("no plan9/mips entry");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(meta.error_kind, "pull_failed", "{meta:?}");
        assert!(
            meta.message.contains("plan9/mips") && meta.message.contains("linux/amd64"),
            "the error names the platform asked for and the ones held: {meta:?}"
        );
    }

    /// `cmd_module_pull` called `pull_module` on a bare
    /// `printer` with no section wrapping it, while `cmd_module_push` (see
    /// `push_settle_line_nests_under_the_push_section_header` below) already
    /// opened a section around the matching `push_module` call — an asymmetry,
    /// since both library fns open their own bare top-level spinner the same
    /// way. `cmd_module_pull` now opens `printer.section("Pull Module")` +
    /// `depth_inheritance()` around `pull_module`, so its settle line nests one
    /// level deeper than the section header instead of sitting flush with it,
    /// matching push's shape exactly.
    #[test]
    fn pull_settle_line_nests_under_the_pull_section_header() {
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
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

        cmd_module_pull(
            &printer,
            &artifact_ref,
            output_dir.path().to_str().unwrap(),
            None,
            false,
            false,
            cfgd_core::oci::VerifyOptions {
                key: None,
                identity: None,
                issuer: None,
            },
        )
        .expect("pull must succeed");
        drop(printer);

        let output = cfgd_core::test_helpers::captured_text(&buf);
        crate::cli::test_support::assert_nests_under(&output, "Pull Module", "Pulled module");
    }

    #[test]
    fn push_happy_path_emits_doc_with_digest() {
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
        assert_eq!(
            doc.get("indexDigest"),
            Some(&serde_json::Value::Null),
            "a push that wrote no index says so: {doc}"
        );
    }

    #[test]
    fn push_joining_another_platform_reports_the_index_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_module_yaml(dir.path());

        let (_server, registry) = mock_push_registry_holding(Some(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "annotations": { cfgd_core::OCI_ANNOTATION_PLATFORM: "linux/amd64" },
        })));
        let artifact = format!("{}/test/mod:v1", registry);

        let (printer, cap) = Printer::for_test_doc();
        cmd_module_push(
            &printer,
            dir.path().to_str().unwrap(),
            &artifact,
            PushOptions {
                platform: Some("linux/arm64"),
                apply: false,
                sign: false,
                key: None,
                attest: false,
            },
        )
        .expect("push beside another platform must succeed");
        drop(printer);

        let doc = cap.json().expect("success doc must be emitted");
        assert_eq!(doc["indexDigest"], TAG_DIGEST, "{doc}");
        assert_eq!(doc["digest"], PLATFORM_TAG_DIGEST, "{doc}");
        assert_eq!(doc["platform"], "linux/arm64", "{doc}");
    }
}
