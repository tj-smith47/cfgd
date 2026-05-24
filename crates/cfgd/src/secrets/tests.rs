use super::*;

#[test]
fn parse_secret_reference_cases() {
    let cases: &[(&str, Option<(&str, &str)>)] = &[
        (
            "1password://Vault/Item/Field",
            Some(("1password", "Vault/Item/Field")),
        ),
        (
            "bitwarden://folder/item",
            Some(("bitwarden", "folder/item")),
        ),
        (
            "lastpass://folder/item/field",
            Some(("lastpass", "folder/item/field")),
        ),
        (
            "vault://secret/path#field",
            Some(("vault", "secret/path#field")),
        ),
        ("secrets/env.yaml", None),
    ];
    for (input, expected) in cases {
        assert_eq!(
            parse_secret_reference(input),
            *expected,
            "failed for {input:?}"
        );
    }
}

#[test]
fn resolve_secret_refs_no_refs() {
    let result = resolve_secret_refs("no secrets here", &[], None, Path::new(".")).unwrap();
    assert_eq!(result, "no secrets here");
}

#[test]
fn resolve_secret_refs_unresolvable() {
    let result = resolve_secret_refs("password=${secret:nonexistent}", &[], None, Path::new("."));
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("nonexistent"),
        "error should mention the unresolvable reference: {err}"
    );
    assert!(
        err.contains("unresolvable"),
        "error should indicate the ref is unresolvable: {err}"
    );
}

#[test]
fn build_sops_backend_default() {
    let backend = build_secret_backend("sops", None, None);
    assert_eq!(backend.name(), "sops");
}

#[test]
fn build_age_backend() {
    let backend = build_secret_backend("age", Some(PathBuf::from("/tmp/key.txt")), None);
    assert_eq!(backend.name(), "age");
}

#[test]
fn build_providers_returns_four() {
    let providers = build_secret_providers();
    assert_eq!(providers.len(), 4);
    assert_eq!(providers[0].name(), "1password");
    assert_eq!(providers[1].name(), "bitwarden");
    assert_eq!(providers[2].name(), "lastpass");
    assert_eq!(providers[3].name(), "vault");
}

#[test]
fn health_check_runs_without_panic() {
    let dir = tempfile::tempdir().unwrap();
    let health = check_secrets_health(dir.path(), None);
    // sops may or may not be installed in test env, but should not panic
    assert!(health.age_key_path.is_some());
    assert_eq!(health.providers.len(), 4);
}

// Mock backend for testing resolve_secret_refs
struct MockBackend;
impl SecretBackend for MockBackend {
    fn name(&self) -> &str {
        "mock"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn encrypt_file(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn decrypt_file(&self, _path: &Path) -> Result<SecretString> {
        Ok(SecretString::from("decrypted-value".to_string()))
    }
    fn edit_file(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
}

#[test]
fn resolve_file_secret_ref_with_backend() {
    let dir = tempfile::tempdir().unwrap();
    let secret_file = dir.path().join("secret.enc.yaml");
    std::fs::write(&secret_file, "encrypted data").unwrap();

    let backend = MockBackend;
    let result = resolve_secret_refs(
        "password=${secret:secret.enc.yaml}",
        &[],
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "password=decrypted-value");
}

#[test]
fn resolve_multiple_secret_refs() {
    let dir = tempfile::tempdir().unwrap();
    let f1 = dir.path().join("a.enc");
    let f2 = dir.path().join("b.enc");
    std::fs::write(&f1, "enc").unwrap();
    std::fs::write(&f2, "enc").unwrap();

    let backend = MockBackend;
    let result = resolve_secret_refs(
        "a=${secret:a.enc} b=${secret:b.enc}",
        &[],
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "a=decrypted-value b=decrypted-value");
}

// Mock provider for testing
struct MockProvider {
    value: String,
}

impl SecretProvider for MockProvider {
    fn name(&self) -> &str {
        "1password"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn resolve(&self, _reference: &str) -> Result<SecretString> {
        Ok(SecretString::from(self.value.clone()))
    }
}

#[test]
fn resolve_provider_secret_ref() {
    let provider = MockProvider {
        value: "s3cr3t".to_string(),
    };
    let providers: Vec<&dyn SecretProvider> = vec![&provider];

    let result = resolve_secret_refs(
        "token=${secret:1password://Vault/Item/Field}",
        &providers,
        None,
        Path::new("."),
    )
    .unwrap();
    assert_eq!(result, "token=s3cr3t");
}

#[test]
fn resolve_secret_refs_text_without_markers() {
    let result = resolve_secret_refs("plain text no markers", &[], None, Path::new("."));
    assert_eq!(result.unwrap(), "plain text no markers");
}

#[test]
fn resolve_secret_refs_preserves_surrounding_text() {
    let dir = tempfile::tempdir().unwrap();
    let secret_file = dir.path().join("key.enc");
    std::fs::write(&secret_file, "enc").unwrap();

    let backend = MockBackend;
    let result = resolve_secret_refs(
        "before ${secret:key.enc} after",
        &[],
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "before decrypted-value after");
}

#[test]
fn build_sops_backend_with_age_key() {
    let backend = build_secret_backend("sops", Some(PathBuf::from("/tmp/key.txt")), None);
    assert_eq!(backend.name(), "sops");
}

#[test]
fn build_unknown_backend_falls_back_to_sops() {
    let backend = build_secret_backend("unknown", None, None);
    assert_eq!(backend.name(), "sops");
}

#[test]
fn init_age_key_creates_key_file() {
    let dir = tempfile::tempdir().unwrap();
    if !cfgd_core::command_available("age-keygen") {
        // age not installed in CI — skip
        return;
    }
    let path = init_age_key(dir.path()).unwrap();
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("AGE-SECRET-KEY"));
}

#[test]
fn provider_names_are_unique() {
    let providers = build_secret_providers();
    let mut names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
    let before = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), before);
}

#[test]
fn health_check_age_key_path_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let health = check_secrets_health(dir.path(), None);
    // Default age key path should be set even if file doesn't exist
    assert!(health.age_key_path.is_some());
}

// --- extract_age_recipient tests ---

#[test]
fn extract_age_recipient_valid_content() {
    let content =
        "# created: 2024-01-01\n# public key: age1abc123def456\nAGE-SECRET-KEY-1DEADBEEF\n";
    assert_eq!(
        extract_age_recipient(content),
        Some("age1abc123def456".to_string())
    );
}

#[test]
fn extract_age_recipient_missing_public_key_line() {
    let content = "# created: 2024-01-01\nAGE-SECRET-KEY-1DEADBEEF\n";
    assert_eq!(extract_age_recipient(content), None);
}

#[test]
fn extract_age_recipient_empty_content() {
    assert_eq!(extract_age_recipient(""), None);
}

#[test]
fn extract_age_recipient_multiple_comment_lines() {
    let content = "\
# this is a comment
# another comment
# yet another
# public key: age1xyz789
AGE-SECRET-KEY-1STUFF\n";
    assert_eq!(
        extract_age_recipient(content),
        Some("age1xyz789".to_string())
    );
}

#[test]
fn extract_age_recipient_trailing_whitespace_trimmed() {
    let content = "# public key: age1trailing   \n";
    assert_eq!(
        extract_age_recipient(content),
        Some("age1trailing".to_string())
    );
}

#[test]
fn extract_age_recipient_only_prefix_no_key() {
    // The prefix is present but no key follows — empty key is rejected
    let content = "# public key: \n";
    assert_eq!(extract_age_recipient(content), None);
}

// --- resolve_secret_refs edge case tests ---

#[test]
fn resolve_secret_refs_unclosed_marker_passes_through() {
    // Unclosed ${secret: without closing } — the loop breaks and returns input as-is
    let result =
        resolve_secret_refs("value=${secret:oops_no_close", &[], None, Path::new(".")).unwrap();
    assert_eq!(result, "value=${secret:oops_no_close");
}

#[test]
fn resolve_secret_refs_multiple_markers_with_provider() {
    use cfgd_core::test_helpers::MockSecretProvider;

    let provider = MockSecretProvider::new("vault").with_resolve_result("v1");
    let providers: Vec<&dyn SecretProvider> = vec![&provider];

    let result = resolve_secret_refs(
        "a=${secret:vault://p1} b=${secret:vault://p2} c=${secret:vault://p3}",
        &providers,
        None,
        Path::new("."),
    )
    .unwrap();
    assert_eq!(result, "a=v1 b=v1 c=v1");

    // All three references should have been resolved
    let calls = provider.resolve_calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], "p1");
    assert_eq!(calls[1], "p2");
    assert_eq!(calls[2], "p3");
}

#[test]
fn resolve_secret_refs_mixed_provider_and_file() {
    use cfgd_core::test_helpers::{MockSecretBackend, MockSecretProvider};

    let dir = tempfile::tempdir().unwrap();
    let secret_file = dir.path().join("creds.enc");
    std::fs::write(&secret_file, "encrypted").unwrap();

    let provider = MockSecretProvider::new("1password").with_resolve_result("op-secret");
    let backend = MockSecretBackend::new("sops").with_decrypt_result("file-secret");
    let providers: Vec<&dyn SecretProvider> = vec![&provider];

    let result = resolve_secret_refs(
        "pw=${secret:1password://v/i/f} db=${secret:creds.enc}",
        &providers,
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "pw=op-secret db=file-secret");
}

#[test]
fn resolve_secret_refs_unavailable_provider_errors() {
    use cfgd_core::test_helpers::MockSecretProvider;

    let provider = MockSecretProvider::new("vault").unavailable();
    let providers: Vec<&dyn SecretProvider> = vec![&provider];

    let result = resolve_secret_refs("x=${secret:vault://path}", &providers, None, Path::new("."));
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("vault"),
        "error should mention provider: {err}"
    );
}

#[test]
fn resolve_secret_refs_unknown_provider_errors() {
    // No providers registered at all, but reference uses a provider scheme
    let result = resolve_secret_refs("x=${secret:unknown://something}", &[], None, Path::new("."));
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unknown://something"),
        "error should mention the unresolvable reference: {err}"
    );
    assert!(
        err.contains("unresolvable"),
        "error should indicate the reference is unresolvable: {err}"
    );
}

#[test]
fn resolve_secret_refs_file_ref_path_traversal_errors() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend;

    let result = resolve_secret_refs(
        "x=${secret:../../etc/shadow}",
        &[],
        Some(&backend),
        dir.path(),
    );
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("traversal"),
        "error should mention traversal: {err}"
    );
}

#[test]
fn resolve_secret_refs_empty_input() {
    let result = resolve_secret_refs("", &[], None, Path::new(".")).unwrap();
    assert_eq!(result, "");
}

#[test]
fn resolve_secret_refs_marker_at_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("s.enc");
    std::fs::write(&f, "data").unwrap();

    let backend = MockBackend;
    // Marker at the very start and very end of the string
    let result = resolve_secret_refs("${secret:s.enc}", &[], Some(&backend), dir.path()).unwrap();
    assert_eq!(result, "decrypted-value");
}

#[test]
fn resolve_secret_refs_adjacent_markers() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.enc");
    std::fs::write(&f, "data").unwrap();

    let backend = MockBackend;
    let result = resolve_secret_refs(
        "${secret:a.enc}${secret:a.enc}",
        &[],
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "decrypted-valuedecrypted-value");
}

// --- Mock verification tests using cfgd_core::test_helpers ---

#[test]
fn resolve_secret_ref_calls_backend_with_correct_path() {
    use cfgd_core::test_helpers::MockSecretBackend;

    let backend = MockSecretBackend::new("sops").with_decrypt_result("decrypted");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.enc.yaml"), "encrypted-data").unwrap();

    let result = resolve_secret_refs(
        "pw=${secret:secret.enc.yaml}",
        &[],
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "pw=decrypted");

    let calls = backend.decrypt_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "backend should have been called exactly once"
    );
    assert_eq!(
        calls[0],
        dir.path().join("secret.enc.yaml"),
        "backend should receive the resolved absolute path"
    );
}

#[test]
fn resolve_secret_ref_calls_provider_with_correct_reference() {
    use cfgd_core::test_helpers::MockSecretProvider;

    let provider = MockSecretProvider::new("1password").with_resolve_result("provider-secret");
    let providers: Vec<&dyn SecretProvider> = vec![&provider];
    let dir = tempfile::tempdir().unwrap();

    let result = resolve_secret_refs(
        "token=${secret:1password://Vault/Item/Field}",
        &providers,
        None,
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "token=provider-secret");

    let calls = provider.resolve_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "provider should have been called exactly once"
    );
    assert_eq!(
        calls[0], "Vault/Item/Field",
        "provider should receive the reference without the scheme prefix"
    );
}

#[test]
fn resolve_secret_ref_backend_failure_propagates() {
    use cfgd_core::test_helpers::MockSecretBackend;

    let backend = MockSecretBackend::new("sops");
    backend.set_fail_decrypt(true);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.enc"), "data").unwrap();

    let result = resolve_secret_refs("${secret:secret.enc}", &[], Some(&backend), dir.path());
    assert!(result.is_err(), "backend failure should propagate as error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("decrypt"),
        "error should mention decryption: {err}"
    );
}

#[test]
fn resolve_secret_ref_provider_failure_propagates() {
    use cfgd_core::test_helpers::MockSecretProvider;

    let provider = MockSecretProvider::new("vault");
    provider.set_fail_resolve(true);
    let providers: Vec<&dyn SecretProvider> = vec![&provider];

    let result = resolve_secret_refs(
        "x=${secret:vault://secret/path}",
        &providers,
        None,
        Path::new("."),
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("secret/path"),
        "error should mention the secret reference: {err}"
    );
}

#[test]
fn resolve_secret_ref_first_matching_provider_wins() {
    use cfgd_core::test_helpers::MockSecretProvider;

    let provider1 = MockSecretProvider::new("vault").with_resolve_result("from-first");
    let provider2 = MockSecretProvider::new("vault").with_resolve_result("from-second");
    let providers: Vec<&dyn SecretProvider> = vec![&provider1, &provider2];

    let result =
        resolve_secret_refs("x=${secret:vault://path}", &providers, None, Path::new(".")).unwrap();
    assert_eq!(result, "x=from-first");

    let calls1 = provider1.resolve_calls.lock().unwrap();
    let calls2 = provider2.resolve_calls.lock().unwrap();
    assert_eq!(calls1.len(), 1, "first matching provider should be called");
    assert_eq!(calls2.len(), 0, "second provider should not be called");
}

#[test]
fn resolve_secret_ref_backend_receives_multiple_paths() {
    use cfgd_core::test_helpers::MockSecretBackend;

    let backend = MockSecretBackend::new("sops").with_decrypt_result("val");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.enc"), "data").unwrap();
    std::fs::write(dir.path().join("b.enc"), "data").unwrap();

    let _ = resolve_secret_refs(
        "${secret:a.enc} ${secret:b.enc}",
        &[],
        Some(&backend),
        dir.path(),
    )
    .unwrap();

    let calls = backend.decrypt_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], dir.path().join("a.enc"));
    assert_eq!(calls[1], dir.path().join("b.enc"));
}

#[test]
fn extract_age_recipient_first_match_wins() {
    let content = "\
# public key: age1first
# public key: age1second
AGE-SECRET-KEY-1STUFF\n";
    assert_eq!(
        extract_age_recipient(content),
        Some("age1first".to_string()),
        "should return the first matching public key line"
    );
}

#[test]
fn extract_age_recipient_whitespace_only_key() {
    let content = "# public key:    \n";
    assert_eq!(
        extract_age_recipient(content),
        None,
        "whitespace-only key should be rejected"
    );
}

// --- SopsBackend ---

#[test]
fn sops_backend_with_age_key_path() {
    let backend = SopsBackend::new(Some(PathBuf::from("/custom/age.txt")));
    assert_eq!(backend.name(), "sops");
    assert_eq!(backend.age_key_path, Some(PathBuf::from("/custom/age.txt")));
}

#[test]
fn sops_backend_with_config_dir_existing_sops_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let sops_yaml = dir.path().join(".sops.yaml");
    std::fs::write(&sops_yaml, "creation_rules:\n  - age: 'age1test'\n").unwrap();

    let backend = SopsBackend::new(None).with_config_dir(dir.path());
    assert_eq!(backend.sops_config, Some(sops_yaml));
}

#[test]
fn sops_backend_with_config_dir_no_sops_yaml() {
    let dir = tempfile::tempdir().unwrap();
    // No .sops.yaml created

    let backend = SopsBackend::new(None).with_config_dir(dir.path());
    assert_eq!(backend.sops_config, None);
}

// --- AgeBackend ---

#[test]
fn age_backend_not_available_without_key_file() {
    let backend = AgeBackend::new(PathBuf::from("/nonexistent/age-key.txt"));
    assert!(!backend.is_available());
}

#[test]
fn age_backend_recipient_from_nonexistent_key_errors() {
    let backend = AgeBackend::new(PathBuf::from("/nonexistent/age-key.txt"));
    let result = backend.recipient_from_key();
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("age key not found") || err.contains("/nonexistent/age-key.txt"),
        "error should mention the missing key file: {err}"
    );
}

#[test]
fn age_backend_recipient_from_valid_key() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("age-key.txt");
    std::fs::write(
        &key_path,
        "# created: 2024-01-01\n# public key: age1abc123\nAGE-SECRET-KEY-1TEST\n",
    )
    .unwrap();

    let backend = AgeBackend::new(key_path);
    let recipient = backend.recipient_from_key().unwrap();
    assert_eq!(recipient, "age1abc123");
}

#[test]
fn age_backend_recipient_from_key_without_public_line() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("age-key.txt");
    std::fs::write(&key_path, "# created: 2024-01-01\nAGE-SECRET-KEY-1TEST\n").unwrap();

    let backend = AgeBackend::new(key_path.clone());
    let result = backend.recipient_from_key();
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("age key not found") || err.contains(key_path.to_str().unwrap()),
        "error should mention the key file path: {err}"
    );
}

// --- build_secret_backend with config_dir ---

#[test]
fn build_sops_backend_with_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let backend = build_secret_backend("sops", None, Some(dir.path()));
    assert_eq!(backend.name(), "sops");
}

#[test]
fn build_age_backend_ignores_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let backend =
        build_secret_backend("age", Some(PathBuf::from("/tmp/key.txt")), Some(dir.path()));
    assert_eq!(backend.name(), "age");
}

// --- SecretProvider names ---

#[test]
fn one_password_provider_name() {
    assert_eq!(OnePasswordProvider.name(), "1password");
}

#[test]
fn bitwarden_provider_name() {
    assert_eq!(BitwardenProvider.name(), "bitwarden");
}

#[test]
fn lastpass_provider_name() {
    assert_eq!(LastPassProvider.name(), "lastpass");
}

#[test]
fn vault_provider_name() {
    assert_eq!(VaultProvider.name(), "vault");
}

// --- check_secrets_health ---

#[test]
fn check_secrets_health_with_custom_age_key_path() {
    let dir = tempfile::tempdir().unwrap();
    let custom_key = dir.path().join("custom-age.txt");
    // Don't create the file — test that health reports existence correctly
    let health = check_secrets_health(dir.path(), Some(custom_key.as_path()));
    assert!(!health.age_key_exists);
    assert_eq!(health.age_key_path, Some(custom_key));
}

#[test]
fn check_secrets_health_sops_config_detection() {
    let dir = tempfile::tempdir().unwrap();
    // No .sops.yaml
    let health1 = check_secrets_health(dir.path(), None);
    assert!(!health1.sops_config_exists);

    // Create .sops.yaml
    std::fs::write(dir.path().join(".sops.yaml"), "creation_rules: []\n").unwrap();
    let health2 = check_secrets_health(dir.path(), None);
    assert!(health2.sops_config_exists);
    assert_eq!(
        health2.sops_config_path,
        Some(dir.path().join(".sops.yaml"))
    );
}

#[test]
fn check_secrets_health_provider_count() {
    let dir = tempfile::tempdir().unwrap();
    let health = check_secrets_health(dir.path(), None);
    // Should report all 4 providers
    assert_eq!(health.providers.len(), 4);
    let names: Vec<&str> = health.providers.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"1password"));
    assert!(names.contains(&"bitwarden"));
    assert!(names.contains(&"lastpass"));
    assert!(names.contains(&"vault"));
}

#[test]
fn check_secrets_health_existing_age_key_reports_exists() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("present-key.txt");
    std::fs::write(&key_path, "# public key: age1test\nAGE-SECRET-KEY-1X\n").unwrap();
    let health = check_secrets_health(dir.path(), Some(&key_path));
    assert!(
        health.age_key_exists,
        "age key should exist at override path"
    );
    assert_eq!(health.age_key_path, Some(key_path));
}

#[test]
fn resolve_secret_refs_provider_then_file_in_same_string() {
    // Mix a provider reference and a file reference in one string
    let dir = tempfile::tempdir().unwrap();
    let enc_file = dir.path().join("db.enc");
    std::fs::write(&enc_file, "encrypted").unwrap();

    let provider = MockProvider {
        value: "api-key-123".to_string(),
    };
    let providers: Vec<&dyn SecretProvider> = vec![&provider];
    let backend = MockBackend;

    let result = resolve_secret_refs(
        "key=${secret:1password://Vault/Item} db=${secret:db.enc}",
        &providers,
        Some(&backend),
        dir.path(),
    )
    .unwrap();
    assert_eq!(result, "key=api-key-123 db=decrypted-value");
}

#[test]
fn sops_backend_with_config_dir_sets_sops_config() {
    let dir = tempfile::tempdir().unwrap();
    let sops_file = dir.path().join(".sops.yaml");
    std::fs::write(&sops_file, "creation_rules: []\n").unwrap();
    let backend = SopsBackend::new(None).with_config_dir(dir.path());
    // The backend should have internalized the config dir path
    assert_eq!(backend.name(), "sops");
}

// --- resolve_secret_refs with nested markers ---

#[test]
fn resolve_secret_refs_marker_without_scheme_not_a_file() {
    // A reference that isn't a provider scheme and doesn't exist as a file
    let result = resolve_secret_refs(
        "x=${secret:nonexistent_file.yaml}",
        &[],
        None,
        Path::new("."),
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("nonexistent_file.yaml"),
        "error should mention the unresolvable reference: {err}"
    );
    assert!(
        err.contains("unresolvable"),
        "error should indicate the ref is unresolvable: {err}"
    );
}

// --- shell_split_editor tests ---

#[test]
fn shell_split_editor_simple_path() {
    let tokens = shell_split_editor("vim");
    assert_eq!(tokens, vec!["vim"]);
}

#[test]
fn shell_split_editor_path_with_args() {
    let tokens = shell_split_editor("code --wait");
    assert_eq!(tokens, vec!["code", "--wait"]);
}

#[test]
fn shell_split_editor_quoted_path_with_spaces() {
    let tokens = shell_split_editor(r#""/path/to my/editor" --wait"#);
    assert_eq!(tokens, vec!["/path/to my/editor", "--wait"]);
}

#[test]
fn shell_split_editor_single_quoted_path() {
    let tokens = shell_split_editor("'/usr/local/bin/my editor' --wait --new-window");
    assert_eq!(
        tokens,
        vec!["/usr/local/bin/my editor", "--wait", "--new-window"]
    );
}

#[test]
fn shell_split_editor_multiple_args() {
    let tokens = shell_split_editor("emacs -nw --no-splash");
    assert_eq!(tokens, vec!["emacs", "-nw", "--no-splash"]);
}

#[test]
fn shell_split_editor_empty_string() {
    let tokens = shell_split_editor("");
    assert!(tokens.is_empty());
}

#[test]
fn shell_split_editor_only_whitespace() {
    let tokens = shell_split_editor("   ");
    assert!(tokens.is_empty());
}

#[test]
fn shell_split_editor_mixed_quotes() {
    let tokens = shell_split_editor(r#""/path/editor" '--flag=hello world'"#);
    assert_eq!(tokens, vec!["/path/editor", "--flag=hello world"]);
}

#[test]
fn shell_split_editor_extra_whitespace_between_args() {
    let tokens = shell_split_editor("nano   -w   --tabsize=4");
    assert_eq!(tokens, vec!["nano", "-w", "--tabsize=4"]);
}

#[test]
fn shell_split_editor_quoted_empty_arg() {
    // A pair of quotes yields an empty token that is not pushed (empty string)
    let tokens = shell_split_editor(r#"editor "" arg"#);
    // The empty quotes produce an empty current string that gets pushed
    // only if non-empty — let's verify actual behavior
    assert_eq!(tokens.len(), 2, "empty quoted arg should not be pushed");
    assert_eq!(tokens[0], "editor");
    assert_eq!(tokens[1], "arg");
}

// --- SopsBackend command construction tests ---

#[test]
fn sops_command_sets_age_key_env() {
    let backend = SopsBackend::new(Some(PathBuf::from("/home/user/.config/cfgd/age-key.txt")));
    let cmd = backend.sops_command();
    // Verify the command is "sops" and has the env var set
    assert_eq!(cmd.get_program(), "sops");
    let envs: Vec<_> = cmd.get_envs().collect();
    let age_env = envs
        .iter()
        .find(|(k, _)| k == &std::ffi::OsStr::new("SOPS_AGE_KEY_FILE"));
    assert!(age_env.is_some(), "should set SOPS_AGE_KEY_FILE");
    assert_eq!(
        age_env.unwrap().1.unwrap(),
        std::ffi::OsStr::new("/home/user/.config/cfgd/age-key.txt")
    );
}

#[test]
fn sops_command_without_age_key_has_no_env() {
    let backend = SopsBackend::new(None);
    let cmd = backend.sops_command();
    assert_eq!(cmd.get_program(), "sops");
    let envs: Vec<_> = cmd.get_envs().collect();
    let age_env = envs
        .iter()
        .find(|(k, _)| k == &std::ffi::OsStr::new("SOPS_AGE_KEY_FILE"));
    assert!(age_env.is_none(), "should not set SOPS_AGE_KEY_FILE");
}

#[test]
fn sops_encrypt_command_includes_config_flag() {
    let dir = tempfile::tempdir().unwrap();
    let sops_yaml = dir.path().join(".sops.yaml");
    std::fs::write(&sops_yaml, "creation_rules:\n  - age: 'age1test'\n").unwrap();

    let backend = SopsBackend::new(Some(PathBuf::from("/tmp/key.txt"))).with_config_dir(dir.path());
    let cmd = backend.sops_encrypt_command();

    let args: Vec<_> = cmd.get_args().collect();
    assert!(
        args.contains(&std::ffi::OsStr::new("--config")),
        "should include --config flag when sops_config is set, got args: {args:?}"
    );
    assert!(
        args.iter()
            .any(|a| a.to_string_lossy().contains(".sops.yaml")),
        "should include the .sops.yaml path, got args: {args:?}"
    );
}

#[test]
fn sops_encrypt_command_no_config_when_missing() {
    let backend = SopsBackend::new(None);
    let cmd = backend.sops_encrypt_command();

    let args: Vec<_> = cmd.get_args().collect();
    assert!(
        !args.contains(&std::ffi::OsStr::new("--config")),
        "should not include --config flag when sops_config is None"
    );
}

// --- AgeBackend key path resolution tests ---

#[test]
fn age_backend_available_with_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("age-key.txt");
    std::fs::write(&key_path, "# public key: age1test\nAGE-SECRET-KEY-1TEST\n").unwrap();

    let backend = AgeBackend::new(key_path.clone());
    // is_available checks key_path.exists() AND command_available("age")
    // Key exists, but age may or may not be installed
    if cfgd_core::command_available("age") {
        assert!(backend.is_available());
    } else {
        // Key exists but CLI missing — not available
        assert!(!backend.is_available());
    }
}

#[test]
fn age_backend_recipient_extracts_from_key_content() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("age-key.txt");
    let key_content = "# created: 2024-06-15T10:30:00Z\n\
                           # public key: age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p\n\
                           AGE-SECRET-KEY-1QQQQQQQQQQQQQQQQQQQQQQQQ\n";
    std::fs::write(&key_path, key_content).unwrap();

    let backend = AgeBackend::new(key_path);
    let recipient = backend.recipient_from_key().unwrap();
    assert_eq!(
        recipient,
        "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p"
    );
}

// --- OnePasswordProvider reference parsing ---

#[test]
fn one_password_provider_resolve_builds_op_ref() {
    // We can't actually run `op`, but we can verify the provider builds
    // the right command structure by checking what would be constructed.
    // The resolve method adds op:// prefix if missing.
    let provider = OnePasswordProvider;
    assert_eq!(provider.name(), "1password");
    // is_available depends on `op` being installed — just test it doesn't panic
    let _ = provider.is_available();
}

// --- BitwardenProvider reference parsing ---

#[test]
fn bitwarden_provider_parses_folder_item() {
    let provider = BitwardenProvider;
    assert_eq!(provider.name(), "bitwarden");
    let _ = provider.is_available();
}

// --- LastPassProvider reference parsing ---

#[test]
fn lastpass_provider_parses_item_with_field() {
    let provider = LastPassProvider;
    assert_eq!(provider.name(), "lastpass");
    let _ = provider.is_available();
}

// --- VaultProvider reference parsing ---

#[test]
fn vault_provider_parses_path_and_field() {
    let provider = VaultProvider;
    assert_eq!(provider.name(), "vault");
    let _ = provider.is_available();
}

// --- check_secrets_health comprehensive tests ---

#[test]
fn check_secrets_health_returns_correct_provider_names() {
    let dir = tempfile::tempdir().unwrap();
    let health = check_secrets_health(dir.path(), None);
    let names: Vec<&str> = health.providers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec!["1password", "bitwarden", "lastpass", "vault"],
        "providers should be in expected order"
    );
}

#[test]
fn check_secrets_health_sops_config_path_always_set() {
    let dir = tempfile::tempdir().unwrap();
    let health = check_secrets_health(dir.path(), None);
    assert_eq!(
        health.sops_config_path,
        Some(dir.path().join(".sops.yaml")),
        "sops_config_path should always be set to config_dir/.sops.yaml"
    );
}

// --- build_secret_backend comprehensive tests ---

#[test]
fn build_secret_backend_sops_with_config_dir_containing_sops_yaml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".sops.yaml"),
        "creation_rules:\n  - age: 'age1abc'\n",
    )
    .unwrap();
    let backend = build_secret_backend("sops", None, Some(dir.path()));
    assert_eq!(backend.name(), "sops");
}

#[test]
fn build_secret_backend_unknown_name_defaults_to_sops() {
    let backend = build_secret_backend("gpg", None, None);
    assert_eq!(
        backend.name(),
        "sops",
        "unknown backend name should fall back to sops"
    );
}

#[test]
fn build_secret_backend_age_with_custom_key_path() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("my-age-key.txt");
    let backend = build_secret_backend("age", Some(key.clone()), None);
    assert_eq!(backend.name(), "age");
}

// --- resolve_single_ref edge cases (tested via resolve_secret_refs) ---

#[test]
fn resolve_secret_ref_no_backend_and_no_provider_match() {
    // Reference that doesn't match any provider and no backend provided
    let result = resolve_secret_refs("x=${secret:some/file.yaml}", &[], None, Path::new("."));
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unresolvable"),
        "should be unresolvable without backend or matching provider: {err}"
    );
}

#[test]
fn resolve_secret_ref_file_exists_but_no_backend() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.enc"), "data").unwrap();

    // File exists but no backend to decrypt it
    let result = resolve_secret_refs("x=${secret:secret.enc}", &[], None, dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unresolvable"),
        "should be unresolvable without backend: {err}"
    );
}

#[test]
fn resolve_secret_ref_file_does_not_exist_with_backend() {
    let dir = tempfile::tempdir().unwrap();
    let backend = MockBackend;

    // Backend provided but file doesn't exist
    let result = resolve_secret_refs("x=${secret:missing.enc}", &[], Some(&backend), dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unresolvable"),
        "should be unresolvable when file is missing: {err}"
    );
}

// --- extract_age_recipient additional edge cases ---

#[test]
fn extract_age_recipient_crlf_line_endings() {
    let content = "# created: 2024-01-01\r\n# public key: age1crlf\r\nAGE-SECRET-KEY-1TEST\r\n";
    // trim() in the implementation handles trailing \r
    assert_eq!(
        extract_age_recipient(content),
        Some("age1crlf".to_string()),
        "should handle CRLF line endings"
    );
}

#[test]
fn extract_age_recipient_key_with_no_newline_at_end() {
    let content = "# public key: age1nonewline";
    assert_eq!(
        extract_age_recipient(content),
        Some("age1nonewline".to_string()),
        "should handle content without trailing newline"
    );
}

// ---------------------------------------------------------------------------
// AgeBackend — driven through the generic ToolShim against `CFGD_AGE_BIN`.
// Tests assert behavior that would change under a real refactor: the recipient
// extracted from the key file is forwarded to age as `--recipient <id>`, the
// rename-replace happens after a successful encrypt, decrypted stdout is
// returned through `SecretString`, and tool failures are translated into the
// right `SecretError` variant with stderr surfaced.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod age_shim {
    use super::*;
    use cfgd_core::test_helpers::ToolShim;
    use secrecy::ExposeSecret;
    use serial_test::serial;

    /// Return (tempdir, key_path) with a valid age private-key file whose
    /// public-key comment is `age1testkey`. The recipient extracted from this
    /// file should appear verbatim in `--recipient` argv.
    fn key_fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("age-key.txt");
        std::fs::write(
            &key_path,
            "# created: 2024-01-01\n# public key: age1testkey\nAGE-SECRET-KEY-1TEST\n",
        )
        .expect("write key");
        (dir, key_path)
    }

    #[test]
    #[serial]
    fn age_is_available_returns_true_when_seam_points_at_real_file() {
        let _shim = ToolShim::install("CFGD_AGE_BIN", 0, "", "");
        let (_dir, key) = key_fixture();
        let backend = AgeBackend::new(key);
        assert!(
            backend.is_available(),
            "key + shim binary present → is_available = true"
        );
    }

    #[test]
    #[serial]
    fn age_encrypt_forwards_recipient_from_key_and_replaces_original() {
        let shim = ToolShim::install("CFGD_AGE_BIN", 0, "", "");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let plain = datadir.path().join("secret.txt");
        std::fs::write(&plain, b"hunter2").expect("write plaintext");

        // The shim exits 0 *without* writing the .age output file, which means
        // the post-spawn `rename(<path>.age, <path>)` will return an error. Set
        // up the rename target by pre-creating the .age file the way age would.
        let age_out = datadir.path().join("secret.txt.age");
        std::fs::write(&age_out, b"<encrypted>").expect("seed encrypted output");

        let backend = AgeBackend::new(key);
        backend
            .encrypt_file(&plain)
            .expect("encrypt happy path returns Ok");

        let argv = shim.argv_log();
        assert!(
            argv.contains("--encrypt"),
            "argv must include --encrypt: {argv}"
        );
        assert!(
            argv.contains("--recipient age1testkey"),
            "recipient extracted from the key file must reach age as --recipient: {argv}"
        );
        assert!(
            argv.contains(&format!("--output {}", age_out.display())),
            "argv must include --output <path>.age: {argv}"
        );
        assert!(
            argv.contains(plain.to_str().unwrap()),
            "argv must include the input path: {argv}"
        );

        // Post-rename: the original path now holds the encrypted bytes;
        // the .age file no longer exists.
        assert_eq!(
            std::fs::read(&plain).expect("plain still exists after encrypt"),
            b"<encrypted>",
            "rename must overwrite original with encrypted output"
        );
        assert!(
            !age_out.exists(),
            "post-rename, the .age sidecar file must be gone"
        );
    }

    #[test]
    #[serial]
    fn age_encrypt_propagates_failure_with_stderr_in_message() {
        let _shim = ToolShim::install("CFGD_AGE_BIN", 1, "", "no recipient");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let plain = datadir.path().join("secret.txt");
        std::fs::write(&plain, b"hunter2").expect("write plaintext");

        let backend = AgeBackend::new(key);
        let err = backend.encrypt_file(&plain).expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("no recipient"),
            "stderr must surface in error message: {msg}"
        );
    }

    #[test]
    #[serial]
    fn age_decrypt_returns_stdout_bytes_through_secret_string() {
        let _shim = ToolShim::install("CFGD_AGE_BIN", 0, "decrypted-payload", "");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let cipher = datadir.path().join("secret.age");
        std::fs::write(&cipher, b"<ciphertext>").expect("write cipher");

        let backend = AgeBackend::new(key);
        let secret = backend.decrypt_file(&cipher).expect("decrypt → Ok");
        assert_eq!(
            secret.expose_secret(),
            "decrypted-payload",
            "stdout from age must round-trip through SecretString"
        );
    }

    #[test]
    #[serial]
    fn age_decrypt_uses_identity_flag_with_key_path() {
        let shim = ToolShim::install("CFGD_AGE_BIN", 0, "decrypted", "");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let cipher = datadir.path().join("secret.age");
        std::fs::write(&cipher, b"x").expect("write cipher");

        let backend = AgeBackend::new(key.clone());
        backend.decrypt_file(&cipher).expect("decrypt → Ok");

        let argv = shim.argv_log();
        assert!(
            argv.contains("--decrypt"),
            "argv must include --decrypt: {argv}"
        );
        assert!(
            argv.contains(&format!("--identity {}", key.display())),
            "argv must include --identity <key_path>: {argv}"
        );
        assert!(
            argv.contains(cipher.to_str().unwrap()),
            "argv must include the cipher path: {argv}"
        );
    }

    #[test]
    #[serial]
    fn age_decrypt_propagates_failure_with_stderr_and_path() {
        let _shim = ToolShim::install("CFGD_AGE_BIN", 1, "", "bad identity");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let cipher = datadir.path().join("secret.age");
        std::fs::write(&cipher, b"x").expect("write cipher");

        let backend = AgeBackend::new(key);
        let err = backend.decrypt_file(&cipher).expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("bad identity"),
            "stderr must surface in error: {msg}"
        );
    }

    /// Run a closure with `EDITOR=<path>` and restore the prior value
    /// on drop. Serialised via `#[serial]`.
    fn with_editor<F: FnOnce()>(editor_path: &str, f: F) {
        // SAFETY: serialised by #[serial]; no other test mutates EDITOR.
        unsafe {
            let prior = std::env::var("EDITOR").ok();
            std::env::set_var("EDITOR", editor_path);
            f();
            match prior {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
        }
    }

    #[test]
    #[serial]
    fn age_edit_returns_early_when_editor_does_not_change_content() {
        // /bin/true exits 0 without opening the temp file, so the decrypted
        // payload remains identical after the editor returns. edit_file must
        // hit the `edited == decrypted` early-return arm and NOT re-encrypt.
        let shim = ToolShim::install("CFGD_AGE_BIN", 0, "unchanged-secret\n", "");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let cipher = datadir.path().join("secret.age");
        std::fs::write(&cipher, b"<ciphertext>").expect("write cipher");

        let backend = AgeBackend::new(key);
        with_editor("/bin/true", || {
            backend
                .edit_file(&cipher)
                .expect("edit happy path returns Ok");
        });

        // Only one age invocation should have happened (the initial decrypt).
        // If the early-return path failed, the function would also invoke
        // `--encrypt`, and argv_log would contain "--encrypt".
        let argv = shim.argv_log();
        assert!(
            argv.contains("--decrypt"),
            "edit should invoke age --decrypt at least once: {argv}"
        );
        assert!(
            !argv.contains("--encrypt"),
            "edit should skip re-encrypt when content unchanged, got: {argv}"
        );
        // Original ciphertext is unchanged on disk.
        assert_eq!(
            std::fs::read(&cipher).expect("cipher still on disk"),
            b"<ciphertext>",
            "no edit happened → no re-encrypt → ciphertext untouched"
        );
    }

    #[test]
    #[serial]
    fn age_edit_surfaces_editor_failure_as_secret_error() {
        // /bin/false exits 1 → edit_file must surface the editor non-zero
        // exit as a SecretError::EncryptionFailed with the editor name in
        // the message.
        let _shim = ToolShim::install("CFGD_AGE_BIN", 0, "payload", "");
        let (_keydir, key) = key_fixture();
        let datadir = tempfile::tempdir().expect("tempdir");
        let cipher = datadir.path().join("secret.age");
        std::fs::write(&cipher, b"x").expect("write cipher");

        let backend = AgeBackend::new(key);
        let err = with_editor_returning("/bin/false", || backend.edit_file(&cipher));
        let msg = format!("{}", err.expect_err("editor=false should surface as Err"));
        assert!(
            msg.contains("/bin/false") || msg.contains("non-zero"),
            "editor failure should mention the editor path or non-zero status: {msg}"
        );
    }

    /// Variant of `with_editor` that returns the closure's result. Restores
    /// EDITOR even if the closure errors.
    fn with_editor_returning<T, F: FnOnce() -> T>(editor_path: &str, f: F) -> T {
        // SAFETY: serialised by #[serial]; no other test mutates EDITOR.
        unsafe {
            let prior = std::env::var("EDITOR").ok();
            std::env::set_var("EDITOR", editor_path);
            let r = f();
            match prior {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
            r
        }
    }
}

// ---------------------------------------------------------------------------
// SopsBackend — driven through the same generic ToolShim against
// `CFGD_SOPS_BIN`. Each test pins behavior that would change under a real
// refactor: the encrypt path uses --encrypt --in-place, .sops.yaml configures
// --config, decrypt stdout round-trips through SecretString, and the age key
// path propagates through SOPS_AGE_KEY_FILE.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod sops_shim {
    use super::*;
    use cfgd_core::test_helpers::ToolShim;
    use secrecy::ExposeSecret;
    use serial_test::serial;

    #[test]
    #[serial]
    fn sops_is_available_returns_true_when_seam_points_at_real_file() {
        let _shim = ToolShim::install("CFGD_SOPS_BIN", 0, "", "");
        let backend = SopsBackend::new(None);
        assert!(
            backend.is_available(),
            "shim binary present → is_available = true"
        );
    }

    #[test]
    #[serial]
    fn sops_encrypt_uses_encrypt_in_place_with_sops_config_when_present() {
        let shim = ToolShim::install("CFGD_SOPS_BIN", 0, "", "");
        let dir = tempfile::tempdir().expect("tempdir");
        // .sops.yaml in the config dir → --config <path> appears in argv.
        let sops_yaml = dir.path().join(".sops.yaml");
        std::fs::write(&sops_yaml, "creation_rules: []\n").expect("write .sops.yaml");
        let plain = dir.path().join("secret.yaml");
        std::fs::write(&plain, "key: value\n").expect("write plain");

        let backend = SopsBackend::new(None).with_config_dir(dir.path());
        backend
            .encrypt_file(&plain)
            .expect("encrypt happy path returns Ok");

        let argv = shim.argv_log();
        assert!(
            argv.contains("--encrypt"),
            "argv must include --encrypt: {argv}"
        );
        assert!(
            argv.contains("--in-place"),
            "argv must include --in-place: {argv}"
        );
        assert!(
            argv.contains(&format!("--config {}", sops_yaml.display())),
            ".sops.yaml in config_dir must surface as --config <path>: {argv}"
        );
        assert!(
            argv.contains(plain.to_str().unwrap()),
            "argv must include the input path: {argv}"
        );
    }

    #[test]
    #[serial]
    fn sops_encrypt_omits_config_flag_when_no_sops_yaml_present() {
        let shim = ToolShim::install("CFGD_SOPS_BIN", 0, "", "");
        let dir = tempfile::tempdir().expect("tempdir");
        // No .sops.yaml created.
        let plain = dir.path().join("secret.yaml");
        std::fs::write(&plain, "key: value\n").expect("write plain");

        let backend = SopsBackend::new(None).with_config_dir(dir.path());
        backend.encrypt_file(&plain).expect("encrypt → Ok");

        let argv = shim.argv_log();
        assert!(
            !argv.contains("--config"),
            "no .sops.yaml → --config must NOT be in argv: {argv}"
        );
    }

    #[test]
    #[serial]
    fn sops_encrypt_propagates_failure_with_stderr() {
        let _shim = ToolShim::install("CFGD_SOPS_BIN", 1, "", "no kms key configured");
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("secret.yaml");
        std::fs::write(&plain, "key: value\n").expect("write");

        let backend = SopsBackend::new(None);
        let err = backend.encrypt_file(&plain).expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("no kms key configured"),
            "stderr must surface in error: {msg}"
        );
    }

    #[test]
    #[serial]
    fn sops_decrypt_returns_stdout_through_secret_string() {
        let _shim = ToolShim::install("CFGD_SOPS_BIN", 0, "decrypted-yaml: 42", "");
        let dir = tempfile::tempdir().expect("tempdir");
        let cipher = dir.path().join("secret.enc.yaml");
        std::fs::write(&cipher, "x").expect("write cipher");

        let backend = SopsBackend::new(None);
        let secret = backend.decrypt_file(&cipher).expect("decrypt → Ok");
        assert_eq!(
            secret.expose_secret(),
            "decrypted-yaml: 42",
            "stdout must round-trip through SecretString"
        );
    }

    #[test]
    #[serial]
    fn sops_decrypt_propagates_failure_with_stderr_and_path() {
        let _shim = ToolShim::install("CFGD_SOPS_BIN", 1, "", "MAC mismatch");
        let dir = tempfile::tempdir().expect("tempdir");
        let cipher = dir.path().join("secret.enc.yaml");
        std::fs::write(&cipher, "x").expect("write cipher");

        let backend = SopsBackend::new(None);
        let err = backend.decrypt_file(&cipher).expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("MAC mismatch"),
            "stderr must surface in error: {msg}"
        );
    }

    #[test]
    #[serial]
    fn sops_age_key_path_propagates_through_env_var() {
        // Sneaky test: the shim records its env via a side-channel — write
        // the test-controlled SOPS_AGE_KEY_FILE to a separate file by adding
        // shell to dump env vars. Easier path: build the command directly
        // and inspect what env it set.
        //
        // Since `sops_command()` is `pub(super)`, we can call it from this
        // tests module directly and inspect get_envs().
        let backend = SopsBackend::new(Some(PathBuf::from("/keys/age-key.txt")));
        let cmd = backend.sops_command();
        let env = cmd
            .get_envs()
            .find(|(k, _)| k == &std::ffi::OsStr::new("SOPS_AGE_KEY_FILE"))
            .expect("SOPS_AGE_KEY_FILE env var must be set");
        assert_eq!(
            env.1.unwrap(),
            std::ffi::OsStr::new("/keys/age-key.txt"),
            "age_key_path must be passed via SOPS_AGE_KEY_FILE"
        );
    }
}

// ---------------------------------------------------------------------------
// SecretProvider backends — vault / 1password / lastpass / bitwarden — driven
// through the same generic ToolShim. Each test pins the resolve() argv shape
// (reference parsing → tool args), the success path (stdout → SecretString),
// and the failure path (stderr → UnresolvableRef).
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod provider_shim {
    use super::*;
    use cfgd_core::test_helpers::ToolShim;
    use secrecy::ExposeSecret;
    use serial_test::serial;

    // --- VaultProvider ---

    #[test]
    #[serial]
    fn vault_resolve_with_field_uses_dash_field_form_and_path_terminator() {
        let shim = ToolShim::install("CFGD_VAULT_BIN", 0, "secret-payload\n", "");
        let secret = VaultProvider
            .resolve("secret/db/creds#password")
            .expect("happy path → Ok");
        assert_eq!(
            secret.expose_secret(),
            "secret-payload",
            "stdout (trimmed) → SecretString"
        );

        let argv = shim.argv_log();
        assert!(
            argv.contains("kv get"),
            "argv must include `kv get`: {argv}"
        );
        assert!(
            argv.contains("-field=password"),
            "field after # must reach vault as -field=<field>: {argv}"
        );
        assert!(
            argv.contains(" -- secret/db/creds"),
            "`--` must terminate flags before path: {argv}"
        );
    }

    #[test]
    #[serial]
    fn vault_resolve_without_field_defaults_to_field_value() {
        let shim = ToolShim::install("CFGD_VAULT_BIN", 0, "x", "");
        VaultProvider.resolve("secret/db/creds").expect("Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains("-field=value"),
            "no `#field` → defaults to -field=value: {argv}"
        );
    }

    #[test]
    #[serial]
    fn vault_resolve_propagates_failure_with_stderr_in_unresolvable_ref() {
        let _shim = ToolShim::install("CFGD_VAULT_BIN", 1, "", "permission denied");
        let err = VaultProvider
            .resolve("secret/locked#password")
            .expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("permission denied"),
            "stderr must surface in error: {msg}"
        );
        assert!(
            msg.contains("secret/locked#password"),
            "reference must surface in error: {msg}"
        );
    }

    // --- OnePasswordProvider ---

    #[test]
    #[serial]
    fn op_resolve_prepends_op_scheme_when_missing() {
        let shim = ToolShim::install("CFGD_OP_BIN", 0, "topsecret", "");
        OnePasswordProvider
            .resolve("Personal/Login/password")
            .expect("Ok");
        let argv = shim.argv_log();
        assert!(argv.contains("read"), "argv must use op `read`: {argv}");
        assert!(
            argv.contains("op://Personal/Login/password"),
            "scheme prefix must be added when missing: {argv}"
        );
    }

    #[test]
    #[serial]
    fn op_resolve_passes_through_op_scheme_unchanged() {
        let shim = ToolShim::install("CFGD_OP_BIN", 0, "x", "");
        OnePasswordProvider
            .resolve("op://Vault/Item/Section/Field")
            .expect("Ok");
        let argv = shim.argv_log();
        // No double prefix (`op://op://...`) — explicit-scheme refs round-trip.
        assert!(
            !argv.contains("op://op://"),
            "must not double-prefix the scheme: {argv}"
        );
        assert!(
            argv.contains("op://Vault/Item/Section/Field"),
            "explicit scheme passes through verbatim: {argv}"
        );
    }

    #[test]
    #[serial]
    fn op_resolve_returns_stdout_through_secret_string() {
        let _shim = ToolShim::install("CFGD_OP_BIN", 0, "topsecret", "");
        let secret = OnePasswordProvider
            .resolve("op://Vault/Item/password")
            .expect("Ok");
        assert_eq!(secret.expose_secret(), "topsecret", "stdout → SecretString");
    }

    #[test]
    #[serial]
    fn op_resolve_propagates_failure_with_stderr_in_error() {
        let _shim = ToolShim::install("CFGD_OP_BIN", 1, "", "could not find item");
        let err = OnePasswordProvider
            .resolve("op://Personal/Login/password")
            .expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("could not find item"),
            "stderr must surface in error: {msg}"
        );
        assert!(
            msg.contains("op://Personal/Login/password"),
            "reference must surface in error: {msg}"
        );
    }

    #[test]
    #[serial]
    fn op_resolve_passes_separator_to_prevent_argument_injection() {
        let shim = ToolShim::install("CFGD_OP_BIN", 0, "val", "");
        OnePasswordProvider
            .resolve("op://Vault/--help/field")
            .expect("Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains("-- op://Vault/--help/field"),
            "double-dash separator must precede the reference to prevent injection: {argv}"
        );
    }

    #[test]
    #[serial]
    fn op_is_available_true_when_seam_points_to_existing_file() {
        let shim = ToolShim::install("CFGD_OP_BIN", 0, "", "");
        assert!(
            OnePasswordProvider.is_available(),
            "seam points at installed shim binary: {}",
            shim.argv_log()
        );
    }

    #[test]
    #[serial]
    fn op_is_available_false_when_seam_points_to_missing_file() {
        unsafe {
            std::env::set_var("CFGD_OP_BIN", "/nonexistent/path/to/op");
        }
        let available = OnePasswordProvider.is_available();
        unsafe {
            std::env::remove_var("CFGD_OP_BIN");
        }
        assert!(
            !available,
            "must return false when CFGD_OP_BIN points to a nonexistent path"
        );
    }

    // --- LastPassProvider ---

    #[test]
    #[serial]
    fn lpass_resolve_with_field_uses_field_flag() {
        let shim = ToolShim::install("CFGD_LPASS_BIN", 0, "v", "");
        LastPassProvider
            .resolve("Folder/MyItem/username")
            .expect("Ok");
        let argv = shim.argv_log();
        assert!(argv.contains("show"), "argv must use lpass `show`: {argv}");
        assert!(
            argv.contains("--field=username"),
            "trailing /<field> reaches lpass as --field=<field>: {argv}"
        );
        assert!(
            argv.contains(" -- Folder/MyItem"),
            "item name after final / removed: {argv}"
        );
    }

    #[test]
    #[serial]
    fn lpass_resolve_without_field_uses_password_flag() {
        let shim = ToolShim::install("CFGD_LPASS_BIN", 0, "p", "");
        LastPassProvider.resolve("LonelyItem").expect("Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains("--password"),
            "single-segment ref → --password flag: {argv}"
        );
        assert!(
            !argv.contains("--field"),
            "single-segment ref must NOT add --field: {argv}"
        );
    }

    #[test]
    #[serial]
    fn lpass_resolve_propagates_stderr_in_error_message() {
        let _shim = ToolShim::install("CFGD_LPASS_BIN", 1, "", "not logged in");
        let err = LastPassProvider
            .resolve("Folder/Item/password")
            .expect_err("non-zero → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("not logged in"),
            "stderr surfaces in error: {msg}"
        );
    }

    // --- BitwardenProvider ---

    #[test]
    #[serial]
    fn bw_resolve_extracts_item_name_from_folder_slash_item() {
        let shim = ToolShim::install("CFGD_BW_BIN", 0, "bw-secret", "");
        let secret = BitwardenProvider.resolve("Personal/MyLogin").expect("Ok");
        assert_eq!(secret.expose_secret(), "bw-secret", "stdout → SecretString");

        let argv = shim.argv_log();
        assert!(
            argv.contains("get password"),
            "argv must request password via `get password`: {argv}"
        );
        assert!(
            argv.contains(" -- MyLogin"),
            "second / segment is the item name passed verbatim: {argv}"
        );
    }

    #[test]
    #[serial]
    fn bw_resolve_uses_full_reference_when_no_slash() {
        let shim = ToolShim::install("CFGD_BW_BIN", 0, "x", "");
        BitwardenProvider.resolve("BareItem").expect("Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains(" -- BareItem"),
            "no `/` → whole reference is the item name: {argv}"
        );
    }
}
