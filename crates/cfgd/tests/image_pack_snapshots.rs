//! Snapshot tests for `cfgd image pack`.
//!
//! Goldens live under `tests/output_snapshots/image_pack/`. Regenerate with:
//!     INSTA_UPDATE=always cargo test -p cfgd --test image_pack_snapshots
//!
//! The command pushes to a real registry endpoint, so these drive it against a
//! mockito registry rather than a hand-built payload: what the goldens hold is
//! the render of an actual pack-and-push, which is what `docs/image-pack.md`
//! quotes. The registry's ephemeral `127.0.0.1:PORT` authority is substituted
//! for `<REGISTRY>` before the compare — it is the one part of the render that
//! changes between runs.

use std::path::Path;

use cfgd::cli::image::{ImagePackOptions, cmd_image_pack};
use cfgd_core::assert_snapshot_golden as assert_snapshot;
use cfgd_core::output::{Printer, strip_ansi};

const SNAPSHOT_ROOT: &str = "tests/output_snapshots";
const MANIFEST_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn no_opts() -> ImagePackOptions<'static> {
    ImagePackOptions {
        platform: None,
        entrypoint: Vec::new(),
        cmd: Vec::new(),
        env: Vec::new(),
        working_dir: None,
        user: None,
        labels: Vec::new(),
        annotations: Vec::new(),
        sign: false,
        key: None,
        attest: false,
        base: None,
        lock: None,
    }
}

/// A registry that accepts every blob and manifest push, and the artifact
/// reference addressing it.
fn mock_registry() -> (mockito::ServerGuard, String) {
    let mut server = mockito::Server::new();
    let registry = server.url().trim_start_matches("http://").to_string();
    let artifact = format!("{registry}/test/image:v1");
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
        .with_header("Docker-Content-Digest", MANIFEST_DIGEST)
        .create();

    (server, artifact)
}

/// A directory with one file in it — the "already-produced directory" the
/// command exists to pack.
fn packable_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("config.yaml"), "key: value\n").expect("write payload file");
    dir
}

/// Fold the ephemeral registry authority, the local directory and this host's
/// platform out of a captured render, so the golden holds the shape rather
/// than this run's port — or this runner's os/arch, which the pack row now
/// reports because nothing passed `--platform`.
fn normalized(human: &str, registry: &str, dir: &Path) -> String {
    cfgd_core::normalize_snapshot_durations(&strip_ansi(human))
        .replace(registry, "<REGISTRY>")
        .replace(&cfgd_core::to_posix_string(dir), "<DIR>")
        .replace(&cfgd_core::oci::current_platform(), "<PLATFORM>")
}

#[test]
fn image_pack_human() {
    let dir = packable_dir();
    let (server, artifact) = mock_registry();
    let registry = server.url().trim_start_matches("http://").to_string();

    let (printer, cap) = Printer::for_test_doc();
    cmd_image_pack(&printer, dir.path(), &artifact, no_opts()).expect("pack must succeed");
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "image_pack/packed.txt",
        &normalized(&cap.human(), &registry, dir.path()),
    );
}

#[test]
fn image_pack_with_lock_human() {
    let dir = packable_dir();
    let lock_dir = tempfile::tempdir().expect("lock tempdir");
    let lock_path = lock_dir.path().join("cfgd-images.lock");
    let (server, artifact) = mock_registry();
    let registry = server.url().trim_start_matches("http://").to_string();

    let mut opts = no_opts();
    let lock = lock_path.to_string_lossy().into_owned();
    opts.lock = Some(&lock);

    let (printer, cap) = Printer::for_test_doc();
    cmd_image_pack(&printer, dir.path(), &artifact, opts).expect("pack must succeed");
    drop(printer);

    let human = normalized(&cap.human(), &registry, dir.path())
        .replace(&cfgd_core::to_posix_string(lock_dir.path()), "<LOCKDIR>");
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "image_pack/packed_locked.txt",
        &human,
    );

    // The lockfile is the deploy step's whole input, so the golden above is
    // only half the promise: the entry it announces must be on disk.
    let written = std::fs::read_to_string(&lock_path).expect("lockfile written");
    let lockfile: cfgd_core::config::ImagesLockfile =
        serde_yaml::from_str(&written).expect("lockfile parses");
    assert_eq!(lockfile.images.len(), 1, "one entry: {written}");
    assert_eq!(lockfile.images[0].reference, artifact);
    assert_eq!(lockfile.images[0].digest, MANIFEST_DIGEST);
    assert!(
        lockfile.images[0].pinned.ends_with(MANIFEST_DIGEST),
        "the pinned reference addresses the digest: {}",
        lockfile.images[0].pinned
    );
}

/// `--sign` adds cosign's own verdict under the pack's, which is the pair
/// `docs/image-pack.md`'s quickstart quotes. Serialized: the shim redirects a
/// process-global env seam.
#[test]
#[serial_test::serial]
fn image_pack_signed_human() {
    let _cosign = cfgd_core::test_helpers::CosignTestShim::install();
    let dir = packable_dir();
    let (server, artifact) = mock_registry();
    let registry = server.url().trim_start_matches("http://").to_string();

    let mut opts = no_opts();
    opts.sign = true;

    let (printer, cap) = Printer::for_test_doc();
    cmd_image_pack(&printer, dir.path(), &artifact, opts).expect("pack must succeed");
    drop(printer);

    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "image_pack/packed_signed.txt",
        &normalized(&cap.human(), &registry, dir.path()),
    );
}

#[test]
fn image_pack_json() {
    let dir = packable_dir();
    let (server, artifact) = mock_registry();
    let registry = server.url().trim_start_matches("http://").to_string();

    let (printer, cap) = Printer::for_test_doc();
    cmd_image_pack(&printer, dir.path(), &artifact, no_opts()).expect("pack must succeed");
    drop(printer);

    let mut json = cap.json().expect("pack emits a data payload");
    json["artifact"] = serde_json::Value::String(artifact.replace(&registry, "<REGISTRY>"));
    assert_snapshot!(
        Path::new(SNAPSHOT_ROOT),
        "image_pack/packed.json",
        &serde_json::to_string_pretty(&json).expect("payload serializes"),
    );
}
