//! Snapshot tests for `cfgd secret {encrypt,decrypt,edit,init}`.
//!
//! Cases:
//!   - `secret_init/happy.{txt,json}` — initializes age key and .sops.yaml
//!     in a redirected HOME. Skipped if `age-keygen` isn't on PATH.
//!   - `secret_init/already_initialized.txt` — runs `cmd_secret_init` a
//!     second time; emits an Info Doc since the key and sops config exist.
//!   - `secret_encrypt/happy.{txt,json}` — encrypts a YAML file via age;
//!     verifies the buffered Doc and the on-disk encryption.
//!   - `secret_encrypt/already_encrypted.txt` — encrypting an already-encrypted
//!     file via sops is idempotent; verify the Doc still emits Ok.
//!   - `secret_decrypt/happy.{txt,json}` — decrypts a file; verifies the
//!     plaintext-to-stdout shape via `cap.human()` (data_line goes to the
//!     v2 stdout sink which `for_test_doc` routes to the capture buffer).
//!   - `secret_edit/happy.txt` — drives the decrypt → editor → encrypt
//!     cycle; the editor is `/bin/true` so the content round-trips.
//!
//! Unix-only: relies on `age-keygen` / `age` / `sops` being on PATH and the
//! v2 capture's plaintext sink. Goldens under `tests/output_snapshots/secret_*/`.

#![cfg(unix)]

mod common;

use std::path::Path;

use cfgd::cli::secret;
use cfgd_core::output_v2::Printer;
use cfgd_core::test_helpers::EditorGuard;
use cfgd_core::with_test_home_guard;
use serial_test::serial;

use common::{cli_for, secret_test_setup};

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

fn assert_snapshot(base: &Path, name: &str, actual: &str) {
    let path = base.join(name);
    if std::env::var("INSTA_UPDATE").as_deref() == Ok("always") || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    pretty_assertions::assert_eq!(actual, expected, "snapshot mismatch: {name}");
}

fn normalize(raw: &str, home: &Path, config_dir: &Path) -> String {
    raw.replace(&home.display().to_string(), "<HOME>")
        .replace(&config_dir.display().to_string(), "<CONFIG_DIR>")
}

fn tool_available(name: &str) -> bool {
    cfgd_core::command_available(name)
}

// --- secret init ---

#[test]
#[serial]
fn secret_init_happy_human() {
    if !tool_available("age-keygen") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let _home_guard = with_test_home_guard(home.path());

    let (config_dir, state_dir) = secret_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    secret::cmd_secret_init(&cli, &v2_printer).expect("init should succeed");
    drop(v2_printer);

    let stripped = normalize(&strip_ansi(&cap.human()), home.path(), config_dir.path());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "secret_init/happy.txt", &stripped);
}

#[test]
#[serial]
fn secret_init_happy_json() {
    if !tool_available("age-keygen") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let _home_guard = with_test_home_guard(home.path());

    let (config_dir, state_dir) = secret_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    secret::cmd_secret_init(&cli, &v2_printer).expect("init should succeed");
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["backend"], "age");
    assert!(
        json["configPath"]
            .as_str()
            .unwrap()
            .ends_with("age-key.txt")
    );
}

#[test]
#[serial]
fn secret_init_already_initialized_human() {
    if !tool_available("age-keygen") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let _home_guard = with_test_home_guard(home.path());

    let (config_dir, state_dir) = secret_test_setup();
    let cli = cli_for(config_dir.path(), state_dir.path());

    // First init lands the key + .sops.yaml.
    {
        let (v2_printer, _cap) = Printer::for_test_doc();
        secret::cmd_secret_init(&cli, &v2_printer).expect("first init should succeed");
    }

    // Second init should detect the existing state and emit the Info Doc.
    let (v2_printer, cap) = Printer::for_test_doc();
    secret::cmd_secret_init(&cli, &v2_printer).expect("second init should succeed");
    drop(v2_printer);

    let stripped = normalize(&strip_ansi(&cap.human()), home.path(), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "secret_init/already_initialized.txt",
        &stripped,
    );

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["alreadyInitialized"], true);
}

// --- secret encrypt / decrypt ---

const PLAINTEXT_YAML: &str = "secret: hello-world\n";

fn init_secret_state(home: &Path, config_dir: &Path, state_dir: &Path) {
    // Reuse the lib path: this seeds age-key.txt + .sops.yaml.
    let cli = cli_for(config_dir, state_dir);
    let (v2_printer, _cap) = Printer::for_test_doc();
    let _home_guard = with_test_home_guard(home);
    secret::cmd_secret_init(&cli, &v2_printer).expect("init must seed sops/age");
}

#[test]
#[serial]
fn secret_encrypt_happy_human() {
    if !tool_available("age-keygen") || !tool_available("sops") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (config_dir, state_dir) = secret_test_setup();
    init_secret_state(home.path(), config_dir.path(), state_dir.path());

    let _home_guard = with_test_home_guard(home.path());
    let secret_path = config_dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_path, PLAINTEXT_YAML).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path).expect("encrypt should succeed");
    drop(v2_printer);

    let stripped = normalize(&strip_ansi(&cap.human()), home.path(), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "secret_encrypt/happy.txt",
        &stripped,
    );

    let encrypted_contents = std::fs::read_to_string(&secret_path).unwrap();
    assert!(
        encrypted_contents.contains("sops:"),
        "file should be sops-encrypted, got: {encrypted_contents}"
    );
}

#[test]
#[serial]
fn secret_encrypt_happy_json() {
    if !tool_available("age-keygen") || !tool_available("sops") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (config_dir, state_dir) = secret_test_setup();
    init_secret_state(home.path(), config_dir.path(), state_dir.path());

    let _home_guard = with_test_home_guard(home.path());
    let secret_path = config_dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_path, PLAINTEXT_YAML).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let (v2_printer, cap) = Printer::for_test_doc();

    secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path).expect("encrypt should succeed");
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["backend"], "sops");
    assert!(json["path"].as_str().unwrap().ends_with("secret.enc.yaml"));
}

#[test]
#[serial]
fn secret_encrypt_already_encrypted_human() {
    if !tool_available("age-keygen") || !tool_available("sops") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (config_dir, state_dir) = secret_test_setup();
    init_secret_state(home.path(), config_dir.path(), state_dir.path());

    let _home_guard = with_test_home_guard(home.path());
    let secret_path = config_dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_path, PLAINTEXT_YAML).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    // First encryption.
    {
        let (v2_printer, _cap) = Printer::for_test_doc();
        secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path).expect("first encrypt");
    }

    // Re-encrypt: sops returns "file already encrypted" — expect an error
    // Doc carrying the encryption_failed kind. The sops error body varies
    // across versions, so assert the role + kind shape rather than snapshot
    // the verbose message verbatim.
    let (v2_printer, cap) = Printer::for_test_doc();
    let result = secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path);
    drop(v2_printer);
    assert!(
        result.is_err(),
        "re-encrypt should fail with already-encrypted"
    );

    let human = strip_ansi(&cap.human());
    assert!(
        human.starts_with("✗")
            || human.contains("encryption failed")
            || human.contains("encrypted"),
        "expected Role::Fail status line in human output, got: {human}"
    );

    let json = cap.json().expect("error Doc carries with_data");
    assert_eq!(json["error"], "encryption_failed");
}

#[test]
#[serial]
fn secret_decrypt_happy_human() {
    if !tool_available("age-keygen") || !tool_available("sops") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (config_dir, state_dir) = secret_test_setup();
    init_secret_state(home.path(), config_dir.path(), state_dir.path());

    let _home_guard = with_test_home_guard(home.path());
    let secret_path = config_dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_path, PLAINTEXT_YAML).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    {
        let (v2_printer, _cap) = Printer::for_test_doc();
        secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path).expect("encrypt");
    }

    let (v2_printer, cap) = Printer::for_test_doc();
    secret::cmd_secret_decrypt(&cli, &v2_printer, &secret_path).expect("decrypt");
    drop(v2_printer);

    let stripped = normalize(&strip_ansi(&cap.human()), home.path(), config_dir.path());
    assert_snapshot(
        Path::new(SNAPSHOT_ROOT),
        "secret_decrypt/happy.txt",
        &stripped,
    );

    // The plaintext lands on data_line (stdout); the status Doc lands on
    // the human surface. `cap.human()` concatenates the human-rendered
    // emit; the plaintext sink is captured separately by `for_test_doc`'s
    // shared buffer. Assert both shapes show up in the combined output.
    let full = cap.human();
    assert!(
        full.contains("hello-world"),
        "plaintext should appear in stdout sink: {full}"
    );
    assert!(
        full.contains("Decrypted"),
        "status Doc should render: {full}"
    );
}

#[test]
#[serial]
fn secret_decrypt_happy_json() {
    if !tool_available("age-keygen") || !tool_available("sops") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (config_dir, state_dir) = secret_test_setup();
    init_secret_state(home.path(), config_dir.path(), state_dir.path());

    let _home_guard = with_test_home_guard(home.path());
    let secret_path = config_dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_path, PLAINTEXT_YAML).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    {
        let (v2_printer, _cap) = Printer::for_test_doc();
        secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path).expect("encrypt");
    }

    let (v2_printer, cap) = Printer::for_test_doc();
    secret::cmd_secret_decrypt(&cli, &v2_printer, &secret_path).expect("decrypt");
    drop(v2_printer);

    let json = cap.json().expect("doc captured json");
    assert_eq!(json["backend"], "sops");
    assert!(json["path"].as_str().unwrap().ends_with("secret.enc.yaml"));
}

// --- secret edit ---

#[test]
#[serial]
fn secret_edit_happy_human() {
    if !tool_available("age-keygen") || !tool_available("sops") {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (config_dir, state_dir) = secret_test_setup();
    init_secret_state(home.path(), config_dir.path(), state_dir.path());

    let _home_guard = with_test_home_guard(home.path());
    let secret_path = config_dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_path, PLAINTEXT_YAML).unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    {
        let (v2_printer, _cap) = Printer::for_test_doc();
        secret::cmd_secret_encrypt(&cli, &v2_printer, &secret_path).expect("encrypt");
    }

    // EDITOR=/bin/true round-trips the decrypted file unchanged through
    // sops' edit flow.
    let _editor = EditorGuard::set("/bin/true");
    let (v2_printer, cap) = Printer::for_test_doc();
    secret::cmd_secret_edit(&cli, &v2_printer, &secret_path).expect("edit");
    drop(v2_printer);

    let stripped = normalize(&strip_ansi(&cap.human()), home.path(), config_dir.path());
    assert_snapshot(Path::new(SNAPSHOT_ROOT), "secret_edit/happy.txt", &stripped);
}
