use super::*;
use cfgd_core::output::{Printer, Role};

/// Cryptographically verify a git tag signature using `git tag -v`.
/// Returns `Ok(true)` if verified, `Ok(false)` if verification fails (bad sig),
/// or `Err` if `git` is not available or keyring is not configured.
fn verify_tag_signature_cryptographic(repo_dir: &Path, tag_name: &str) -> anyhow::Result<bool> {
    let output = cfgd_core::git_cmd_local()
        .args(["tag", "-v", tag_name])
        .current_dir(repo_dir)
        .output()?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = cfgd_core::stderr_lossy_trimmed(&output);
        // Distinguish "bad signature" from "no keyring / gpg not found"
        if stderr.contains("BAD signature")
            || stderr.contains("BADSIG")
            || stderr.contains("verification failed")
        {
            Ok(false) // Signature present but invalid
        } else {
            // gpg not installed, key not in keyring, etc.
            anyhow::bail!("{}", stderr)
        }
    }
}

/// Check tag signature and enforce require-signatures policy.
/// Loads `require_signatures` from config, checks the tag signature,
/// and bails if policy is violated. Returns Ok(()) if allowed to proceed.
pub(crate) fn enforce_signature_policy(
    cli: &Cli,
    printer: &Printer,
    tag: Option<&str>,
    module_name: &str,
    allow_unsigned: bool,
    cache_base: &Path,
    repo_url: &str,
) -> anyhow::Result<()> {
    let require_signatures = cli
        .config
        .exists()
        .then(|| config::load_config(&cli.config).ok())
        .flatten()
        .and_then(|c| c.spec.modules)
        .and_then(|m| m.security)
        .is_some_and(|s| s.require_signatures);

    let Some(tag) = tag else {
        if require_signatures && !allow_unsigned {
            printer.emit(cfgd_core::output::error_doc(
                module_name,
                "signature_required",
                format!(
                    "Module '{}' has no tag — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                    module_name
                ),
                serde_json::json!({}),
            ));
            anyhow::bail!(
                "Module '{}' has no tag — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                module_name
            );
        }
        return Ok(());
    };

    let repo_dir = modules::git_cache_dir(cache_base, repo_url);
    let sig_status = modules::check_tag_signature(&repo_dir, tag, module_name);

    match &sig_status {
        Ok(modules::TagSignatureStatus::SignaturePresent) => {
            match verify_tag_signature_cryptographic(&repo_dir, tag) {
                Ok(true) => {
                    printer.status_simple(Role::Ok, format!("Tag '{}' signature verified", tag))
                }
                Ok(false) => {
                    printer.emit(cfgd_core::output::error_doc(
                        module_name,
                        "signature_failed",
                        format!(
                            "Tag '{}' has an INVALID signature — refusing to proceed",
                            tag
                        ),
                        serde_json::json!({ "tag": tag }),
                    ));
                    anyhow::bail!(
                        "Signature verification failed for tag '{}' — the signature is present but invalid",
                        tag
                    );
                }
                Err(e) => {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Tag '{}' has a signature but verification skipped: {}",
                            tag, e
                        ),
                    );
                }
            }
        }
        Ok(modules::TagSignatureStatus::Unsigned)
        | Ok(modules::TagSignatureStatus::LightweightTag) => {
            let label = match &sig_status {
                Ok(modules::TagSignatureStatus::LightweightTag) => {
                    format!("Tag '{}' is lightweight (cannot carry a signature)", tag)
                }
                _ => format!("Tag '{}' is unsigned", tag),
            };
            if require_signatures && !allow_unsigned {
                printer.emit(cfgd_core::output::error_doc(
                    module_name,
                    "signature_required",
                    label.clone(),
                    serde_json::json!({ "tag": tag }),
                ));
                anyhow::bail!(
                    "Module '{}' tag '{}' is unsigned — your config has require-signatures enabled. Pass --allow-unsigned to override.",
                    module_name,
                    tag
                );
            }
            printer.status_simple(Role::Info, label);
        }
        Ok(modules::TagSignatureStatus::TagNotFound) => {
            printer.status_simple(Role::Warn, format!("Tag '{}' not found in repo", tag));
        }
        Err(e) => printer.status_simple(Role::Warn, format!("Signature check: {}", e)),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::Printer;

    fn make_cli_with_config(config_path: &Path) -> Cli {
        Cli {
            config: config_path.to_path_buf(),
            profile: None,
            no_color: true,
            verbose: 0,
            quiet: true,
            output: crate::cli::OutputFormatArg(cfgd_core::output::OutputFormat::Table),
            jsonpath: None,
            state_dir: None,
            command: None,
        }
    }

    fn make_cache_with_tag(repo_url: &str, tag_name: &str, signed: bool) -> tempfile::TempDir {
        let cache = tempfile::tempdir().expect("cache tmpdir");
        let repo_dir = modules::git_cache_dir(cache.path(), repo_url);
        std::fs::create_dir_all(&repo_dir).expect("create repo dir");
        let repo = git2::Repository::init(&repo_dir).expect("init repo");
        let sig = git2::Signature::now("Test", "test@example.com").expect("sig");
        let tree_id = repo.index().unwrap().write_tree().expect("tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .expect("commit");
        let obj = repo.find_object(commit_oid, None).expect("obj");
        if signed {
            // Embed a fake PGP block in the tag message; check_tag_signature
            // detects via substring (no crypto), so this is enough to land in
            // the SignaturePresent arm.
            let msg = format!(
                "release {}\n-----BEGIN PGP SIGNATURE-----\nfake\n-----END PGP SIGNATURE-----\n",
                tag_name
            );
            repo.tag(tag_name, &obj, &sig, &msg, false).expect("tag");
        } else {
            repo.tag_lightweight(tag_name, &obj, false).expect("tag");
        }
        cache
    }

    #[test]
    fn enforce_no_tag_returns_ok_when_require_signatures_off() {
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let tmp = tempfile::tempdir().unwrap();
        let cli = make_cli_with_config(&tmp.path().join("noexist.yaml"));
        let cache = tempfile::tempdir().unwrap();
        let result =
            enforce_signature_policy(&cli, &printer, None, "mod", false, cache.path(), "url");
        result.expect("no tag with require_signatures off must succeed");
    }

    #[test]
    fn enforce_no_tag_with_require_signatures_bails() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &cfg_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  modules:\n    security:\n      requireSignatures: true\n",
        )
        .unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cli = make_cli_with_config(&cfg_path);
        let cache = tempfile::tempdir().unwrap();
        let err = enforce_signature_policy(&cli, &printer, None, "mod", false, cache.path(), "url")
            .expect_err("must bail when no tag + require_signatures + !allow_unsigned");
        assert!(
            err.to_string().contains("no tag"),
            "error must mention 'no tag': {err}"
        );
    }

    #[test]
    fn enforce_no_tag_with_require_signatures_but_allow_unsigned_returns_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &cfg_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  modules:\n    security:\n      requireSignatures: true\n",
        )
        .unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cli = make_cli_with_config(&cfg_path);
        let cache = tempfile::tempdir().unwrap();
        enforce_signature_policy(&cli, &printer, None, "mod", true, cache.path(), "url")
            .expect("allow_unsigned must short-circuit the bail");
    }

    #[test]
    fn enforce_lightweight_tag_with_require_signatures_bails() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &cfg_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  modules:\n    security:\n      requireSignatures: true\n",
        )
        .unwrap();
        let cache = make_cache_with_tag("url-lw", "v0.1.0", false);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cli = make_cli_with_config(&cfg_path);
        let err = enforce_signature_policy(
            &cli,
            &printer,
            Some("v0.1.0"),
            "mod",
            false,
            cache.path(),
            "url-lw",
        )
        .expect_err("lightweight tag + require_signatures must bail");
        assert!(
            err.to_string().contains("unsigned") || err.to_string().contains("lightweight"),
            "error must mention unsigned/lightweight: {err}"
        );
    }

    #[test]
    fn enforce_lightweight_tag_without_require_signatures_returns_ok() {
        let cache = make_cache_with_tag("url-lw-ok", "v0.1.0", false);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let tmp = tempfile::tempdir().unwrap();
        let cli = make_cli_with_config(&tmp.path().join("noexist.yaml"));
        enforce_signature_policy(
            &cli,
            &printer,
            Some("v0.1.0"),
            "mod",
            false,
            cache.path(),
            "url-lw-ok",
        )
        .expect("lightweight tag is OK when require_signatures is off");
    }

    #[test]
    fn enforce_tag_not_found_emits_warn_but_returns_ok() {
        // Repo with no tag — check_tag_signature returns TagNotFound, which
        // emits a Role::Warn status but returns Ok(()).
        let cache = tempfile::tempdir().unwrap();
        let repo_dir = modules::git_cache_dir(cache.path(), "url-no-tag");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo = git2::Repository::init(&repo_dir).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let tmp = tempfile::tempdir().unwrap();
        let cli = make_cli_with_config(&tmp.path().join("noexist.yaml"));
        enforce_signature_policy(
            &cli,
            &printer,
            Some("no-such"),
            "mod",
            false,
            cache.path(),
            "url-no-tag",
        )
        .expect("tag-not-found should warn but not error");
    }

    #[test]
    fn enforce_signed_tag_runs_through_cryptographic_path() {
        // Tag with a (fake) PGP-block message lands in SignaturePresent →
        // verify_tag_signature_cryptographic runs `git tag -v`, which has
        // no GPG public key in the test env so it returns Err → Warn arm
        // fires and the fn returns Ok(()).
        let cache = make_cache_with_tag("url-signed", "v1.0.0", true);
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let tmp = tempfile::tempdir().unwrap();
        let cli = make_cli_with_config(&tmp.path().join("noexist.yaml"));
        let result = enforce_signature_policy(
            &cli,
            &printer,
            Some("v1.0.0"),
            "mod",
            false,
            cache.path(),
            "url-signed",
        );
        result.expect("signed tag without keyring must Warn but succeed");
    }

    #[test]
    fn enforce_check_tag_signature_err_emits_warn_but_returns_ok() {
        // Pass a cache that has no repo at all — check_tag_signature errors,
        // matched by the `Err(e)` arm and a Warn status is emitted; the fn
        // returns Ok(()) (best-effort).
        let cache = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        let tmp = tempfile::tempdir().unwrap();
        let cli = make_cli_with_config(&tmp.path().join("noexist.yaml"));
        enforce_signature_policy(
            &cli,
            &printer,
            Some("v0.1.0"),
            "mod",
            false,
            cache.path(),
            "url-noexist",
        )
        .expect("check_tag_signature err should warn but not bail");
    }
}
