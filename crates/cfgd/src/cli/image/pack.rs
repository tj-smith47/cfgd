use std::collections::BTreeMap;
use std::path::Path;

use cfgd_core::PathDisplayExt;
use cfgd_core::oci::{PackOptions, PackOutcome};
use cfgd_core::output::{Doc, Printer, Role};

/// Options forwarded from the `cfgd image pack` clap args to the handler.
pub struct ImagePackOptions<'a> {
    pub platform: Option<&'a str>,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: Option<&'a str>,
    pub user: Option<&'a str>,
    pub labels: Vec<String>,
    pub annotations: Vec<String>,
    pub sign: bool,
    pub key: Option<&'a str>,
    pub attest: bool,
}

pub fn cmd_image_pack(
    printer: &Printer,
    dir: &Path,
    artifact: &str,
    opts: ImagePackOptions<'_>,
) -> anyhow::Result<()> {
    let ImagePackOptions {
        platform,
        entrypoint,
        cmd,
        env,
        working_dir,
        user,
        labels,
        annotations,
        sign,
        key,
        attest,
    } = opts;

    if !dir.exists() {
        return Err(crate::cli::cli_error(
            dir.posix().to_string(),
            "not_found",
            format!("Directory '{}' does not exist", dir.posix()),
            serde_json::json!({ "dir": dir.posix().to_string() }),
        ));
    }
    if !dir.is_dir() {
        return Err(crate::cli::cli_error(
            dir.posix().to_string(),
            "invalid",
            format!("'{}' is not a directory", dir.posix()),
            serde_json::json!({ "dir": dir.posix().to_string() }),
        ));
    }

    let parsed_labels = parse_kv_pairs(&labels, "label").map_err(|e| {
        crate::cli::cli_error(
            artifact,
            "invalid_label",
            e.clone(),
            serde_json::json!({ "artifact": artifact, "error": e }),
        )
    })?;

    let parsed_annotations = parse_kv_pairs(&annotations, "annotation").map_err(|e| {
        crate::cli::cli_error(
            artifact,
            "invalid_annotation",
            e.clone(),
            serde_json::json!({ "artifact": artifact, "error": e }),
        )
    })?;

    let pack_opts = PackOptions {
        entrypoint: if entrypoint.is_empty() {
            None
        } else {
            Some(entrypoint)
        },
        cmd: if cmd.is_empty() { None } else { Some(cmd) },
        env,
        working_dir: working_dir.map(|s| s.to_string()),
        user: user.map(|s| s.to_string()),
        labels: parsed_labels,
        annotations: parsed_annotations,
        platform: platform.map(|s| s.to_string()),
    };

    printer.heading("Pack Image");
    let mut header = vec![
        ("Directory".to_string(), dir.posix().to_string()),
        ("Artifact".to_string(), artifact.to_string()),
    ];
    if let Some(p) = platform {
        header.push(("Platform".to_string(), p.to_string()));
    }
    printer.kv_block(header);

    let PackOutcome {
        digest,
        platform: platform_str,
    } = cfgd_core::oci::pack_image(dir, artifact, &pack_opts, Some(printer)).map_err(|e| {
        crate::cli::cli_error(
            artifact,
            "pack_failed",
            cfgd_core::output::collapse_to_subject_line(&e),
            serde_json::json!({ "artifact": artifact, "dir": dir.posix().to_string() }),
        )
    })?;

    printer.kv("Digest", &digest);

    let crate::cli::helpers::SignAttestOutcome {
        signed,
        attested: attestation_attached,
    } = crate::cli::helpers::sign_and_attest(printer, artifact, &digest, key, sign, attest)?;

    printer.status_simple(Role::Ok, format!("Packed and pushed {artifact}"));

    printer.emit(Doc::new().with_data(serde_json::json!({
        "artifact": artifact,
        "digest": digest,
        "platform": platform_str,
        "signed": signed,
        "attested": attestation_attached,
    })));

    Ok(())
}

fn parse_kv_pairs(pairs: &[String], kind: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair.split_once('=').ok_or_else(|| {
            format!("Invalid {kind} '{pair}': expected KEY=VALUE format (missing '=')")
        })?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use cfgd_core::output::Printer;
    use cfgd_core::test_helpers::test_printer;

    use super::{ImagePackOptions, cmd_image_pack};

    fn no_opts() -> ImagePackOptions<'static> {
        ImagePackOptions {
            platform: None,
            entrypoint: vec![],
            cmd: vec![],
            env: vec![],
            working_dir: None,
            user: None,
            labels: vec![],
            annotations: vec![],
            sign: false,
            key: None,
            attest: false,
        }
    }

    #[test]
    fn cmd_image_pack_nonexistent_dir_errors() {
        let printer = test_printer();
        let err = cmd_image_pack(
            &printer,
            std::path::Path::new("/nonexistent/path/that/does/not/exist"),
            "localhost:5000/test/image:v1",
            no_opts(),
        )
        .expect_err("nonexistent dir must return Err");

        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent") || msg.contains("exist"),
            "error must mention the path or existence: {msg}"
        );
    }

    #[test]
    fn cmd_image_pack_nonexistent_dir_error_kind_is_not_found() {
        let (printer, _cap) = Printer::for_test_doc();
        let err = cmd_image_pack(
            &printer,
            std::path::Path::new("/nonexistent/path/that/does/not/exist"),
            "localhost:5000/test/image:v1",
            no_opts(),
        )
        .expect_err("nonexistent dir must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "not_found",
            "error kind must be not_found: {meta:?}"
        );
        assert!(
            meta.extras["dir"].is_string(),
            "meta must carry dir payload: {:?}",
            meta.extras
        );
    }

    #[test]
    fn cmd_image_pack_path_is_file_error_kind_is_invalid() {
        // A path that exists but is a regular file (not a directory) must hit
        // the !is_dir() branch and surface error kind "invalid".
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        let (printer, _cap) = Printer::for_test_doc();
        let err = cmd_image_pack(
            &printer,
            file.path(),
            "localhost:5000/test/image:v1",
            no_opts(),
        )
        .expect_err("non-directory path must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "invalid",
            "error kind for a non-directory path must be invalid: {meta:?}"
        );
        assert!(
            meta.extras["dir"].is_string(),
            "meta must carry dir payload: {:?}",
            meta.extras
        );
    }

    #[test]
    fn cmd_image_pack_bad_annotation_error_kind_is_invalid_annotation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (printer, _cap) = Printer::for_test_doc();
        let err = cmd_image_pack(
            &printer,
            dir.path(),
            "localhost:5000/test/image:v1",
            ImagePackOptions {
                annotations: vec!["noequals".to_string()],
                ..no_opts()
            },
        )
        .expect_err("bad annotation must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "invalid_annotation",
            "error kind must be invalid_annotation: {meta:?}"
        );
        assert!(
            meta.extras["artifact"].is_string(),
            "meta must carry artifact payload: {:?}",
            meta.extras
        );
    }

    #[test]
    fn cmd_image_pack_bad_label_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let printer = test_printer();
        let err = cmd_image_pack(
            &printer,
            dir.path(),
            "localhost:5000/test/image:v1",
            ImagePackOptions {
                labels: vec!["bad-label-no-equals".to_string()],
                ..no_opts()
            },
        )
        .expect_err("bad label must return Err");

        let msg = err.to_string();
        assert!(
            msg.contains('=')
                || msg.contains("KEY=VALUE")
                || msg.contains("invalid")
                || msg.contains("label"),
            "error must mention format: {msg}"
        );
    }

    #[test]
    fn cmd_image_pack_bad_label_error_kind_is_invalid_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (printer, _cap) = Printer::for_test_doc();
        let err = cmd_image_pack(
            &printer,
            dir.path(),
            "localhost:5000/test/image:v1",
            ImagePackOptions {
                labels: vec!["noequalssign".to_string()],
                ..no_opts()
            },
        )
        .expect_err("bad label must return Err");
        drop(printer);

        let meta = err
            .downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("handler returns CliErrorMeta");
        assert_eq!(
            meta.error_kind, "invalid_label",
            "error kind must be invalid_label: {meta:?}"
        );
    }

    #[cfg(unix)]
    mod with_mock_registry {
        use cfgd_core::output::Printer;

        use super::super::cmd_image_pack;
        use super::no_opts;

        #[test]
        fn cmd_image_pack_success_emits_doc_payload() {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("config.yaml"), "key: value\n")
                .expect("write config file");

            let mut server = mockito::Server::new();
            let registry = server.url().trim_start_matches("http://").to_string();
            let artifact = format!("{}/test/image:v1", registry);
            let upload_location = format!("{}/v2/test/image/blobs/uploads/up-id", server.url());

            server
                .mock(
                    "HEAD",
                    mockito::Matcher::Regex(r"/v2/test/image/blobs/sha256:.*".to_string()),
                )
                .with_status(404)
                .expect_at_least(2)
                .create();

            server
                .mock("POST", "/v2/test/image/blobs/uploads/")
                .with_status(202)
                .with_header("Location", &upload_location)
                .expect_at_least(2)
                .create();

            server
                .mock(
                    "PUT",
                    mockito::Matcher::Regex(
                        r"/v2/test/image/blobs/uploads/up-id\?digest=sha256:.*".to_string(),
                    ),
                )
                .with_status(201)
                .expect_at_least(2)
                .create();

            server
                .mock("PUT", "/v2/test/image/manifests/v1")
                .with_status(201)
                .with_header(
                    "Docker-Content-Digest",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .create();

            let (printer, cap) = Printer::for_test_doc();

            cmd_image_pack(&printer, dir.path(), &artifact, no_opts())
                .expect("pack should succeed with mock registry");
            drop(printer);

            let doc = cap.json().expect("handler must emit a Doc payload");
            assert!(
                doc.get("artifact")
                    .and_then(|v| v.as_str())
                    .map(|s| s == artifact)
                    .unwrap_or(false),
                "doc must carry artifact: {doc:?}"
            );
            let digest = doc.get("digest").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                digest.starts_with("sha256:"),
                "digest must start with 'sha256:': {digest}"
            );
            assert_eq!(
                doc.get("signed").and_then(|v| v.as_bool()),
                Some(false),
                "signed must be false: {doc:?}"
            );
            assert_eq!(
                doc.get("attested").and_then(|v| v.as_bool()),
                Some(false),
                "attested must be false: {doc:?}"
            );
        }
    }

    #[cfg(unix)]
    mod with_cosign_shim {
        use cfgd_core::output::Printer;
        use cfgd_core::test_helpers::CosignTestShim;
        use serial_test::serial;

        use super::super::{ImagePackOptions, cmd_image_pack};
        use super::no_opts;

        // Stand up a mock OCI registry that accepts the two blob uploads and
        // returns 201 on the manifest PUT, so the pack happy path completes and
        // we reach the sign/attest + doc-emit stage.
        fn mock_pack_registry() -> (mockito::ServerGuard, String) {
            let mut server = mockito::Server::new();
            let registry = server.url().trim_start_matches("http://").to_string();
            let upload_location = format!("{}/v2/test/image/blobs/uploads/up-id", server.url());

            server
                .mock(
                    "HEAD",
                    mockito::Matcher::Regex(r"/v2/test/image/blobs/sha256:.*".to_string()),
                )
                .with_status(404)
                .expect_at_least(2)
                .create();
            server
                .mock("POST", "/v2/test/image/blobs/uploads/")
                .with_status(202)
                .with_header("Location", &upload_location)
                .expect_at_least(2)
                .create();
            server
                .mock(
                    "PUT",
                    mockito::Matcher::Regex(
                        r"/v2/test/image/blobs/uploads/up-id\?digest=sha256:.*".to_string(),
                    ),
                )
                .with_status(201)
                .expect_at_least(2)
                .create();
            server
                .mock("PUT", "/v2/test/image/manifests/v1")
                .with_status(201)
                .create();

            (server, registry)
        }

        #[test]
        #[serial]
        fn cmd_image_pack_sign_success_emits_signed_true_doc() {
            // Cosign shim exits 0; the pack happy path then signs successfully
            // and the emitted JSON doc must record signed=true.
            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(0)
                .install();

            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("config.yaml"), "key: value\n")
                .expect("write config file");
            let (_server, registry) = mock_pack_registry();
            let artifact = format!("{}/test/image:v1", registry);

            let (printer, cap) = Printer::for_test_doc();
            cmd_image_pack(
                &printer,
                dir.path(),
                &artifact,
                ImagePackOptions {
                    sign: true,
                    key: Some("cosign.key"),
                    ..no_opts()
                },
            )
            .expect("pack + sign must succeed");
            drop(printer);

            let doc = cap.json().expect("success doc must be emitted");
            assert_eq!(doc["signed"], true, "signed must be true: {doc}");
            assert_eq!(doc["attested"], false, "attested must be false: {doc}");
        }

        #[test]
        #[serial]
        fn cmd_image_pack_attest_success_emits_attested_true_doc() {
            // Cosign shim exits 0; the pack happy path then attests successfully
            // and the emitted JSON doc must record attested=true.
            let _shim = CosignTestShim::builder()
                .with_argv_logging(false)
                .with_exit(0)
                .install();

            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("config.yaml"), "key: value\n")
                .expect("write config file");
            let (_server, registry) = mock_pack_registry();
            let artifact = format!("{}/test/image:v1", registry);

            let (printer, cap) = Printer::for_test_doc();
            cmd_image_pack(
                &printer,
                dir.path(),
                &artifact,
                ImagePackOptions {
                    attest: true,
                    key: Some("cosign.key"),
                    ..no_opts()
                },
            )
            .expect("pack + attest must succeed");
            drop(printer);

            let doc = cap.json().expect("success doc must be emitted");
            assert_eq!(doc["attested"], true, "attested must be true: {doc}");
            assert_eq!(doc["signed"], false, "signed must be false: {doc}");
        }
    }
}
