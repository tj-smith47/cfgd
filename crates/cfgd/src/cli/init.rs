use std::path::Path;

use super::*;

// ─────────────────────────────────────────────────────
// cfgd init — pure scaffolding
// ─────────────────────────────────────────────────────

pub(super) struct InitArgs<'a> {
    pub path: Option<&'a str>,
    pub from: Option<&'a str>,
    pub branch: &'a str,
    pub name: Option<&'a str>,
    pub apply: bool,
    pub yes: bool,
    pub install_daemon: bool,
}

/// Scaffold a new cfgd configuration repository.
pub(super) fn cmd_init(printer: &Printer, args: &InitArgs<'_>) -> anyhow::Result<()> {
    printer.header("Initialize cfgd");

    if !check_prerequisites(printer) {
        return Ok(());
    }

    // 1. Determine target directory
    let target_dir = match args.path {
        Some(p) => cfgd_core::expand_tilde(Path::new(p)),
        None => std::env::current_dir()?,
    };

    // 2. Create directory if it doesn't exist
    if !target_dir.exists() {
        std::fs::create_dir_all(&target_dir)?;
    }

    // 3. Check if already initialized
    if target_dir.join("cfgd.yaml").exists() {
        printer.info(&format!(
            "Already initialized at {}",
            target_dir.display()
        ));
        return Ok(());
    }

    // 4. Clone or scaffold
    if let Some(url) = args.from {
        clone_into(&target_dir, url, args.branch, printer)?;
    } else {
        scaffold(&target_dir, args.name, printer)?;
    }

    // 5. Generate release workflow based on what's present
    regenerate_workflow(&target_dir, printer)?;

    // 6. Git init if not already a repo
    if !target_dir.join(".git").exists() {
        match git2::Repository::init(&target_dir) {
            Ok(_) => printer.success("Initialized git repository"),
            Err(e) => printer.warning(&format!("Could not init git repo: {}", e)),
        }
    }

    printer.newline();
    printer.success(&format!("Initialized at {}", target_dir.display()));

    // 7. Apply if requested (requires a profile to exist)
    if args.apply {
        let config_path = target_dir.join("cfgd.yaml");
        let cfg = config::load_config(&config_path)?;

        if cfg.spec.profile.is_some() {
            printer.newline();
            printer.header("Applying Configuration");

            let profile_name = cfg.active_profile()?;
            let profiles_dir = target_dir.join("profiles");
            let resolved = config::resolve_profile(profile_name, &profiles_dir)?;
            let mut registry = super::build_registry_with_config(Some(&cfg));
            let store = super::open_state_store()?;

            let all_managers: Vec<&dyn cfgd_core::providers::PackageManager> = registry
                .package_managers
                .iter()
                .map(|m| m.as_ref())
                .collect();
            let pkg_actions = super::packages::plan_packages(&resolved.merged, &all_managers)?;

            let fm = super::CfgdFileManager::new(&target_dir, &resolved)?;
            let file_actions = fm.plan(&resolved.merged)?;

            registry.file_manager = Some(Box::new(fm));

            let reconciler = cfgd_core::reconciler::Reconciler::new(&registry, &store);
            let plan = reconciler.plan(&resolved, file_actions, pkg_actions, Vec::new())?;

            let total = plan.total_actions();
            if total == 0 {
                printer.success("Nothing to do — system is already configured");
            } else {
                if !args.yes {
                    for phase in &plan.phases {
                        let items = cfgd_core::reconciler::format_plan_items(phase);
                        printer.plan_phase(phase.name.display_name(), &items);
                    }
                    printer.info(&format!("{} action(s) planned", total));
                    let confirmed = printer
                        .prompt_confirm("Apply these changes?")
                        .unwrap_or(false);
                    if !confirmed {
                        printer.info("Skipped — run 'cfgd apply' to apply later");
                        return Ok(());
                    }
                }

                let result = reconciler.apply(&plan, &resolved, &target_dir, printer, None, &[])?;
                super::print_apply_result(&result, printer);
            }
        } else {
            printer.info("No profile configured — skipping apply");
            printer.info("Create one with: cfgd profile create <name>");
        }
    }

    // 8. Install daemon if requested
    if args.install_daemon {
        let config_path = target_dir.join("cfgd.yaml");
        let cfg = config::load_config(&config_path)?;
        let profile = cfg.spec.profile.as_deref();
        match cfgd_core::daemon::install_service(&config_path, profile) {
            Ok(()) => printer.success("Daemon service installed"),
            Err(e) => {
                printer.warning(&format!("Could not install daemon: {}", e));
                printer.info("Install later with: cfgd daemon --install");
            }
        }
    }

    // 9. Print next steps
    if !args.apply {
        printer.newline();
        printer.info("Next steps:");
        printer.info("  cfgd module create <name>   — create a module");
        printer.info("  cfgd profile create <name>  — create a profile");
        printer.info("  cfgd apply                  — apply configuration");
    }

    Ok(())
}

/// Clone a remote repo into the target directory.
fn clone_into(target_dir: &Path, url: &str, branch: &str, printer: &Printer) -> anyhow::Result<()> {
    // If target already has .git, pull instead
    if target_dir.join(".git").exists() {
        printer.info("Repository already exists, pulling latest...");
        match cfgd_core::daemon::git_pull_sync(target_dir) {
            Ok(true) => printer.success("Pulled new changes"),
            Ok(false) => printer.success("Already up to date"),
            Err(e) => printer.warning(&format!("Pull failed: {} — continuing", e)),
        }
        return Ok(());
    }

    printer.info(&format!("Cloning {} (branch: {}) ...", url, branch));

    cfgd_core::sources::git_clone_with_fallback(url, target_dir)
        .map_err(|e| anyhow::anyhow!("Clone failed: {}", e))?;

    printer.success(&format!("Cloned to {}", target_dir.display()));

    // Checkout branch if not main
    if branch != "main" {
        let repo = git2::Repository::open(target_dir)
            .map_err(|e| anyhow::anyhow!("Failed to open cloned repo: {}", e))?;
        let remote_branch = format!("origin/{}", branch);
        let obj = repo
            .revparse_single(&remote_branch)
            .map_err(|_| anyhow::anyhow!("Branch '{}' not found in remote", branch))?;
        repo.checkout_tree(&obj, None)
            .map_err(|e| anyhow::anyhow!("Failed to checkout '{}': {}", branch, e))?;
        repo.set_head(&format!("refs/heads/{}", branch))
            .map_err(|e| anyhow::anyhow!("Failed to set HEAD to '{}': {}", branch, e))?;
        printer.info(&format!("Checked out branch: {}", branch));
    }

    Ok(())
}

/// Create the cfgd directory structure from scratch.
fn scaffold(dir: &Path, name: Option<&str>, printer: &Printer) -> anyhow::Result<()> {
    let config_name = name
        .or_else(|| dir.file_name().and_then(|n| n.to_str()))
        .unwrap_or("my-config");

    // Create directories
    std::fs::create_dir_all(dir.join("profiles"))?;
    std::fs::create_dir_all(dir.join("modules"))?;
    std::fs::create_dir_all(dir.join("files"))?;

    // cfgd.yaml — no profile set; user creates one after init
    let content = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: {config_name}
spec: {{}}
"#
    );
    std::fs::write(dir.join("cfgd.yaml"), &content)?;
    printer.success("Created cfgd.yaml");

    // .gitignore
    let gitignore = ".cfgd-state/\n*.age\ntarget/\n";
    std::fs::write(dir.join(".gitignore"), gitignore)?;

    Ok(())
}

/// Generate or regenerate the release workflow based on current modules/profiles.
/// Called by init and also by module create / profile create.
pub(super) fn regenerate_workflow(config_dir: &Path, printer: &Printer) -> anyhow::Result<()> {
    let profiles = scan_profile_names(&config_dir.join("profiles"))?;
    let modules = scan_module_names(&config_dir.join("modules"))?;

    if profiles.is_empty() && modules.is_empty() {
        return Ok(());
    }

    let workflow_dir = config_dir.join(".github").join("workflows");
    std::fs::create_dir_all(&workflow_dir)?;

    let yaml = generate_release_workflow_yaml(&modules, &profiles);
    std::fs::write(workflow_dir.join("cfgd-release.yml"), &yaml)?;
    printer.success("Generated .github/workflows/cfgd-release.yml");

    Ok(())
}

fn check_prerequisites(printer: &Printer) -> bool {
    if !which("git") {
        printer.error("git is not installed — cfgd requires git");
        if cfg!(target_os = "macos") {
            printer.info("Install with: xcode-select --install");
        } else {
            printer.info("Install with: sudo apt install git (or your package manager)");
        }
        return false;
    }
    true
}

// ─────────────────────────────────────────────────────
// ensure_config_file — used by profile switch and tests
// ─────────────────────────────────────────────────────

#[cfg(test)]
fn ensure_config_file(
    config_dir: &Path,
    config_path: &Path,
    profile_name: &str,
    from_url: Option<&str>,
    branch: &str,
    theme: Option<&str>,
) -> anyhow::Result<()> {
    if config_path.exists() {
        let contents = std::fs::read_to_string(config_path)?;
        let mut cfg = config::parse_config(&contents, config_path)?;
        if cfg.spec.profile.as_deref() != Some(profile_name) {
            cfg.spec.profile = Some(profile_name.to_string());
            let yaml = serde_yaml::to_string(&cfg)?;
            std::fs::write(config_path, &yaml)?;
        }
        return Ok(());
    }

    let name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-config");

    let origin_section = if let Some(url) = from_url {
        format!(
            r#"  origin:
    type: git
    url: {}
    branch: {}
"#,
            url, branch
        )
    } else {
        String::new()
    };

    let theme_section = if let Some(preset) = theme {
        format!(
            r#"  theme:
    preset: {}
"#,
            preset
        )
    } else {
        String::new()
    };

    let config_content = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: {}
spec:
  profile: {}
{}{}"#,
        name, profile_name, origin_section, theme_section
    );

    std::fs::write(config_path, &config_content)?;
    Ok(())
}

// ─────────────────────────────────────────────────────
// count_packages — used by profile display
// ─────────────────────────────────────────────────────

#[cfg(test)]
fn count_packages(spec: &config::ProfileSpec) -> usize {
    let mut count = 0;
    if let Some(ref pkgs) = spec.packages {
        if let Some(ref brew) = pkgs.brew {
            count += brew.formulae.len() + brew.casks.len();
        }
        if let Some(ref apt) = pkgs.apt {
            count += apt.packages.len();
        }
        if let Some(ref cargo) = pkgs.cargo {
            count += cargo.packages.len();
        }
        if let Some(ref npm) = pkgs.npm {
            count += npm.global.len();
        }
        count += pkgs.pipx.len();
        count += pkgs.dnf.len();
    }
    count
}

// ─────────────────────────────────────────────────────
// cfgd enroll — unified enrollment (token + key-based)
// ─────────────────────────────────────────────────────

pub(super) fn cmd_enroll(
    printer: &Printer,
    server_url: &str,
    token: Option<&str>,
    ssh_key: Option<&str>,
    gpg_key: Option<&str>,
    username: Option<&str>,
) -> anyhow::Result<()> {
    let username = match username {
        Some(u) => u.to_string(),
        None => std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
    };
    let device_id = default_device_id();

    printer.key_value("Server", server_url);
    printer.key_value("Device ID", &device_id);

    let client = cfgd_core::server_client::ServerClient::new(server_url, None, &device_id);

    // Token-based enrollment (direct exchange)
    if let Some(token) = token {
        printer.header("Token Enrollment");
        printer.info("Exchanging bootstrap token for device credential...");

        let resp = client
            .enroll(token, printer)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        return finish_enrollment(printer, server_url, &device_id, resp);
    }

    // Key-based enrollment (challenge-response)
    printer.header("Key-Based Enrollment");
    printer.key_value("Username", &username);

    // Check server enrollment method
    let info = client
        .enroll_info()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if info.method == "token" {
        printer.warning("This server uses bootstrap token enrollment");
        printer.info("Run: cfgd enroll --server <url> --token <token>");
        return Ok(());
    }

    // Determine signing method
    let (key_type, key_ref) = if let Some(gpg_id) = gpg_key {
        ("gpg".to_string(), gpg_id.to_string())
    } else if let Some(ssh_path) = ssh_key {
        ("ssh".to_string(), ssh_path.to_string())
    } else {
        match detect_ssh_key(printer) {
            Some(path) => ("ssh".to_string(), path),
            None => {
                anyhow::bail!(
                    "no SSH key found — provide --ssh-key <path> or --gpg-key <id>\n\
                     Checked: SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa, ~/.ssh/id_ecdsa"
                );
            }
        }
    };

    printer.key_value(
        "Signing with",
        &format!("{} ({})", key_type.to_uppercase(), key_ref),
    );
    printer.newline();

    // Challenge-response
    let challenge = client
        .request_challenge(&username, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    printer.key_value("Challenge ID", &challenge.challenge_id);
    printer.key_value("Expires", &challenge.expires_at);

    let signature = match key_type.as_str() {
        "ssh" => sign_with_ssh(&challenge.nonce, &key_ref)?,
        "gpg" => sign_with_gpg(&challenge.nonce, &key_ref)?,
        _ => unreachable!(),
    };

    printer.success("Challenge signed");

    let resp = client
        .submit_verification(&challenge.challenge_id, &signature, &key_type, printer)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    finish_enrollment(printer, server_url, &device_id, resp)
}

/// Shared enrollment completion: save credential, handle desired config, print next steps.
fn finish_enrollment(
    printer: &Printer,
    server_url: &str,
    device_id: &str,
    resp: cfgd_core::server_client::EnrollResponse,
) -> anyhow::Result<()> {
    printer.newline();
    printer.success(&format!("Enrolled as user '{}'", resp.username));
    if let Some(ref team) = resp.team {
        printer.key_value("Team", team);
    }
    printer.key_value("Device", &resp.device_id);

    let credential = cfgd_core::server_client::DeviceCredential {
        server_url: server_url.to_string(),
        device_id: device_id.to_string(),
        api_key: resp.api_key.clone(),
        username: resp.username.clone(),
        team: resp.team.clone(),
        enrolled_at: cfgd_core::utc_now_iso8601(),
    };

    match cfgd_core::server_client::save_credential(&credential) {
        Ok(path) => {
            printer.success(&format!("Credential saved to {}", path.display()));
        }
        Err(e) => {
            printer.error(&format!("Failed to save credential: {}", e));
            printer.warning("You will need to manually provide --api-key for future commands");
        }
    }

    if let Some(ref desired) = resp.desired_config {
        match cfgd_core::state::save_pending_server_config(desired) {
            Ok(path) => {
                printer.newline();
                printer.info(&format!(
                    "Server pushed desired config — saved to {}",
                    path.display()
                ));
                printer.info(MSG_RUN_APPLY);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to save pending server config");
            }
        }
    }

    printer.newline();
    printer.header("Next Steps");
    printer.info("  cfgd checkin --server-url <url>  — report status to server");
    printer.info("  cfgd apply --dry-run             — preview configuration");
    printer.info("  cfgd apply                       — apply configuration");
    printer.info("  cfgd daemon --install             — start background sync");

    Ok(())
}

// ─────────────────────────────────────────────────────
// SSH/GPG signing helpers
// ─────────────────────────────────────────────────────

fn detect_ssh_key(printer: &Printer) -> Option<String> {
    // Try SSH agent first
    if let Ok(output) = std::process::Command::new("ssh-add").arg("-l").output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = stdout.lines().next()
            && !line.contains("no identities")
        {
            let home = std::env::var("HOME").unwrap_or_default();
            for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
                let pub_path = Path::new(&home)
                    .join(".ssh")
                    .join(format!("{key_name}.pub"));
                if pub_path.exists() {
                    printer.info(&format!("Using SSH key from agent: {}", pub_path.display()));
                    return Some(pub_path.to_string_lossy().to_string());
                }
            }
            for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
                let key_path = Path::new(&home).join(".ssh").join(key_name);
                if key_path.exists() {
                    printer.info(&format!("Using SSH key: {}", key_path.display()));
                    return Some(key_path.to_string_lossy().to_string());
                }
            }
        }
    }

    // Fall back to on-disk keys
    let home = std::env::var("HOME").unwrap_or_default();
    for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        let pub_path = Path::new(&home)
            .join(".ssh")
            .join(format!("{key_name}.pub"));
        if pub_path.exists() {
            printer.info(&format!("Using SSH key: {}", pub_path.display()));
            return Some(pub_path.to_string_lossy().to_string());
        }
        let key_path = Path::new(&home).join(".ssh").join(key_name);
        if key_path.exists() {
            printer.info(&format!("Using SSH key: {}", key_path.display()));
            return Some(key_path.to_string_lossy().to_string());
        }
    }

    None
}

fn sign_with_ssh(nonce: &str, key_path: &str) -> anyhow::Result<String> {
    let tmp_dir = tempfile::tempdir()?;
    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.sig");

    std::fs::write(&data_path, nonce)?;

    let status = std::process::Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            key_path,
            "-n",
            "cfgd-enroll",
            data_path.to_str().unwrap_or("challenge.txt"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| anyhow::anyhow!("ssh-keygen not found: {e} — is OpenSSH installed?"))?;

    if !status.success() {
        anyhow::bail!("ssh-keygen -Y sign failed — check that your SSH key is accessible");
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read SSH signature: {e}"))?;

    Ok(signature)
}

fn sign_with_gpg(nonce: &str, gpg_key_id: &str) -> anyhow::Result<String> {
    let tmp_dir = tempfile::tempdir()?;
    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.asc");

    std::fs::write(&data_path, nonce)?;

    let status = std::process::Command::new("gpg")
        .args([
            "--batch",
            "--yes",
            "--detach-sign",
            "--armor",
            "-u",
            gpg_key_id,
            "-o",
            sig_path.to_str().unwrap_or("challenge.txt.asc"),
            data_path.to_str().unwrap_or("challenge.txt"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| anyhow::anyhow!("gpg not found: {e} — is GPG installed?"))?;

    if !status.success() {
        anyhow::bail!(
            "gpg --detach-sign failed — check that key '{}' is available",
            gpg_key_id
        );
    }

    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| anyhow::anyhow!("failed to read GPG signature: {e}"))?;

    Ok(signature)
}

// ─────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_creates_structure() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), Some("test-config"), &printer).unwrap();

        assert!(dir.path().join("cfgd.yaml").exists());
        assert!(dir.path().join("profiles").is_dir());
        assert!(dir.path().join("modules").is_dir());
        assert!(dir.path().join("files").is_dir());
        assert!(dir.path().join(".gitignore").exists());

        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        assert!(contents.contains("name: test-config"));
    }

    #[test]
    fn scaffold_uses_dir_name_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);

        scaffold(dir.path(), None, &printer).unwrap();

        let contents = std::fs::read_to_string(dir.path().join("cfgd.yaml")).unwrap();
        // Should use the tempdir name
        assert!(contents.contains("name:"));
    }

    #[test]
    fn ensure_config_file_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        ensure_config_file(
            dir.path(),
            &config_path,
            "work",
            Some("https://github.com/test/init-cfg.git"),
            "main",
            None,
        )
        .unwrap();

        assert!(config_path.exists());
        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("profile: work"));
        assert!(contents.contains("https://github.com/test/init-cfg.git"));
    }

    #[test]
    fn ensure_config_file_updates_existing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        ensure_config_file(dir.path(), &config_path, "work", None, "main", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    }

    #[test]
    fn ensure_config_file_no_update_if_same_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        let original = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n";
        std::fs::write(&config_path, original).unwrap();

        ensure_config_file(dir.path(), &config_path, "default", None, "main", None).unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, original);
    }

    #[test]
    fn ensure_config_file_with_branch_and_theme() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("cfgd.yaml");

        ensure_config_file(
            dir.path(),
            &config_path,
            "work",
            Some("https://github.com/test/cfg.git"),
            "develop",
            Some("minimal"),
        )
        .unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("branch: develop"));
        assert!(contents.contains("preset: minimal"));
    }

    #[test]
    fn count_packages_empty() {
        let spec = config::ProfileSpec::default();
        assert_eq!(count_packages(&spec), 0);
    }

    #[test]
    fn count_packages_with_various() {
        let spec = config::ProfileSpec {
            packages: Some(config::PackagesSpec {
                brew: Some(config::BrewSpec {
                    formulae: vec!["rg".into(), "fd".into()],
                    casks: vec!["firefox".into()],
                    ..Default::default()
                }),
                cargo: Some(config::CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(count_packages(&spec), 4);
    }

    #[test]
    fn profile_switch_via_config_update() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("work.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  variables: {}\n",
        )
        .unwrap();

        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

        ensure_config_file(dir.path(), &config_path, "work", None, "main", None).unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
    }

    #[test]
    fn regenerate_workflow_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        regenerate_workflow(dir.path(), &printer).unwrap();

        // No profiles or modules → no workflow generated
        assert!(!dir.path().join(".github/workflows/cfgd-release.yml").exists());
    }

    #[test]
    fn regenerate_workflow_with_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profiles_dir = dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();
        std::fs::write(
            profiles_dir.join("base.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
        )
        .unwrap();

        let printer = Printer::new(cfgd_core::output::Verbosity::Quiet);
        regenerate_workflow(dir.path(), &printer).unwrap();

        let workflow = dir.path().join(".github/workflows/cfgd-release.yml");
        assert!(workflow.exists());
        let contents = std::fs::read_to_string(&workflow).unwrap();
        assert!(contents.contains("base"));
    }
}
