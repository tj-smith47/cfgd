//! Snapshot tests for `cfgd module keys list/generate/rotate`.
//!
//! Cases:
//!   - `module_keys/list_empty.txt` / `.json` — empty workspace, no keys.
//!   - `module_keys_generate/happy.txt` / `.json` — fake-cosign shim writes
//!     `cosign.key` + `cosign.pub` under a tempdir.
//!   - `module_keys_rotate/happy.txt` / `.json` — old key pair on disk is
//!     backed up; shim generates a fresh pair. `artifacts: &[]` so no
//!     OCI re-signing happens.
//!   - `module_keys_rotate/bridge.txt` — per-artifact re-sign spinners
//!     stream, the buffered "Key rotation complete" Doc follows with
//!     exactly one blank line between them (one-blank-line bridge invariant).

mod common;

#[cfg(unix)]
use cfgd::cli::module;
#[cfg(unix)]
use cfgd_core::output::Printer;
#[cfg(unix)]
use cfgd_core::test_helpers::CosignTestShim;
#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
const SNAPSHOT_ROOT: &str = "tests/output_snapshots";

#[cfg(unix)]
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

#[cfg(unix)]
fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();

    // Normalize CRLF→LF: windows captured output has \r\n; committed snapshot is LF.

    let actual_norm = actual.replace("\r\n", "\n");

    let expected_norm = expected.replace("\r\n", "\n");

    pretty_assertions::assert_eq!(actual_norm, expected_norm, "snapshot mismatch: {name}");
}

/// Mask filename-safe ISO 8601 timestamps (e.g. `20260518123456`) so backup
/// filenames are stable across runs.
#[cfg(unix)]
fn mask_timestamp(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        // ISO-8601-filename-safe stamps after iso8601_to_filename_safe are
        // 14 consecutive digits (YYYYMMDDhhmmss).
        if i + 14 <= chars.len()
            && chars[i..i + 14].iter().all(|c| c.is_ascii_digit())
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
            && (i + 14 == chars.len() || !chars[i + 14].is_ascii_alphanumeric())
        {
            out.push_str("<TIMESTAMP>");
            i += 14;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_list_empty_human() {
    // Resolve the snapshot path BEFORE changing the cwd so the "./cosign.*"
    // lookup misses (the relative SNAPSHOT_ROOT would otherwise dangle).
    let snap_root = std::env::current_dir().unwrap().join(SNAPSHOT_ROOT);

    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();

    // Use HOME override so ~/.cfgd lookup also misses.
    let home_guard =
        cfgd_core::test_helpers::EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_list(&printer).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human());
    assert_snapshot(&snap_root, "module_keys/list_empty.txt", &stripped);

    let json = cap.json().expect("doc captured json");
    assert!(json.is_array(), "list payload is a Vec<KeyListEntry>");

    std::env::set_current_dir(original).unwrap();
    drop(home_guard);
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_list_empty_json() {
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let home_guard =
        cfgd_core::test_helpers::EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_list(&printer).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    let arr = json.as_array().expect("Vec payload");
    assert_eq!(arr.len(), 0, "no keys present in empty workspace");

    std::env::set_current_dir(original).unwrap();
    drop(home_guard);
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_generate_happy_human() {
    let _shim = CosignTestShim::builder()
        .with_argv_logging(false)
        .with_keygen(true)
        .install();
    let work = tempfile::tempdir().unwrap();
    let dir_str = work.path().to_str().unwrap();

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_generate(&printer, Some(dir_str)).unwrap();
    drop(printer);

    let stripped = strip_ansi(&cap.human()).replace(dir_str, "<DIR>");
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_keys_generate/happy.txt",
        &stripped,
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_generate_happy_json() {
    let _shim = CosignTestShim::builder()
        .with_argv_logging(false)
        .with_keygen(true)
        .install();
    let work = tempfile::tempdir().unwrap();
    let dir_str = work.path().to_str().unwrap();

    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_generate(&printer, Some(dir_str)).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["dir"], dir_str);
    assert!(
        json["privateKey"]
            .as_str()
            .is_some_and(|s| s.ends_with("cosign.key")),
        "privateKey path written"
    );
    assert!(
        json["publicKey"]
            .as_str()
            .is_some_and(|s| s.ends_with("cosign.pub")),
        "publicKey path written"
    );

    // Tempdir paths vary every run; lock a normalized JSON shape with stable
    // placeholders.
    let normalized = serde_json::json!({
        "dir": "<DIR>",
        "privateKey": "<DIR>/cosign.key",
        "publicKey": "<DIR>/cosign.pub",
    });
    let path = Path::new(SNAPSHOT_ROOT).join("module_keys_generate/happy.json");
    let actual = serde_json::to_string_pretty(&normalized).unwrap();
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(&path).unwrap();
        pretty_assertions::assert_eq!(
            actual,
            expected,
            "snapshot mismatch: module_keys_generate/happy.json"
        );
    }
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_rotate_happy_human() {
    let _shim = CosignTestShim::builder()
        .with_argv_logging(false)
        .with_keygen(true)
        .install();
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    std::fs::write(dir.join("cosign.key"), b"old-private").unwrap();
    std::fs::write(dir.join("cosign.pub"), b"old-public").unwrap();

    let dir_str = dir.to_str().unwrap();
    let (printer, cap) = Printer::for_test_doc();
    // Empty artifacts list — exercises the path that skips OCI re-signing.
    module::cmd_module_keys_rotate(&printer, Some(dir_str), &[]).unwrap();
    drop(printer);

    let stripped = mask_timestamp(&strip_ansi(&cap.human()).replace(dir_str, "<DIR>"));
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_keys_rotate/happy.txt",
        &stripped,
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_rotate_happy_json() {
    let _shim = CosignTestShim::builder()
        .with_argv_logging(false)
        .with_keygen(true)
        .install();
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    std::fs::write(dir.join("cosign.key"), b"old-private").unwrap();
    std::fs::write(dir.join("cosign.pub"), b"old-public").unwrap();

    let dir_str = dir.to_str().unwrap();
    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_rotate(&printer, Some(dir_str), &[]).unwrap();
    drop(printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["dir"], dir_str);
    assert!(json["backupPrivateKey"].as_str().is_some());
    assert!(json["artifactsResigned"].is_array());
    assert_eq!(json["artifactsResigned"].as_array().unwrap().len(), 0);

    // Tempdir paths + timestamp suffixes vary every run; lock a normalized
    // JSON shape with stable placeholders.
    let normalized = serde_json::json!({
        "dir": "<DIR>",
        "backupPrivateKey": "<DIR>/cosign.key.<TIMESTAMP>",
        "backupPublicKey": "<DIR>/cosign.pub.<TIMESTAMP>",
        "artifactsResigned": [],
    });
    let path = Path::new(SNAPSHOT_ROOT).join("module_keys_rotate/happy.json");
    let actual = serde_json::to_string_pretty(&normalized).unwrap();
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(&path).unwrap();
        pretty_assertions::assert_eq!(
            actual,
            expected,
            "snapshot mismatch: module_keys_rotate/happy.json"
        );
    }
}

#[cfg(unix)]
#[test]
#[serial]
fn module_keys_rotate_bridge_one_blank_line() {
    // Bridge invariant: with at least one artifact in the list, the
    // re-sign spinner streams before the buffered "Key rotation complete"
    // Doc — the two surfaces must be separated by exactly one blank line.
    //
    // sign_artifact() shells out via cosign_cmd(), which respects
    // CFGD_COSIGN_BIN; the shim exits 0 so the re-sign succeeds without
    // a real OCI registry.
    let _shim = CosignTestShim::builder()
        .with_argv_logging(false)
        .with_keygen(true)
        .install();
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    std::fs::write(dir.join("cosign.key"), b"old-private").unwrap();
    std::fs::write(dir.join("cosign.pub"), b"old-public").unwrap();

    let dir_str = dir.to_str().unwrap();
    let artifacts = vec!["oci.example.com/m:v1".to_string()];
    let (printer, cap) = Printer::for_test_doc();
    module::cmd_module_keys_rotate(&printer, Some(dir_str), &artifacts).unwrap();
    drop(printer);

    let combined = cap.human();
    assert!(
        combined.contains("\n\n"),
        "bridge missing blank line:\n{combined}"
    );
    assert!(
        !combined.contains("\n\n\n"),
        "bridge has duplicate blank line:\n{combined}"
    );

    let stripped = mask_timestamp(&strip_ansi(&combined).replace(dir_str, "<DIR>"));
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "module_keys_rotate/bridge.txt",
        &stripped,
    );
}
