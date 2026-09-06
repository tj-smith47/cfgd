//! Snapshot tests for `cfgd module push` / `pull`.
//!
//! `push` is captured over a mock registry, the way `image pack` is; `pull`'s
//! happy path is exercised in cfgd-core's unit tests against mock responses.

mod common;

use std::path::Path;

use cfgd::cli::error::render_cli_error;
use cfgd::cli::module;
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::Printer;

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn module_push_missing_yaml_human() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, cap) = Printer::for_test_doc();

    let err = module::cmd_module_push(
        &printer,
        dir.path().to_str().unwrap(),
        "oci.example.com/test:v1",
        module::PushOptions {
            platform: None,
            apply: false,
            sign: false,
            key: None,
            attest: false,
        },
    )
    .expect_err("missing module.yaml must return Err");
    render_cli_error(&printer, &err);
    drop(printer);

    let stripped =
        cfgd_core::normalize_for_snapshot(&strip_ansi(&cap.human()), &[(dir.path(), "<DIR>")]);
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "module_push/missing_yaml.txt",
        &stripped,
    );

    let meta = err
        .downcast_ref::<cfgd::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "module_yaml_missing");
}

/// The manifest digest the mock registry answers with, so the golden holds a
/// stable `sha256:` rather than the digest of this run's timestamped manifest.
const MANIFEST_DIGEST: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A registry that accepts one module push: two blob uploads and one
/// manifest, answering the digest above.
fn mock_registry() -> (mockito::ServerGuard, String) {
    let mut server = mockito::Server::new();
    let registry = server.url().trim_start_matches("http://").to_string();
    let artifact = format!("{registry}/test/module:v1");
    let upload_location = format!("{}/v2/test/module/blobs/uploads/up-id", server.url());

    server
        .mock(
            "HEAD",
            mockito::Matcher::Regex(r"/v2/test/module/blobs/sha256:.*".to_string()),
        )
        .with_status(404)
        .expect_at_least(2)
        .create();
    server
        .mock("POST", "/v2/test/module/blobs/uploads/")
        .with_status(202)
        .with_header("Location", &upload_location)
        .expect_at_least(2)
        .create();
    server
        .mock(
            "PUT",
            mockito::Matcher::Regex(
                r"/v2/test/module/blobs/uploads/up-id\?digest=sha256:.*".to_string(),
            ),
        )
        .with_status(201)
        .expect_at_least(2)
        .create();
    server
        .mock("PUT", "/v2/test/module/manifests/v1")
        .with_status(201)
        .with_header("Docker-Content-Digest", MANIFEST_DIGEST)
        .create();

    (server, artifact)
}

fn pushable_module() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: pushed\nspec: {}\n",
    )
    .expect("write module.yaml");
    dir
}

/// The push verdict carries the digest it produced as its detail: no kv row
/// sits between the header block and the closing hint, so a `--sign` run
/// reads as two verdicts in a row rather than a verdict, a fact, a verdict.
#[test]
fn module_push_pushed_human() {
    let dir = pushable_module();
    let (server, artifact) = mock_registry();
    let registry = server.url().trim_start_matches("http://").to_string();
    let dir_str = cfgd_core::to_posix_string(dir.path());

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_push(
        &printer,
        &dir_str,
        &artifact,
        module::PushOptions {
            platform: Some("linux/amd64"),
            apply: false,
            sign: false,
            key: None,
            attest: false,
        },
    )
    .expect("push must succeed against the mock registry");
    drop(printer);

    let normalized = cfgd_core::normalize_snapshot_durations(&strip_ansi(&cap.human()))
        .replace(&registry, "<REGISTRY>")
        .replace(&dir_str, "<DIR>");
    assert!(
        !normalized.contains("Digest "),
        "the digest is the push row's detail, never a kv row: {normalized}"
    );
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "module_push/pushed.txt",
        &normalized,
    );
}
