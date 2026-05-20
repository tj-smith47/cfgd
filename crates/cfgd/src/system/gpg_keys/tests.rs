use super::*;

// --- name() ---

// --- extract_email_from_uid ---

#[test]
fn extract_email_from_uid_cases() {
    let cases: &[(&str, Option<&str>)] = &[
        ("Jane Doe (work) <jane@work.com>", Some("jane@work.com")),
        ("Jane Doe <jane@example.com>", Some("jane@example.com")),
        ("Jane Doe", None),
        ("Jane <>", None),
    ];
    for (input, expected) in cases {
        assert_eq!(
            extract_email_from_uid(input),
            expected.map(String::from),
            "failed for {input:?}",
        );
    }
}

// --- required_capabilities ---

#[test]
fn required_capabilities_mapping() {
    let cases: &[(&str, &[char])] = &[
        ("sign", &['S']),
        ("encrypt", &['E']),
        ("sign,encrypt", &['S', 'E']),
        ("auth", &['A']),
        ("certify", &[]),
    ];
    for (input, expected) in cases {
        assert_eq!(
            required_capabilities(input),
            *expected,
            "failed for {input:?}"
        );
    }
}

// --- GpgKeyType::from_str ---

#[test]
fn key_type_from_str_cases() {
    let cases: &[(&str, Option<GpgKeyType>)] = &[
        ("ed25519", Some(GpgKeyType::Ed25519)),
        ("rsa4096", Some(GpgKeyType::Rsa4096)),
        ("dsa", None),
    ];
    for (input, expected) in cases {
        assert_eq!(
            GpgKeyType::from_str(input),
            *expected,
            "failed for {input:?}"
        );
    }
}

// --- build_param_file ---

#[test]
fn param_file_ed25519() {
    let spec = GpgKeySpec {
        name: "work".to_string(),
        key_type: GpgKeyType::Ed25519,
        real_name: "Jane Doe".to_string(),
        email: "jane@work.com".to_string(),
        expiry: "2y".to_string(),
        usage: "sign".to_string(),
    };
    let param = build_param_file(&spec);
    assert!(param.contains("%no-protection"), "missing %no-protection");
    assert!(param.contains("Key-Type: eddsa"), "missing Key-Type: eddsa");
    assert!(param.contains("Key-Curve: ed25519"), "missing Key-Curve");
    assert!(param.contains("Key-Usage: sign"), "missing Key-Usage");
    assert!(param.contains("Name-Real: Jane Doe"), "missing Name-Real");
    assert!(
        param.contains("Name-Email: jane@work.com"),
        "missing Name-Email"
    );
    assert!(param.contains("Expire-Date: 2y"), "missing Expire-Date");
    assert!(param.contains("%commit"), "missing %commit");
}

#[test]
fn param_file_rsa4096() {
    let spec = GpgKeySpec {
        name: "enc".to_string(),
        key_type: GpgKeyType::Rsa4096,
        real_name: "Bob Smith".to_string(),
        email: "bob@example.com".to_string(),
        expiry: "1y".to_string(),
        usage: "encrypt".to_string(),
    };
    let param = build_param_file(&spec);
    assert!(param.contains("Key-Type: rsa"), "missing Key-Type: rsa");
    assert!(
        param.contains("Key-Length: 4096"),
        "missing Key-Length: 4096"
    );
    assert!(!param.contains("ed25519"), "should not contain ed25519");
    assert!(param.contains("Key-Usage: encrypt"), "missing Key-Usage");
}

#[test]
fn param_file_multi_usage() {
    let spec = GpgKeySpec {
        name: "combo".to_string(),
        key_type: GpgKeyType::Ed25519,
        real_name: "Alice".to_string(),
        email: "alice@example.com".to_string(),
        expiry: "0".to_string(),
        usage: "sign,encrypt".to_string(),
    };
    let param = build_param_file(&spec);
    assert!(
        param.contains("Key-Usage: sign encrypt"),
        "multi-usage should be space-separated"
    );
}

// --- parse_gpg_colon_output ---

const SAMPLE_OUTPUT: &str = "\
pub:-:255:22:AABBCCDD11223344:1700000000:1800000000::u:::scESC:::23::\n\
fpr:::::::::AABBCCDD11223344AABBCCDD11223344AABBCCDD:\n\
uid:u::::1700000000::ABC123::Jane Doe <jane@work.com>::::::::::0:\n\
sub:-:255:18:DDCCBBAA44332211:1700000000:1800000000::u:::e:::\n\
";

#[test]
fn parse_colon_extracts_entry() {
    let entries = parse_gpg_colon_output(SAMPLE_OUTPUT);
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.email, "jane@work.com");
    assert_eq!(e.fingerprint, "AABBCCDD11223344AABBCCDD11223344AABBCCDD");
    assert!(!e.is_revoked());
}

#[test]
fn parse_colon_revoked_key() {
    let output = "\
pub:r:255:22:AABBCCDD11223344:1700000000:0::r:::scESC:::23::\n\
fpr:::::::::REVOKEDFPR:\n\
uid:r::::1700000000::ABC123::Revoked User <rev@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_revoked());
}

#[test]
fn parse_colon_expired_key() {
    // expiry_ts of 1 is well in the past
    let output = "\
pub:e:255:22:AABBCCDD11223344:1000000000:1::-:::scESC:::23::\n\
fpr:::::::::EXPIREDFPR:\n\
uid:e::::1000000000::ABC123::Expired User <exp@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_expired());
}

#[test]
fn parse_colon_no_expiry() {
    let output = "\
pub:u:255:22:AABBCCDD11223344:1700000000:0::u:::scESC:::23::\n\
fpr:::::::::NOEEXPIRYFPR:\n\
uid:u::::1700000000::ABC123::No Expiry <noexp@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].is_expired());
}

#[test]
fn parse_colon_empty_output() {
    let entries = parse_gpg_colon_output("");
    assert!(entries.is_empty());
}

// --- diff() with empty/non-sequence desired ---

#[test]
fn diff_empty_sequence_returns_no_drift() {
    let configurator = GpgKeysConfigurator;
    let desired = serde_yaml::Value::Sequence(Vec::new());
    let drifts = configurator.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn diff_non_sequence_returns_no_drift() {
    let configurator = GpgKeysConfigurator;
    let desired = serde_yaml::Value::String("not a sequence".into());
    let drifts = configurator.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- current_state() ---

#[test]
fn current_state_is_empty_sequence() {
    let state = GpgKeysConfigurator.current_state().unwrap();
    assert!(state.is_sequence());
    assert!(state.as_sequence().unwrap().is_empty());
}

// --- parse_key_spec ---

#[test]
fn parse_key_spec_all_fields() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
name: work-signing
type: ed25519
realName: "Jane Doe"
email: jane@work.com
expiry: 2y
usage: sign
"#,
    )
    .unwrap();
    let spec = parse_key_spec(&yaml).unwrap();
    assert_eq!(spec.name, "work-signing");
    assert_eq!(spec.key_type, GpgKeyType::Ed25519);
    assert_eq!(spec.real_name, "Jane Doe");
    assert_eq!(spec.email, "jane@work.com");
    assert_eq!(spec.expiry, "2y");
    assert_eq!(spec.usage, "sign");
}

#[test]
fn parse_key_spec_defaults() {
    // type, expiry, usage have defaults
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
name: minimal
realName: "Alice"
email: alice@example.com
"#,
    )
    .unwrap();
    let spec = parse_key_spec(&yaml).unwrap();
    assert_eq!(spec.key_type, GpgKeyType::Ed25519);
    assert_eq!(spec.expiry, "2y");
    assert_eq!(spec.usage, "sign");
}

#[test]
fn parse_key_spec_missing_email_returns_none() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
name: bad
realName: "No Email"
"#,
    )
    .unwrap();
    assert!(parse_key_spec(&yaml).is_none());
}

#[test]
fn parse_key_spec_bad_type_returns_none() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
name: bad
type: dsa512
realName: "Bad Type"
email: bad@example.com
"#,
    )
    .unwrap();
    assert!(parse_key_spec(&yaml).is_none());
}

// --- Integration test with isolated GNUPGHOME ---
// Only runs when gpg binary is available.

#[test]
fn gpg_integration_generate_and_detect() {
    if !cfgd_core::command_available("gpg") {
        return; // skip if gpg not installed
    }

    let tmp = tempfile::tempdir().unwrap();
    let gnupghome = tmp.path().to_str().unwrap();

    // Generate a key in the isolated keyring
    let param = "%no-protection\nKey-Type: eddsa\nKey-Curve: ed25519\nKey-Usage: sign\nName-Real: Test User\nName-Email: test@cfgd.local\nExpire-Date: 1y\n%commit\n";
    let param_path = tmp.path().join("gen.params");
    std::fs::write(&param_path, param).unwrap();

    let gen_output = Command::new("gpg")
        .env("GNUPGHOME", gnupghome)
        .args(["--batch", "--gen-key", param_path.to_str().unwrap()])
        .output()
        .unwrap();

    if !gen_output.status.success() {
        // gpg key gen might fail in sandboxed envs; skip gracefully
        return;
    }

    // Now query the isolated keyring
    let list = Command::new("gpg")
        .env("GNUPGHOME", gnupghome)
        .args([
            "--list-keys",
            "--with-colons",
            "--with-fingerprint",
            "test@cfgd.local",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&list.stdout);
    let entries = parse_gpg_colon_output(&stdout);
    let matching: Vec<&KeyringEntry> = entries
        .iter()
        .filter(|e| e.email.eq_ignore_ascii_case("test@cfgd.local") && !e.is_revoked())
        .collect();

    assert!(!matching.is_empty(), "key should be found after generation");
    let valid = matching.iter().any(|k| !k.is_expired());
    assert!(valid, "newly generated key should not be expired");
}

// --- parse_gpg_colon_output additional edge cases ---

#[test]
fn parse_colon_multiple_pub_blocks() {
    let output = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::\n\
fpr:::::::::FPR1:\n\
uid:u::::1700000000::HASH1::User One <one@example.com>::::::::::0:\n\
pub:u:255:22:BBBB:1700000000:0::u:::E:::23::\n\
fpr:::::::::FPR2:\n\
uid:u::::1700000000::HASH2::User Two <two@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].fingerprint, "FPR1");
    assert_eq!(entries[0].email, "one@example.com");
    assert_eq!(entries[0].capabilities, "SC");
    assert_eq!(entries[1].fingerprint, "FPR2");
    assert_eq!(entries[1].email, "two@example.com");
    assert_eq!(entries[1].capabilities, "E");
}

#[test]
fn parse_colon_pub_without_fpr() {
    let output = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::\n\
uid:u::::1700000000::HASH::User <user@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    // No fpr line means fingerprint is empty => entry is NOT flushed by end
    // because the code checks !current_fingerprint.is_empty()
    assert!(entries.is_empty());
}

#[test]
fn parse_colon_sub_records_are_ignored() {
    // sub records should not create additional entries
    let output = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::\n\
fpr:::::::::PRIMARYFPR:\n\
uid:u::::1700000000::HASH::User <user@example.com>::::::::::0:\n\
sub:u:255:18:BBBB:1700000000:0::u:::e:::\n\
fpr:::::::::SUBFPR:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fingerprint, "PRIMARYFPR");
}

#[test]
fn parse_colon_capabilities_are_uppercased() {
    let output = "\
pub:u:255:22:AAAA:1700000000:0::u:::sc:::23::\n\
fpr:::::::::FPR:\n\
uid:u::::1700000000::HASH::User <user@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries[0].capabilities, "SC");
}

// --- KeyringEntry validity ---

#[test]
fn keyring_entry_not_expired_with_far_future_expiry() {
    let entry = KeyringEntry {
        fingerprint: "FPR".to_string(),
        email: "user@test.com".to_string(),
        validity: 'u',
        expiry_ts: u64::MAX / 2, // far future
        capabilities: "SC".to_string(),
    };
    assert!(!entry.is_expired());
    assert!(!entry.is_revoked());
}

#[test]
fn keyring_entry_expired_with_validity_e() {
    let entry = KeyringEntry {
        fingerprint: "FPR".to_string(),
        email: "user@test.com".to_string(),
        validity: 'e',
        expiry_ts: 0,
        capabilities: "SC".to_string(),
    };
    assert!(entry.is_expired());
}

#[test]
fn keyring_entry_not_expired_with_zero_expiry() {
    // expiry_ts == 0 means no expiry
    let entry = KeyringEntry {
        fingerprint: "FPR".to_string(),
        email: "user@test.com".to_string(),
        validity: 'u',
        expiry_ts: 0,
        capabilities: "SC".to_string(),
    };
    assert!(!entry.is_expired());
}

// --- extract_email_from_uid additional ---

#[test]
fn extract_email_reversed_brackets() {
    // > before < should return None
    assert_eq!(extract_email_from_uid("Name >bad< format"), None);
}

#[test]
fn extract_email_multiple_angle_brackets() {
    // Should extract from the last pair
    assert_eq!(
        extract_email_from_uid("Name <old@email.com> Alias <new@email.com>"),
        Some("new@email.com".to_string())
    );
}

// --- required_capabilities additional ---

#[test]
fn required_caps_with_spaces() {
    assert_eq!(required_capabilities("sign , encrypt"), vec!['S', 'E']);
}

#[test]
fn required_caps_all_three() {
    assert_eq!(
        required_capabilities("sign,encrypt,auth"),
        vec!['S', 'E', 'A']
    );
}

#[test]
fn required_caps_empty_string() {
    assert_eq!(required_capabilities(""), Vec::<char>::new());
}

// --- parse_key_spec additional ---

#[test]
fn parse_key_spec_missing_name_returns_none() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
realName: "No Name Field"
email: test@example.com
"#,
    )
    .unwrap();
    assert!(parse_key_spec(&yaml).is_none());
}

#[test]
fn parse_key_spec_missing_real_name_returns_none() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
name: test
email: test@example.com
"#,
    )
    .unwrap();
    assert!(parse_key_spec(&yaml).is_none());
}

#[test]
fn parse_key_spec_rsa4096_type() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
name: rsa-key
type: rsa4096
realName: "RSA User"
email: rsa@example.com
usage: encrypt
"#,
    )
    .unwrap();
    let spec = parse_key_spec(&yaml).unwrap();
    assert_eq!(spec.key_type, GpgKeyType::Rsa4096);
    assert_eq!(spec.usage, "encrypt");
}

// --- build_param_file additional ---

#[test]
fn build_param_file_no_expiry() {
    let spec = GpgKeySpec {
        name: "no-expire".to_string(),
        key_type: GpgKeyType::Ed25519,
        real_name: "Test".to_string(),
        email: "test@test.com".to_string(),
        expiry: "0".to_string(),
        usage: "sign".to_string(),
    };
    let param = build_param_file(&spec);
    assert!(param.contains("Expire-Date: 0"));
}

#[test]
fn build_param_file_auth_usage() {
    let spec = GpgKeySpec {
        name: "auth-key".to_string(),
        key_type: GpgKeyType::Ed25519,
        real_name: "Auth User".to_string(),
        email: "auth@test.com".to_string(),
        expiry: "1y".to_string(),
        usage: "auth".to_string(),
    };
    let param = build_param_file(&spec);
    assert!(param.contains("Key-Usage: auth"));
}

// --- diff with non-parseable entry ---

#[test]
fn diff_skips_unparseable_entries() {
    let configurator = GpgKeysConfigurator;
    // Entry missing required fields
    let bad_entry: serde_yaml::Value = serde_yaml::from_str(
        r#"
- displayName: "No name or email"
- name: also-bad
  type: dsa512
  realName: "Bad Type"
  email: bad@example.com
"#,
    )
    .unwrap();
    // Should not panic, just skip unparseable entries
    let drifts = configurator.diff(&bad_entry).unwrap();
    // The entries are unparseable so no drift from them
    assert!(drifts.is_empty());
}

// --- KeyringEntry::is_expired: timestamp-based expiry ---

#[test]
fn keyring_entry_expired_by_timestamp() {
    // validity is 'u' (ultimate trust), but expiry_ts is in the past
    let entry = KeyringEntry {
        fingerprint: "OLDKEY".to_string(),
        email: "old@test.com".to_string(),
        validity: 'u',
        expiry_ts: 1, // Unix epoch + 1 second: 1970-01-01T00:00:01Z
        capabilities: "SC".to_string(),
    };
    assert!(entry.is_expired(), "key with expiry_ts=1 should be expired");
}

#[test]
fn keyring_entry_not_expired_validity_u_far_future() {
    let entry = KeyringEntry {
        fingerprint: "FUTURE".to_string(),
        email: "future@test.com".to_string(),
        validity: 'u',
        expiry_ts: u64::MAX - 1,
        capabilities: "E".to_string(),
    };
    assert!(!entry.is_expired());
}

// --- parse_gpg_colon_output: missing field coverage ---

#[test]
fn parse_colon_pub_missing_expiry_field() {
    // pub record with fewer fields than expected (no expiry)
    let output = "\
pub:u:255:22:AAAA:1700000000\n\
fpr:::::::::SHORTFPR:\n\
uid:u::::1700000000::HASH::Short User <short@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].expiry_ts, 0,
        "missing expiry field should default to 0"
    );
    assert!(
        entries[0].capabilities.is_empty(),
        "missing capabilities field should be empty"
    );
}

#[test]
fn parse_colon_pub_missing_validity_char() {
    // pub record with empty validity field
    let output = "\
pub::255:22:AAAA:1700000000:0::u:::SC:::23::\n\
fpr:::::::::FPR:\n\
uid:u::::1700000000::HASH::User <user@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    // Empty validity field means chars().next() returns None, defaults to '-'
    assert_eq!(entries[0].validity, '-');
}

#[test]
fn parse_colon_uid_without_email() {
    // uid record that doesn't contain angle brackets
    let output = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::\n\
fpr:::::::::FPR_NOEMAIL:\n\
uid:u::::1700000000::HASH::Just A Name Without Email::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].email.is_empty(),
        "uid without angle brackets should produce empty email"
    );
}

#[test]
fn parse_colon_multiple_uids_uses_first() {
    // First uid has an email, second has a different one; should capture first
    let output = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::\n\
fpr:::::::::MULTIUID:\n\
uid:u::::1700000000::HASH1::Primary <primary@example.com>::::::::::0:\n\
uid:u::::1700000000::HASH2::Secondary <secondary@example.com>::::::::::0:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].email, "primary@example.com",
        "should use the first uid email"
    );
}

#[test]
fn parse_colon_non_pub_fpr_uid_lines_ignored() {
    // Lines with unknown record types should be ignored
    let output = "\
tru::1:1700000000:0:3:1:5\n\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::\n\
fpr:::::::::FPROK:\n\
uid:u::::1700000000::HASH::User <user@ok.com>::::::::::0:\n\
grp:::::::::0123456789ABCDEF0123456789ABCDEF:\n\
";
    let entries = parse_gpg_colon_output(output);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].email, "user@ok.com");
}

// --- build_param_file: verify RSA structure ---

#[test]
fn build_param_file_rsa_has_no_curve() {
    let spec = GpgKeySpec {
        name: "rsa-nocurve".to_string(),
        key_type: GpgKeyType::Rsa4096,
        real_name: "RSA Test".to_string(),
        email: "rsa@test.com".to_string(),
        expiry: "6m".to_string(),
        usage: "sign,encrypt".to_string(),
    };
    let param = build_param_file(&spec);
    assert!(param.contains("Key-Type: rsa"));
    assert!(param.contains("Key-Length: 4096"));
    assert!(
        !param.contains("Key-Curve"),
        "RSA should not have Key-Curve"
    );
    assert!(param.contains("Key-Usage: sign encrypt"));
    assert!(param.contains("Expire-Date: 6m"));
}

// --- GpgKeysConfigurator trait methods ---

#[test]
fn configurator_name_is_gpg_keys() {
    let c = GpgKeysConfigurator;
    assert_eq!(c.name(), "gpgKeys");
}

// --- apply with non-sequence desired ---

#[test]
fn apply_non_sequence_is_noop() {
    let c = GpgKeysConfigurator;
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let desired = serde_yaml::Value::String("not a sequence".into());
    let result = c.apply(&desired, &printer);
    assert!(
        result.is_ok(),
        "apply with non-sequence should succeed as no-op"
    );
}

#[test]
fn apply_empty_sequence_is_noop() {
    let c = GpgKeysConfigurator;
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let desired = serde_yaml::Value::Sequence(Vec::new());
    let result = c.apply(&desired, &printer);
    assert!(
        result.is_ok(),
        "apply with empty sequence should succeed as no-op"
    );
}

#[test]
fn apply_unparseable_entries_skipped() {
    let c = GpgKeysConfigurator;
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let desired: serde_yaml::Value = serde_yaml::from_str(
        r#"
- displayName: "Missing required fields"
"#,
    )
    .unwrap();
    let result = c.apply(&desired, &printer);
    assert!(result.is_ok(), "apply should skip unparseable entries");
}

// ---------------------------------------------------------------------------
// SystemConfigurator-impl tests via CFGD_GPG_BIN ToolShim.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod gpg_shim {
    use super::*;
    use cfgd_core::providers::SystemConfigurator;
    use cfgd_core::test_helpers::{ToolShim, test_printer};
    use serial_test::serial;

    const SHIM_ENV: &str = "CFGD_GPG_BIN";

    #[test]
    #[serial]
    fn is_available_true_when_seam_points_to_existing_file() {
        let _s = ToolShim::install(SHIM_ENV, 0, "", "");
        assert!(GpgKeysConfigurator.is_available());
    }

    #[test]
    #[serial]
    fn is_available_false_when_seam_points_to_missing_file() {
        // Snapshot + restore so we don't pollute other serial tests.
        let prev = std::env::var_os("CFGD_GPG_BIN");
        // SAFETY: serial test, no concurrent reader.
        unsafe {
            std::env::set_var("CFGD_GPG_BIN", "/this/path/does/not/exist/gpg");
        }
        let available = GpgKeysConfigurator.is_available();
        // SAFETY: serial.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CFGD_GPG_BIN", v),
                None => std::env::remove_var("CFGD_GPG_BIN"),
            }
        }
        assert!(!available);
    }

    #[test]
    #[serial]
    fn query_keys_for_email_records_expected_argv() {
        let s = ToolShim::install(SHIM_ENV, 0, "", "");
        let entries = query_keys_for_email("jane@work.com").expect("Ok");
        assert!(entries.is_empty());
        let argv = s.argv_log();
        assert!(
            argv.contains("--list-keys --with-colons --with-fingerprint jane@work.com"),
            "expected gpg list-keys argv, got: {argv}"
        );
        assert_eq!(s.invocation_count(), 1);
    }

    #[test]
    #[serial]
    fn query_keys_for_email_returns_empty_on_gpg_exit_2() {
        // exit 2 = "no keys matched" — must NOT propagate as error
        let _s = ToolShim::install(SHIM_ENV, 2, "", "no public key");
        let entries = query_keys_for_email("nobody@example.com").expect("Ok");
        assert!(entries.is_empty());
    }

    #[test]
    #[serial]
    fn query_keys_for_email_propagates_other_exit_codes_with_stderr() {
        let _s = ToolShim::install(SHIM_ENV, 1, "", "gpg: fatal: keyring busted");
        let err = query_keys_for_email("x@y.z").expect_err("expected error");
        let msg = err.to_string();
        assert!(
            msg.contains("exit 1") && msg.contains("keyring busted"),
            "expected exit-code + stderr in error, got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn query_keys_for_email_parses_pub_records_from_shim_stdout() {
        let stdout = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::
fpr:::::::::FPR-ABC:
uid:u::::1700000000::HASH::Jane Doe <jane@work.com>::::::::::0:
";
        let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
        let entries = query_keys_for_email("jane@work.com").expect("Ok");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fingerprint, "FPR-ABC");
        assert_eq!(entries[0].email, "jane@work.com");
        assert_eq!(entries[0].capabilities, "SC");
    }

    #[test]
    #[serial]
    fn query_keys_for_email_filters_non_matching_emails() {
        // Shim returns two pub blocks; only the matching email survives.
        let stdout = "\
pub:u:255:22:AAAA:1700000000:0::u:::SC:::23::
fpr:::::::::FPR-A:
uid:u::::1700000000::HASH1::Other <other@example.com>::::::::::0:
pub:u:255:22:BBBB:1700000000:0::u:::SC:::23::
fpr:::::::::FPR-B:
uid:u::::1700000000::HASH2::Jane <jane@work.com>::::::::::0:
";
        let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
        let entries = query_keys_for_email("jane@work.com").expect("Ok");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fingerprint, "FPR-B");
    }

    #[test]
    #[serial]
    fn apply_invokes_gpg_gen_key_when_no_matching_keys() {
        // Empty stdout for every call: query → no keys; gen-key → success;
        // post-gen query → no keys (apply prints a warning but returns Ok).
        let s = ToolShim::install(SHIM_ENV, 0, "", "");
        let p = test_printer();
        let desired: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: work-signing
  type: ed25519
  realName: "Jane Doe"
  email: jane@work.com
  expiry: 2y
  usage: sign
"#,
        )
        .unwrap();

        GpgKeysConfigurator.apply(&desired, &p).expect("Ok");

        let argv = s.argv_log();
        assert!(
            argv.contains("--list-keys --with-colons --with-fingerprint jane@work.com"),
            "expected initial list-keys, got argv: {argv}"
        );
        assert!(
            argv.lines()
                .any(|l| { l.starts_with("--batch --gen-key ") && l.contains("cfgd-gpg-") }),
            "expected --batch --gen-key with a cfgd-gpg-* param path, got argv: {argv}"
        );
        // 3 calls: initial query, gen-key, post-gen query
        assert_eq!(s.invocation_count(), 3, "argv: {argv}");
    }

    #[test]
    #[serial]
    fn apply_returns_error_when_gen_key_exits_nonzero() {
        // Shim exits non-zero on every call → both the initial query and the
        // gen-key invocation see the failure. Initial query at exit 1 is
        // already an error path (query returns Err for any non-zero/!=2).
        let _s = ToolShim::install(SHIM_ENV, 1, "", "gpg: agent unavailable");
        let p = test_printer();
        let desired: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: work-signing
  type: ed25519
  realName: "Jane Doe"
  email: jane@work.com
  expiry: 2y
  usage: sign
"#,
        )
        .unwrap();

        let err = GpgKeysConfigurator
            .apply(&desired, &p)
            .expect_err("expected gpg failure to surface");
        let msg = err.to_string();
        assert!(
            msg.contains("agent unavailable") || msg.contains("exit 1"),
            "expected gpg stderr in error, got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn diff_reports_missing_when_shim_returns_empty_keyring() {
        let _s = ToolShim::install(SHIM_ENV, 0, "", "");
        let desired: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: work-signing
  type: ed25519
  realName: "Jane Doe"
  email: jane@work.com
  expiry: 2y
  usage: sign
"#,
        )
        .unwrap();

        let drifts = GpgKeysConfigurator.diff(&desired).expect("Ok");
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "gpgKeys.work-signing.presence");
        assert!(drifts[0].actual.contains("not found"));
    }

    #[test]
    #[serial]
    fn diff_reports_no_drift_when_shim_returns_valid_unexpired_key() {
        // Far-future expiry timestamp.
        let stdout = "\
pub:u:255:22:AAAA:1700000000:9999999999::u:::SC:::23::
fpr:::::::::FPR-VALID:
uid:u::::1700000000::HASH::Jane <jane@work.com>::::::::::0:
";
        let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
        let desired: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: work-signing
  type: ed25519
  realName: "Jane Doe"
  email: jane@work.com
  expiry: 2y
  usage: sign
"#,
        )
        .unwrap();

        let drifts = GpgKeysConfigurator.diff(&desired).expect("Ok");
        assert!(
            drifts.is_empty(),
            "expected no drift, got {} entries",
            drifts.len()
        );
    }

    #[test]
    #[serial]
    fn diff_reports_expiry_when_all_matching_keys_expired() {
        // Past expiry timestamp (1700000000 = 2023-11-14, in the past)
        let stdout = "\
pub:e:255:22:AAAA:1700000000:1700000010::u:::SC:::23::
fpr:::::::::FPR-EXPIRED:
uid:e::::1700000000::HASH::Jane <jane@work.com>::::::::::0:
";
        let _s = ToolShim::install(SHIM_ENV, 0, stdout, "");
        let desired: serde_yaml::Value = serde_yaml::from_str(
            r#"
- name: work-signing
  type: ed25519
  realName: "Jane Doe"
  email: jane@work.com
  expiry: 2y
  usage: sign
"#,
        )
        .unwrap();

        let drifts = GpgKeysConfigurator.diff(&desired).expect("Ok");
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "gpgKeys.work-signing.expiry");
        assert!(drifts[0].actual.contains("FPR-EXPIRED"));
    }
}
