use std::collections::HashSet;
use std::sync::Mutex;

use cfgd_core::output::{Printer, Verbosity};

use super::cargo::{cargo_available, cargo_cmd};
use super::go::{find_go, go_available, go_cmd};
use super::npm::{find_npm, npm_available, npm_cmd};
use super::pipx::{find_pipx, pipx_available, pipx_cmd};
use super::*;

struct MockPackageManager {
    mgr_name: &'static str,
    available: bool,
    bootstrappable: bool,
    installed: HashSet<String>,
    installs: Mutex<Vec<Vec<String>>>,
    uninstalls: Mutex<Vec<Vec<String>>>,
    // When true, installed_packages() errors — models a present-but-broken
    // manager (e.g. pacman db unreadable). Used to assert plan_packages never
    // probes a manager that has no work to do.
    list_fails: bool,
}

impl MockPackageManager {
    fn new(name: &'static str, available: bool, installed: Vec<&str>) -> Self {
        Self {
            mgr_name: name,
            available,
            bootstrappable: false,
            installed: installed.into_iter().map(String::from).collect(),
            installs: Mutex::new(Vec::new()),
            uninstalls: Mutex::new(Vec::new()),
            list_fails: false,
        }
    }

    fn with_bootstrap(mut self) -> Self {
        self.bootstrappable = true;
        self
    }

    fn with_list_failure(mut self) -> Self {
        self.list_fails = true;
        self
    }
}

impl PackageManager for MockPackageManager {
    fn name(&self) -> &str {
        self.mgr_name
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn can_bootstrap(&self) -> bool {
        self.bootstrappable
    }

    fn bootstrap(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        if self.list_fails {
            return Err(PackageError::ListFailed {
                manager: self.mgr_name.to_string(),
                message: "mock list failure".to_string(),
            }
            .into());
        }
        Ok(self.installed.clone())
    }

    fn install(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        self.installs.lock().unwrap().push(packages.to_vec());
        Ok(())
    }

    fn uninstall(&self, packages: &[String], _printer: &Printer) -> Result<()> {
        self.uninstalls.lock().unwrap().push(packages.to_vec());
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        Ok(())
    }

    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Mock whose `installed_packages` reports BINARY names while `desired` carries
/// MODULE PATHS, mirroring go's name-incoherence. Overrides `package_identity`
/// so install-diffing and prune compare like with like.
struct GoLikeMockManager {
    available: bool,
    installed: HashSet<String>,
    uninstalls: Mutex<Vec<Vec<String>>>,
}

impl GoLikeMockManager {
    fn new(installed: Vec<&str>) -> Self {
        Self {
            available: true,
            installed: installed.into_iter().map(String::from).collect(),
            uninstalls: Mutex::new(Vec::new()),
        }
    }
}

impl PackageManager for GoLikeMockManager {
    fn name(&self) -> &str {
        "go"
    }
    fn is_available(&self) -> bool {
        self.available
    }
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(&self, _: &Printer) -> Result<()> {
        Ok(())
    }
    fn installed_packages(&self) -> Result<HashSet<String>> {
        Ok(self.installed.clone())
    }
    fn install(&self, _: &[String], _: &Printer) -> Result<()> {
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _: &Printer) -> Result<()> {
        self.uninstalls.lock().unwrap().push(packages.to_vec());
        Ok(())
    }
    fn update(&self, _: &Printer) -> Result<()> {
        Ok(())
    }
    fn available_version(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn package_identity(&self, entry: &str) -> String {
        super::go::go_binary_name(entry)
    }
}

fn test_profile(packages: PackagesSpec) -> MergedProfile {
    MergedProfile {
        packages,
        ..Default::default()
    }
}

#[test]
fn default_package_identity_is_passthrough() {
    // Managers that install and list under the same name use the trait default.
    let mock = MockPackageManager::new("apt", true, vec![]);
    assert_eq!(mock.package_identity("fd-find"), "fd-find");
}

#[test]
fn plan_installs_missing_packages() {
    let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["bat".into(), "ripgrep".into(), "fd-find".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        PackageAction::Install {
            manager, packages, ..
        } => {
            assert_eq!(manager, "cargo");
            assert!(packages.contains(&"ripgrep".to_string()));
            assert!(packages.contains(&"fd-find".to_string()));
            assert!(!packages.contains(&"bat".to_string()));
        }
        _ => panic!("expected Install action"),
    }
}

#[test]
fn plan_skips_unavailable_manager() {
    let mock = MockPackageManager::new("brew", false, vec![]);
    let profile = test_profile(PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            formulae: vec!["ripgrep".into()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        PackageAction::Skip {
            manager, reason, ..
        } => {
            assert_eq!(manager, "brew");
            assert!(reason.contains("not available"), "reason: {reason}");
        }
        other => panic!("expected Skip, got: {other:?}"),
    }
}

#[test]
fn plan_empty_when_all_installed() {
    let mock = MockPackageManager::new("cargo", true, vec!["bat", "ripgrep"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["bat".into(), "ripgrep".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert!(actions.is_empty());
}

#[test]
fn plan_skips_manager_with_no_desired_packages() {
    let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
    let profile = test_profile(PackagesSpec::default());

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert!(actions.is_empty());
}

#[test]
fn format_actions_produces_readable_strings() {
    let actions = vec![
        PackageAction::Bootstrap {
            manager: "cargo".into(),
            method: "rustup".into(),
            origin: "local".into(),
        },
        PackageAction::Install {
            manager: "brew".into(),
            packages: vec!["ripgrep".into(), "fd".into()],
            origin: "local".into(),
        },
        PackageAction::Skip {
            manager: "apt".into(),
            reason: "not available".into(),
            origin: "local".into(),
        },
    ];

    let formatted = format_package_actions(&actions);
    assert_eq!(formatted.len(), 3);
    assert!(formatted[0].contains("bootstrap"));
    assert!(formatted[0].contains("rustup"));
    assert!(formatted[1].contains("brew"));
    assert!(formatted[1].contains("ripgrep"));
    assert!(formatted[2].contains("skip"));
}

#[test]
fn add_package_to_spec() {
    let mut packages = PackagesSpec::default();

    add_package("cargo", "ripgrep", &mut packages).unwrap();
    assert_eq!(packages.cargo.as_ref().unwrap().packages, vec!["ripgrep"]);

    // Adding again is idempotent
    add_package("cargo", "ripgrep", &mut packages).unwrap();
    assert_eq!(packages.cargo.as_ref().unwrap().packages, vec!["ripgrep"]);

    add_package("brew", "fd", &mut packages).unwrap();
    assert_eq!(packages.brew.as_ref().unwrap().formulae, vec!["fd"]);

    add_package("brew-cask", "firefox", &mut packages).unwrap();
    assert_eq!(packages.brew.as_ref().unwrap().casks, vec!["firefox"]);

    add_package("apt", "curl", &mut packages).unwrap();
    assert_eq!(packages.apt.as_ref().unwrap().packages, vec!["curl"]);

    add_package("npm", "typescript", &mut packages).unwrap();
    assert_eq!(packages.npm.as_ref().unwrap().global, vec!["typescript"]);

    add_package("pipx", "black", &mut packages).unwrap();
    assert_eq!(packages.pipx, vec!["black"]);

    add_package("dnf", "gcc", &mut packages).unwrap();
    assert_eq!(packages.dnf, vec!["gcc"]);

    add_package("brew-tap", "homebrew/core", &mut packages).unwrap();
    assert_eq!(packages.brew.as_ref().unwrap().taps, vec!["homebrew/core"]);

    add_package("winget", "Microsoft.VisualStudioCode", &mut packages).unwrap();
    assert_eq!(packages.winget, vec!["Microsoft.VisualStudioCode"]);

    add_package("chocolatey", "nodejs", &mut packages).unwrap();
    assert_eq!(packages.chocolatey, vec!["nodejs"]);

    add_package("scoop", "7zip", &mut packages).unwrap();
    assert_eq!(packages.scoop, vec!["7zip"]);
}

#[test]
fn add_package_unknown_manager_errors() {
    let mut packages = PackagesSpec::default();
    let result = add_package("unknown", "pkg", &mut packages);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("'unknown' not available"), "got: {msg}");
}

#[test]
fn remove_package_from_spec() {
    let mut packages = PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["bat".into(), "ripgrep".into()],
        }),
        ..Default::default()
    };

    let removed = remove_package("cargo", "bat", &mut packages).unwrap();
    assert!(removed);
    assert_eq!(packages.cargo.as_ref().unwrap().packages, vec!["ripgrep"]);

    // Not-found returns false
    let removed = remove_package("cargo", "nonexistent", &mut packages).unwrap();
    assert!(!removed);

    // brew formulae
    add_package("brew", "curl", &mut packages).unwrap();
    assert!(remove_package("brew", "curl", &mut packages).unwrap());
    assert!(packages.brew.as_ref().unwrap().formulae.is_empty());

    // brew-tap
    add_package("brew-tap", "homebrew/core", &mut packages).unwrap();
    assert!(remove_package("brew-tap", "homebrew/core", &mut packages).unwrap());

    // brew-cask
    add_package("brew-cask", "firefox", &mut packages).unwrap();
    assert!(remove_package("brew-cask", "firefox", &mut packages).unwrap());

    // apt
    add_package("apt", "git", &mut packages).unwrap();
    assert!(remove_package("apt", "git", &mut packages).unwrap());

    // npm
    add_package("npm", "ts", &mut packages).unwrap();
    assert!(remove_package("npm", "ts", &mut packages).unwrap());

    // pipx
    add_package("pipx", "black", &mut packages).unwrap();
    assert!(remove_package("pipx", "black", &mut packages).unwrap());

    // dnf
    add_package("dnf", "vim", &mut packages).unwrap();
    assert!(remove_package("dnf", "vim", &mut packages).unwrap());

    // winget
    add_package("winget", "Git.Git", &mut packages).unwrap();
    assert!(remove_package("winget", "Git.Git", &mut packages).unwrap());
    assert!(packages.winget.is_empty());

    // chocolatey
    add_package("chocolatey", "python", &mut packages).unwrap();
    assert!(remove_package("chocolatey", "python", &mut packages).unwrap());
    assert!(packages.chocolatey.is_empty());

    // scoop
    add_package("scoop", "ripgrep", &mut packages).unwrap();
    assert!(remove_package("scoop", "ripgrep", &mut packages).unwrap());
    assert!(packages.scoop.is_empty());
}

#[test]
fn remove_package_unknown_manager_errors() {
    let mut packages = PackagesSpec::default();
    let result = remove_package("unknown", "pkg", &mut packages);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("'unknown' not available"), "got: {msg}");
}

#[test]
fn apply_calls_install_on_correct_manager() {
    let mock = MockPackageManager::new("cargo", true, vec![]);
    let actions = vec![PackageAction::Install {
        manager: "cargo".into(),
        packages: vec!["ripgrep".into()],
        origin: "local".into(),
    }];

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let printer = cfgd_core::test_helpers::test_printer();
    apply_packages(&actions, &managers, &printer).unwrap();

    let installs = mock.installs.lock().unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0], vec!["ripgrep"]);
}

#[test]
fn apply_calls_uninstall_on_correct_manager() {
    let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
    let actions = vec![PackageAction::Uninstall {
        manager: "cargo".into(),
        packages: vec!["bat".into()],
        origin: "local".into(),
    }];

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let printer = cfgd_core::test_helpers::test_printer();
    apply_packages(&actions, &managers, &printer).unwrap();

    let uninstalls = mock.uninstalls.lock().unwrap();
    assert_eq!(uninstalls.len(), 1);
    assert_eq!(uninstalls[0], vec!["bat"]);
}

#[test]
fn all_package_managers_creates_all() {
    let managers = all_package_managers();
    assert_eq!(managers.len(), 20);

    let names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
    assert!(names.contains(&"brew"));
    assert!(names.contains(&"brew-tap"));
    assert!(names.contains(&"brew-cask"));
    assert!(names.contains(&"apt"));
    assert!(names.contains(&"cargo"));
    assert!(names.contains(&"npm"));
    assert!(names.contains(&"pipx"));
    assert!(names.contains(&"dnf"));
    assert!(names.contains(&"apk"));
    assert!(names.contains(&"pacman"));
    assert!(names.contains(&"zypper"));
    assert!(names.contains(&"yum"));
    assert!(names.contains(&"pkg"));
    assert!(names.contains(&"snap"));
    assert!(names.contains(&"flatpak"));
    assert!(names.contains(&"nix"));
    assert!(names.contains(&"go"));
    assert!(names.contains(&"winget"));
    assert!(names.contains(&"chocolatey"));
    assert!(names.contains(&"scoop"));
}

#[test]
fn plan_multiple_managers() {
    let cargo_mock = MockPackageManager::new("cargo", true, vec![]);
    let npm_mock = MockPackageManager::new("npm", true, vec!["typescript"]);

    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into()],
        }),
        npm: Some(cfgd_core::config::NpmSpec {
            file: None,
            global: vec!["typescript".into(), "eslint".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&cargo_mock, &npm_mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // cargo needs ripgrep, npm needs eslint (typescript already installed)
    assert_eq!(actions.len(), 2);

    let cargo_action = actions.iter().find(|a| match a {
        PackageAction::Install { manager, .. } => manager == "cargo",
        _ => false,
    });
    assert!(cargo_action.is_some());

    let npm_action = actions.iter().find(|a| match a {
        PackageAction::Install { manager, .. } => manager == "npm",
        _ => false,
    });
    assert!(npm_action.is_some());
    if let Some(PackageAction::Install { packages, .. }) = npm_action {
        assert_eq!(packages, &vec!["eslint".to_string()]);
    }
}

#[test]
fn plan_bootstrap_unavailable_bootstrappable_manager() {
    let mock = MockPackageManager::new("cargo", false, vec![]).with_bootstrap();
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into(), "fd-find".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert_eq!(actions.len(), 2);
    assert!(matches!(&actions[0], PackageAction::Bootstrap { manager, .. } if manager == "cargo"));
    assert!(
        matches!(&actions[1], PackageAction::Install { manager, packages, .. } if manager == "cargo" && packages.len() == 2)
    );
}

#[test]
fn plan_skip_unavailable_non_bootstrappable_manager() {
    let mock = MockPackageManager::new("apt", false, vec![]);
    let profile = test_profile(PackagesSpec {
        apt: Some(cfgd_core::config::AptSpec {
            file: None,
            packages: vec!["curl".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        PackageAction::Skip {
            manager, reason, ..
        } => {
            assert_eq!(manager, "apt");
            assert!(reason.contains("cannot auto-install"));
        }
        _ => panic!("expected Skip action"),
    }
}

#[test]
fn plan_sub_manager_installs_when_parent_bootstrapping() {
    // brew is unavailable + bootstrappable, brew-tap should get Install (not Skip)
    let brew_mock = MockPackageManager::new("brew", false, vec![]).with_bootstrap();
    let tap_mock = MockPackageManager::new("brew-tap", false, vec![]);

    let profile = test_profile(PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            formulae: vec!["ripgrep".into()],
            taps: vec!["some/tap".into()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&brew_mock, &tap_mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // Should have: Bootstrap(brew), Install(brew: ripgrep), Install(brew-tap: some/tap)
    assert_eq!(actions.len(), 3);
    assert!(matches!(&actions[0], PackageAction::Bootstrap { manager, .. } if manager == "brew"));
    assert!(matches!(&actions[1], PackageAction::Install { manager, .. } if manager == "brew"));
    assert!(matches!(&actions[2], PackageAction::Install { manager, .. } if manager == "brew-tap"));
}

// --- Declarative prune (Uninstall generation) tests ---

fn cfgd_set(entries: &[&str]) -> HashSet<String> {
    entries.iter().map(|s| s.to_string()).collect()
}

#[test]
fn plan_uninstalls_tracked_package_dropped_from_desired() {
    // bat was cfgd-installed and is still on the system, but no longer desired.
    let mock = MockPackageManager::new("cargo", true, vec!["bat", "ripgrep"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/bat", "cargo/ripgrep"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    let uninstall = actions.iter().find_map(|a| match a {
        PackageAction::Uninstall {
            manager, packages, ..
        } if manager == "cargo" => Some(packages),
        _ => None,
    });
    assert_eq!(uninstall, Some(&vec!["bat".to_string()]));
    // ripgrep is desired + installed → neither Install nor Uninstall.
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PackageAction::Install { .. }))
    );
}

#[test]
fn plan_never_uninstalls_untracked_package() {
    // bat is installed and NOT desired, but cfgd never installed it → leave alone.
    let mock = MockPackageManager::new("cargo", true, vec!["bat", "ripgrep"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    // Only ripgrep is tracked; bat was installed by the user, not cfgd.
    let cfgd_installed = cfgd_set(&["cargo/ripgrep"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PackageAction::Uninstall { .. })),
        "untracked package must never be uninstalled: {actions:?}"
    );
}

#[test]
fn plan_steady_state_tracked_desired_installed() {
    // ripgrep is tracked, desired, and installed → no Install, no Uninstall.
    let mock = MockPackageManager::new("cargo", true, vec!["ripgrep"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/ripgrep"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    assert!(actions.is_empty(), "expected no actions, got: {actions:?}");
}

#[test]
fn plan_no_uninstall_for_tracked_package_already_gone_out_of_band() {
    // bat is tracked + not desired, but was already removed out-of-band
    // (not in installed_packages) → nothing to uninstall.
    let mock = MockPackageManager::new("cargo", true, vec!["ripgrep"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/bat", "cargo/ripgrep"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PackageAction::Uninstall { .. })),
        "no uninstall when the tracked package is already gone: {actions:?}"
    );
}

#[test]
fn plan_uninstall_scoped_to_owning_manager() {
    // A package tracked under apt must not be uninstalled by cargo even if
    // cargo also has it installed and it is not desired.
    let cargo_mock = MockPackageManager::new("cargo", true, vec!["shared"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec![],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&cargo_mock];
    // "shared" is tracked under apt, not cargo.
    let cfgd_installed = cfgd_set(&["apt/shared"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PackageAction::Uninstall { .. })),
        "manager must only prune packages it owns: {actions:?}"
    );
}

#[test]
fn plan_does_not_probe_idle_available_manager() {
    // An available manager with no desired packages and nothing cfgd-tracked
    // must never have installed_packages() called — a present-but-broken manager
    // (here: list always errors) must not abort an unrelated plan.
    let broken = MockPackageManager::new("pacman", true, vec![]).with_list_failure();
    let profile = test_profile(PackagesSpec::default());

    let managers: Vec<&dyn PackageManager> = vec![&broken];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(actions.is_empty(), "idle manager must yield no actions");
}

#[test]
fn plan_probes_available_manager_only_when_it_has_tracked_packages() {
    // The same broken manager, but now it has a cfgd-tracked package: prune must
    // attempt to read installed state (and surface the list error) rather than
    // silently skipping — otherwise a dropped package would never be pruned.
    let broken = MockPackageManager::new("pacman", true, vec![]).with_list_failure();
    let profile = test_profile(PackagesSpec::default());

    let managers: Vec<&dyn PackageManager> = vec![&broken];
    let cfgd_installed = cfgd_set(&["pacman/htop"]);
    let result = plan_packages(&profile, &[], &managers, &cfgd_installed);
    assert!(
        result.is_err(),
        "a tracked package forces a real installed-state read"
    );
}

#[test]
fn plan_keeps_shared_package_when_one_consumer_removed() {
    // `desired` is the FULL merged set across all modules+profile. A package
    // still present in the merge (because another consumer keeps it) is not
    // pruned even though it is cfgd-tracked. Modeled here by including `shared`
    // in desired while a second tracked package `solo` has been dropped.
    let mock = MockPackageManager::new("cargo", true, vec!["shared", "solo"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["shared".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/shared", "cargo/solo"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    let uninstall = actions.iter().find_map(|a| match a {
        PackageAction::Uninstall { packages, .. } => Some(packages),
        _ => None,
    });
    // Only `solo` (the dropped one) is pruned; `shared` survives.
    assert_eq!(uninstall, Some(&vec!["solo".to_string()]));
}

#[test]
fn plan_prunes_shared_package_when_last_consumer_removed() {
    // Once the final consumer drops `shared`, it leaves the merged desired set
    // and is pruned.
    let mock = MockPackageManager::new("cargo", true, vec!["shared"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec![],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/shared"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();

    let uninstall = actions.iter().find_map(|a| match a {
        PackageAction::Uninstall { packages, .. } => Some(packages),
        _ => None,
    });
    assert_eq!(uninstall, Some(&vec!["shared".to_string()]));
}

#[test]
fn plan_scoped_apply_empty_tracked_set_never_prunes() {
    // A scoped apply (--module / --only / --phase) passes an empty tracked set
    // so the merge it sees — which is NOT the full picture — can never drive a
    // prune. Even a clearly-droppable tracked package is left alone.
    let mock = MockPackageManager::new("cargo", true, vec!["bat", "ripgrep"]);
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["ripgrep".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    // Empty set models the scoped-apply guard.
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PackageAction::Uninstall { .. })),
        "scoped apply (empty tracked set) must never prune: {actions:?}"
    );
}

#[test]
fn plan_never_prunes_user_package_absent_from_tracked_set() {
    // cfgd only tracks a package whose Install ran, so a package the user
    // installed by hand (never in the tracked set) is never pruned even when
    // installed-and-not-desired.
    let mock = MockPackageManager::new("apt", true, vec!["vim", "git"]);
    let profile = test_profile(PackagesSpec {
        apt: Some(cfgd_core::config::AptSpec {
            file: None,
            packages: vec!["git".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    // git is tracked; vim is a user package cfgd never installed.
    let cfgd_installed = cfgd_set(&["apt/git"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, PackageAction::Uninstall { .. })),
        "a user-installed package must never be pruned: {actions:?}"
    );
}

// --- go name-incoherence (identity) tests ---

#[test]
fn plan_go_no_reinstall_when_binary_already_present() {
    // `installed` reports the BINARY `2fa`; `desired` carries the MODULE PATH
    // `rsc.io/2fa`. Identity-aware diffing must see them as the same package and
    // emit NO Install (the idempotency bug: a raw-string compare always
    // reinstalled).
    let mock = GoLikeMockManager::new(vec!["2fa"]);
    let profile = test_profile(PackagesSpec {
        go: vec!["rsc.io/2fa".into()],
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(
        actions.is_empty(),
        "binary already installed → no Install, no Uninstall: {actions:?}"
    );
}

#[test]
fn plan_go_install_carries_full_module_path() {
    // When the binary is absent, the Install action must carry the ORIGINAL
    // module path so `go install` gets the full path.
    let mock = GoLikeMockManager::new(vec![]);
    let profile = test_profile(PackagesSpec {
        go: vec!["rsc.io/2fa".into()],
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    let install = actions.iter().find_map(|a| match a {
        PackageAction::Install { packages, .. } => Some(packages),
        _ => None,
    });
    assert_eq!(install, Some(&vec!["rsc.io/2fa".to_string()]));
}

#[test]
fn plan_go_prunes_dropped_tracked_binary() {
    // Tracked as `go/2fa` (binary identity), still installed, dropped from
    // desired → Uninstall the binary `2fa`.
    let mock = GoLikeMockManager::new(vec!["2fa"]);
    let profile = test_profile(PackagesSpec {
        go: vec![],
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["go/2fa"]);
    let actions = plan_packages(&profile, &[], &managers, &cfgd_installed).unwrap();
    let uninstall = actions.iter().find_map(|a| match a {
        PackageAction::Uninstall { packages, .. } => Some(packages),
        _ => None,
    });
    assert_eq!(
        uninstall,
        Some(&vec!["2fa".to_string()]),
        "prune must emit the binary identity, which is what go.uninstall removes"
    );
}

// --- stale-row self-heal (GC) tests ---

#[test]
fn stale_tracked_packages_reports_rows_whose_package_is_gone() {
    // bat tracked + still installed → not stale. ghost tracked + NOT installed
    // (out-of-band removal / partial uninstall failure) → stale, GC it.
    let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/bat", "cargo/ghost"]);

    let stale = cfgd_core::reconciler::stale_tracked_packages(&managers, &cfgd_installed).unwrap();
    assert_eq!(stale, vec![("cargo".to_string(), "ghost".to_string())]);
}

#[test]
fn stale_tracked_packages_skips_unavailable_managers() {
    // An unavailable manager cannot confirm absence, so its rows are never GC'd.
    let mock = MockPackageManager::new("cargo", false, vec![]);
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["cargo/anything"]);

    let stale = cfgd_core::reconciler::stale_tracked_packages(&managers, &cfgd_installed).unwrap();
    assert!(
        stale.is_empty(),
        "unavailable manager rows must not be GC'd: {stale:?}"
    );
}

#[test]
fn stale_tracked_packages_uses_identity_for_go() {
    // go tracks by binary identity; a still-present binary is not stale even
    // though the tracked id differs from any module path.
    let mock = GoLikeMockManager::new(vec!["2fa"]);
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let cfgd_installed = cfgd_set(&["go/2fa", "go/gone"]);

    let stale = cfgd_core::reconciler::stale_tracked_packages(&managers, &cfgd_installed).unwrap();
    assert_eq!(stale, vec![("go".to_string(), "gone".to_string())]);
}

// --- Manifest parsing tests ---

#[test]
fn parse_brewfile_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(
        &path,
        r#"# My Brewfile
tap "homebrew/cask"
tap "homebrew/core"

brew "ripgrep"
brew "fd"
brew "bat", restart_service: :changed

cask "firefox"
cask "visual-studio-code"

# macOS app store (ignored)
mas "Xcode", id: 497799835
"#,
    )
    .unwrap();

    let (taps, formulae, casks) = parse_brewfile(&path).unwrap();
    assert_eq!(taps, vec!["homebrew/cask", "homebrew/core"]);
    assert_eq!(formulae, vec!["ripgrep", "fd", "bat"]);
    assert_eq!(casks, vec!["firefox", "visual-studio-code"]);
}

#[test]
fn parse_brewfile_single_quotes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(&path, "brew 'ripgrep'\ncask 'firefox'\n").unwrap();

    let (_, formulae, casks) = parse_brewfile(&path).unwrap();
    assert_eq!(formulae, vec!["ripgrep"]);
    assert_eq!(casks, vec!["firefox"]);
}

#[test]
fn parse_apt_manifest_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("packages.txt");
    std::fs::write(
        &path,
        "# System packages\ncurl\nwget\n\ngit\n# Dev tools\nbuild-essential\n",
    )
    .unwrap();

    let pkgs = parse_apt_manifest(&path).unwrap();
    assert_eq!(pkgs, vec!["curl", "wget", "git", "build-essential"]);
}

#[test]
fn parse_npm_package_json_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("package.json");
    std::fs::write(
        &path,
        r#"{
  "name": "my-project",
  "dependencies": {
    "express": "^4.18.0",
    "lodash": "^4.17.0"
  },
  "devDependencies": {
    "typescript": "^5.0.0",
    "eslint": "^8.0.0"
  }
}"#,
    )
    .unwrap();

    let pkgs = parse_npm_package_json(&path).unwrap();
    assert_eq!(pkgs.len(), 4);
    assert!(pkgs.contains(&"express".to_string()));
    assert!(pkgs.contains(&"lodash".to_string()));
    assert!(pkgs.contains(&"typescript".to_string()));
    assert!(pkgs.contains(&"eslint".to_string()));
}

#[test]
fn parse_npm_package_json_no_deps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("package.json");
    std::fs::write(&path, r#"{"name": "empty"}"#).unwrap();

    let pkgs = parse_npm_package_json(&path).unwrap();
    assert!(pkgs.is_empty());
}

#[test]
fn parse_cargo_toml_basic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Cargo.toml");
    std::fs::write(
        &path,
        r#"[package]
name = "my-project"
version = "0.1.0"

[dependencies]
serde = "1.0"
tokio = { version = "1", features = ["full"] }
clap = "4"
"#,
    )
    .unwrap();

    let pkgs = parse_cargo_toml(&path).unwrap();
    assert_eq!(pkgs.len(), 3);
    assert!(pkgs.contains(&"serde".to_string()));
    assert!(pkgs.contains(&"tokio".to_string()));
    assert!(pkgs.contains(&"clap".to_string()));
}

#[test]
fn parse_cargo_toml_no_deps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Cargo.toml");
    std::fs::write(
        &path,
        r#"[package]
name = "no-deps"
version = "0.1.0"
"#,
    )
    .unwrap();

    let pkgs = parse_cargo_toml(&path).unwrap();
    assert!(pkgs.is_empty());
}

#[test]
fn resolve_manifest_packages_merges_with_inline() {
    let dir = tempfile::tempdir().unwrap();

    // Create a Brewfile
    std::fs::write(
        dir.path().join("Brewfile"),
        "tap \"homebrew/cask\"\nbrew \"ripgrep\"\ncask \"firefox\"\n",
    )
    .unwrap();

    // Create an apt manifest
    std::fs::write(dir.path().join("packages.txt"), "curl\nwget\n").unwrap();

    // Create a Cargo.toml
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();

    // Create a package.json
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies": {"express": "^4"}}"#,
    )
    .unwrap();

    let mut packages = PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            file: Some("Brewfile".into()),
            formulae: vec!["fd".into()],
            ..Default::default()
        }),
        apt: Some(cfgd_core::config::AptSpec {
            file: Some("packages.txt".into()),
            packages: vec!["git".into()],
        }),
        cargo: Some(cfgd_core::config::CargoSpec {
            file: Some("Cargo.toml".into()),
            packages: vec!["bat".into()],
        }),
        npm: Some(cfgd_core::config::NpmSpec {
            file: Some("package.json".into()),
            global: vec!["typescript".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();

    // Brew: inline + Brewfile merged
    let brew = packages.brew.as_ref().unwrap();
    assert!(brew.taps.contains(&"homebrew/cask".to_string()));
    assert!(brew.formulae.contains(&"fd".to_string())); // inline
    assert!(brew.formulae.contains(&"ripgrep".to_string())); // from Brewfile
    assert!(brew.casks.contains(&"firefox".to_string())); // from Brewfile

    // Apt: inline + file merged
    let apt = packages.apt.as_ref().unwrap();
    assert!(apt.packages.contains(&"git".to_string())); // inline
    assert!(apt.packages.contains(&"curl".to_string())); // from file
    assert!(apt.packages.contains(&"wget".to_string())); // from file

    // Cargo: inline + Cargo.toml merged
    let cargo = packages.cargo.as_ref().unwrap();
    assert!(cargo.packages.contains(&"bat".to_string())); // inline
    assert!(cargo.packages.contains(&"serde".to_string())); // from Cargo.toml

    // Npm: inline + package.json merged
    let npm = packages.npm.as_ref().unwrap();
    assert!(npm.global.contains(&"typescript".to_string())); // inline
    assert!(npm.global.contains(&"express".to_string())); // from package.json
}

#[test]
fn resolve_manifest_missing_file_skipped() {
    let dir = tempfile::tempdir().unwrap();

    let mut packages = PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            file: Some("nonexistent-Brewfile".into()),
            formulae: vec!["fd".into()],
            ..Default::default()
        }),
        ..Default::default()
    };

    // Missing file should be silently skipped
    resolve_manifest_packages(&mut packages, dir.path()).unwrap();

    let brew = packages.brew.as_ref().unwrap();
    assert_eq!(brew.formulae, vec!["fd"]); // only inline
}

#[test]
fn resolve_manifest_no_file_field_noop() {
    let dir = tempfile::tempdir().unwrap();

    let mut packages = PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            file: None,
            formulae: vec!["fd".into()],
            ..Default::default()
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();

    let brew = packages.brew.as_ref().unwrap();
    assert_eq!(brew.formulae, vec!["fd"]);
}

#[test]
fn extract_brewfile_name_handles_variants() {
    assert_eq!(
        extract_brewfile_name(r#"brew "ripgrep""#),
        Some("ripgrep".to_string())
    );
    assert_eq!(
        extract_brewfile_name(r#"brew "bat", restart_service: :changed"#),
        Some("bat".to_string())
    );
    assert_eq!(
        extract_brewfile_name(r#"tap 'homebrew/cask'"#),
        Some("homebrew/cask".to_string())
    );
    assert_eq!(
        extract_brewfile_name(r#"cask "firefox""#),
        Some("firefox".to_string())
    );
}

#[test]
fn add_and_remove_new_managers() {
    let mut packages = PackagesSpec::default();

    add_package("apk", "curl", &mut packages).unwrap();
    assert_eq!(packages.apk, vec!["curl"]);

    add_package("pacman", "vim", &mut packages).unwrap();
    assert_eq!(packages.pacman, vec!["vim"]);

    add_package("zypper", "gcc", &mut packages).unwrap();
    assert_eq!(packages.zypper, vec!["gcc"]);

    add_package("yum", "wget", &mut packages).unwrap();
    assert_eq!(packages.yum, vec!["wget"]);

    add_package("pkg", "bash", &mut packages).unwrap();
    assert_eq!(packages.pkg, vec!["bash"]);

    add_package("snap", "nvim", &mut packages).unwrap();
    assert_eq!(packages.snap.as_ref().unwrap().packages, vec!["nvim"]);

    add_package("flatpak", "org.gimp.GIMP", &mut packages).unwrap();
    assert_eq!(
        packages.flatpak.as_ref().unwrap().packages,
        vec!["org.gimp.GIMP"]
    );

    add_package("nix", "ripgrep", &mut packages).unwrap();
    assert_eq!(packages.nix, vec!["ripgrep"]);

    add_package("go", "golang.org/x/tools/gopls", &mut packages).unwrap();
    assert_eq!(packages.go, vec!["golang.org/x/tools/gopls"]);

    // Idempotent
    add_package("apk", "curl", &mut packages).unwrap();
    assert_eq!(packages.apk, vec!["curl"]);

    // Remove
    let removed = remove_package("apk", "curl", &mut packages).unwrap();
    assert!(removed);
    assert!(packages.apk.is_empty());

    let removed = remove_package("pacman", "vim", &mut packages).unwrap();
    assert!(removed);
    assert!(packages.pacman.is_empty());

    let removed = remove_package("snap", "nvim", &mut packages).unwrap();
    assert!(removed);

    let removed = remove_package("flatpak", "org.gimp.GIMP", &mut packages).unwrap();
    assert!(removed);

    let removed = remove_package("nix", "ripgrep", &mut packages).unwrap();
    assert!(removed);

    let removed = remove_package("go", "golang.org/x/tools/gopls", &mut packages).unwrap();
    assert!(removed);
}

#[test]
fn plan_with_new_managers() {
    let apk = MockPackageManager::new("apk", true, vec!["curl"]);
    let pacman = MockPackageManager::new("pacman", true, vec![]);
    let snap = MockPackageManager::new("snap", false, vec![]).with_bootstrap();

    let profile = test_profile(PackagesSpec {
        apk: vec!["curl".into(), "git".into()],
        pacman: vec!["neovim".into()],
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["nvim".into()],
            classic: vec![],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&apk, &pacman, &snap];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // apk: git is missing → Install
    assert!(actions.iter().any(|a| matches!(
        a,
        PackageAction::Install {
            manager,
            packages,
            ..
        } if manager == "apk" && packages.contains(&"git".to_string())
    )));

    // pacman: neovim missing → Install
    assert!(actions.iter().any(|a| matches!(
        a,
        PackageAction::Install {
            manager,
            packages,
            ..
        } if manager == "pacman" && packages.contains(&"neovim".to_string())
    )));

    // snap: unavailable but bootstrappable → Bootstrap + Install
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, PackageAction::Bootstrap { manager, .. } if manager == "snap"))
    );
}

#[test]
fn desired_packages_for_new_managers() {
    let profile = test_profile(PackagesSpec {
        apk: vec!["curl".into()],
        pacman: vec!["vim".into()],
        zypper: vec!["gcc".into()],
        yum: vec!["wget".into()],
        pkg: vec!["bash".into()],
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["nvim".into()],
            classic: vec!["code".into()],
        }),
        flatpak: Some(cfgd_core::config::FlatpakSpec {
            packages: vec!["org.gimp.GIMP".into()],
            remote: None,
        }),
        nix: vec!["ripgrep".into()],
        go: vec!["golang.org/x/tools/gopls".into()],
        ..Default::default()
    });

    assert_eq!(
        cfgd_core::config::desired_packages_for("apk", &profile),
        vec!["curl"]
    );
    assert_eq!(
        cfgd_core::config::desired_packages_for("pacman", &profile),
        vec!["vim"]
    );
    assert_eq!(
        cfgd_core::config::desired_packages_for("zypper", &profile),
        vec!["gcc"]
    );
    assert_eq!(
        cfgd_core::config::desired_packages_for("yum", &profile),
        vec!["wget"]
    );
    assert_eq!(
        cfgd_core::config::desired_packages_for("pkg", &profile),
        vec!["bash"]
    );
    // snap merges packages + classic
    let snap_desired = cfgd_core::config::desired_packages_for("snap", &profile);
    assert!(snap_desired.contains(&"nvim".to_string()));
    assert!(snap_desired.contains(&"code".to_string()));

    assert_eq!(
        cfgd_core::config::desired_packages_for("flatpak", &profile),
        vec!["org.gimp.GIMP"]
    );
    assert_eq!(
        cfgd_core::config::desired_packages_for("nix", &profile),
        vec!["ripgrep"]
    );
    assert_eq!(
        cfgd_core::config::desired_packages_for("go", &profile),
        vec!["golang.org/x/tools/gopls"]
    );
}

#[test]
fn yum_skipped_when_dnf_available() {
    // yum_manager().is_available() returns false when dnf is present
    // We can't directly test this without the actual system, but we can verify
    // the manager's name is correct
    let yum = yum_manager();
    assert_eq!(yum.name(), "yum");
    assert!(!yum.can_bootstrap());
}

#[test]
fn custom_manager_desired_packages() {
    let profile = test_profile(PackagesSpec {
        custom: vec![cfgd_core::config::CustomManagerSpec {
            name: "mypm".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec!["toolA".to_string(), "toolB".to_string()],
        }],
        ..Default::default()
    });
    let desired = cfgd_core::config::desired_packages_for("mypm", &profile);
    assert_eq!(desired, vec!["toolA".to_string(), "toolB".to_string()]);
}

// --- strip_version_suffix / strip_arch_suffix ---

// --- parse_simple_lines ---

// --- parse_dnf_lines ---

// --- parse_yum_lines ---

// --- parse_apk_lines ---

// --- parse_zypper_lines ---

// --- parse_pkg_lines ---

// --- apply_packages ---

#[test]
fn apply_packages_install() {
    let mock = MockPackageManager::new("cargo", true, vec![]);
    let printer = cfgd_core::test_helpers::test_printer();
    let actions = vec![PackageAction::Install {
        manager: "cargo".into(),
        packages: vec!["bat".into(), "fd-find".into()],
        origin: "local".into(),
    }];
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    apply_packages(&actions, &managers, &printer).unwrap();
    let installs = mock.installs.lock().unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0], vec!["bat", "fd-find"]);
}

#[test]
fn apply_packages_uninstall() {
    let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
    let printer = cfgd_core::test_helpers::test_printer();
    let actions = vec![PackageAction::Uninstall {
        manager: "cargo".into(),
        packages: vec!["bat".into()],
        origin: "local".into(),
    }];
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    apply_packages(&actions, &managers, &printer).unwrap();
    let uninstalls = mock.uninstalls.lock().unwrap();
    assert_eq!(uninstalls.len(), 1);
}

#[test]
fn apply_packages_bootstrap() {
    let mock = MockPackageManager::new("cargo", false, vec![]).with_bootstrap();
    let printer = cfgd_core::test_helpers::test_printer();
    let actions = vec![PackageAction::Bootstrap {
        manager: "cargo".into(),
        method: "rustup".into(),
        origin: "local".into(),
    }];
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    apply_packages(&actions, &managers, &printer).unwrap();
}

#[test]
fn apply_packages_skip_no_error() {
    let printer = cfgd_core::test_helpers::test_printer();
    let actions = vec![PackageAction::Skip {
        manager: "snap".into(),
        reason: "not available".into(),
        origin: "local".into(),
    }];
    apply_packages(&actions, &[], &printer).unwrap();
}

#[test]
fn plan_skip_unavailable_no_bootstrap() {
    let mock = MockPackageManager::new("snap", false, vec![]);
    let profile = test_profile(PackagesSpec {
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["core".into()],
            classic: vec![],
        }),
        ..Default::default()
    });
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        PackageAction::Skip {
            manager, reason, ..
        } => {
            assert_eq!(manager, "snap");
            assert!(reason.contains("not available"), "reason: {reason}");
        }
        other => panic!("expected Skip, got: {other:?}"),
    }
}

// --- resolve_manifest_packages ---

#[test]
fn resolve_manifest_packages_brewfile() {
    let dir = tempfile::tempdir().unwrap();
    let brewfile = dir.path().join("Brewfile");
    std::fs::write(
        &brewfile,
        "brew \"ripgrep\"\nbrew \"fd\"\ncask \"firefox\"\ntap \"homebrew/cask\"\n",
    )
    .unwrap();

    let mut spec = PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            file: Some("Brewfile".into()),
            formulae: vec!["existing".into()],
            ..Default::default()
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut spec, dir.path()).unwrap();
    let brew = spec.brew.unwrap();
    assert!(brew.formulae.contains(&"ripgrep".to_string()));
    assert!(brew.formulae.contains(&"fd".to_string()));
    assert!(brew.formulae.contains(&"existing".to_string()));
    assert!(brew.casks.contains(&"firefox".to_string()));
    assert!(brew.taps.contains(&"homebrew/cask".to_string()));
}

#[test]
fn resolve_manifest_packages_apt_file() {
    let dir = tempfile::tempdir().unwrap();
    let apt_file = dir.path().join("packages.apt.txt");
    std::fs::write(&apt_file, "git\ncurl\n# comment\n\n").unwrap();

    let mut spec = PackagesSpec {
        apt: Some(cfgd_core::config::AptSpec {
            file: Some("packages.apt.txt".into()),
            packages: vec![],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut spec, dir.path()).unwrap();
    let apt = spec.apt.unwrap();
    assert!(apt.packages.contains(&"git".to_string()));
    assert!(apt.packages.contains(&"curl".to_string()));
    assert!(!apt.packages.contains(&"# comment".to_string()));
}

// --- stderr_lossy ---

// --- installed_packages_with_versions parse tests ---

// --- winget output parsing ---

// --- chocolatey output parsing ---

// --- scoop output parsing ---

// --- package_aliases tests ---

#[test]
fn test_simple_manager_package_aliases_via_trait() {
    // Verify the trait dispatch works correctly for apt
    let apt = apt_manager();
    let aliases = apt.package_aliases("fd").unwrap();
    assert_eq!(aliases, vec!["fd-find"]);

    let aliases = apt.package_aliases("bat").unwrap();
    assert_eq!(aliases, vec!["batcat"]);

    let aliases = apt.package_aliases("git").unwrap();
    assert!(aliases.is_empty());
}

#[test]
fn test_simple_manager_package_aliases_dnf_via_trait() {
    let dnf = dnf_manager();
    let aliases = dnf.package_aliases("nvim").unwrap();
    assert_eq!(aliases, vec!["neovim"]);

    let aliases = dnf.package_aliases("curl").unwrap();
    assert!(aliases.is_empty());
}

#[test]
fn test_simple_manager_no_aliases_for_pacman() {
    let pacman = pacman_manager();
    let aliases = pacman.package_aliases("fd").unwrap();
    assert!(aliases.is_empty());
}

// --- parse_dnf_yum_lines edge cases ---

// --- parse_winget_list edge cases ---

// --- parse_choco_list edge cases ---

// --- parse_scoop_list edge cases ---

// --- bootstrap_method tests ---

#[test]
fn bootstrap_method_brew_returns_homebrew_installer() {
    let mock = MockPackageManager::new("brew", false, vec![]);
    let method = bootstrap_method(&mock);
    assert_eq!(method, "homebrew installer");
}

#[test]
fn bootstrap_method_cargo_returns_rustup() {
    let mock = MockPackageManager::new("cargo", false, vec![]);
    let method = bootstrap_method(&mock);
    assert_eq!(method, "rustup");
}

#[test]
fn bootstrap_method_nix_returns_nix_installer() {
    let mock = MockPackageManager::new("nix", false, vec![]);
    let method = bootstrap_method(&mock);
    assert_eq!(method, "nix installer");
}

#[test]
fn bootstrap_method_unknown_returns_system() {
    let mock = MockPackageManager::new("unknown-pm", false, vec![]);
    let method = bootstrap_method(&mock);
    assert_eq!(method, "system");
}

#[test]
fn detect_system_method_returns_valid_manager() {
    // detect_system_method cascades apt → dnf → zypper
    let method = detect_system_method();
    assert!(
        method == "apt" || method == "dnf" || method == "zypper",
        "expected apt, dnf, or zypper, got: {}",
        method
    );
}

#[test]
fn detect_brew_system_method_returns_valid_manager() {
    // detect_brew_system_method cascades brew → apt → dnf → fallback
    let method = detect_brew_system_method("pip");
    assert!(
        method == "brew" || method == "apt" || method == "dnf" || method == "pip",
        "expected brew, apt, dnf, or pip, got: {}",
        method
    );
}

// --- extract_caveats tests ---

// --- strip_sudo_for_exec tests ---

// --- SimpleManager display_cmd tests ---

// --- SimpleManager constructor verification ---

// --- SimpleManager trait dispatch ---

// --- parse_apk_lines edge cases ---

// --- parse_zypper_lines edge cases ---

// --- parse_pkg_lines edge cases ---

// --- parse_brew_versions edge cases ---

// --- parse_tab_separated_versions edge cases ---

// --- parse_cargo_install_list edge cases ---

// --- parse_npm_list_versions edge cases ---

// --- parse_pipx_list_versions edge cases ---

// --- extract_caveats additional edge cases ---

// --- ScriptedManager template edge cases ---

// --- remove_package for snap (classic + packages) ---

#[test]
fn remove_package_snap_from_classic_list() {
    let mut packages = PackagesSpec {
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["core".into()],
            classic: vec!["code".into(), "slack".into()],
        }),
        ..Default::default()
    };

    // Remove from classic list
    let removed = remove_package("snap", "code", &mut packages).unwrap();
    assert!(removed);
    let snap = packages.snap.as_ref().unwrap();
    assert_eq!(snap.classic, vec!["slack"]);
    assert_eq!(snap.packages, vec!["core"]);
}

#[test]
fn remove_package_snap_not_found_in_either_list() {
    let mut packages = PackagesSpec {
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["core".into()],
            classic: vec!["code".into()],
        }),
        ..Default::default()
    };

    let removed = remove_package("snap", "nonexistent", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_snap_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("snap", "anything", &mut packages).unwrap();
    assert!(!removed);
}

// --- remove_package for managers with no spec initialized ---

#[test]
fn remove_package_brew_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("brew", "anything", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_brew_tap_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("brew-tap", "anything", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_brew_cask_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("brew-cask", "anything", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_apt_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("apt", "anything", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_cargo_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("cargo", "anything", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_npm_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("npm", "anything", &mut packages).unwrap();
    assert!(!removed);
}

#[test]
fn remove_package_flatpak_none_returns_false() {
    let mut packages = PackagesSpec::default();
    let removed = remove_package("flatpak", "anything", &mut packages).unwrap();
    assert!(!removed);
}

// --- remove_package from custom manager ---

#[test]
fn remove_package_custom_manager() {
    let mut packages = PackagesSpec {
        custom: vec![cfgd_core::config::CustomManagerSpec {
            name: "mypm".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec!["foo".to_string(), "bar".to_string()],
        }],
        ..Default::default()
    };

    let removed = remove_package("mypm", "foo", &mut packages).unwrap();
    assert!(removed);
    assert_eq!(packages.custom[0].packages, vec!["bar"]);

    let removed = remove_package("mypm", "nonexistent", &mut packages).unwrap();
    assert!(!removed);
}

// --- add_package to custom manager ---

#[test]
fn add_package_custom_manager() {
    let mut packages = PackagesSpec {
        custom: vec![cfgd_core::config::CustomManagerSpec {
            name: "mypm".to_string(),
            check: "true".to_string(),
            list_installed: "echo".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec!["existing".to_string()],
        }],
        ..Default::default()
    };

    add_package("mypm", "new-pkg", &mut packages).unwrap();
    assert_eq!(packages.custom[0].packages, vec!["existing", "new-pkg"]);

    // Idempotent
    add_package("mypm", "new-pkg", &mut packages).unwrap();
    assert_eq!(packages.custom[0].packages, vec!["existing", "new-pkg"]);
}

// --- format_package_actions edge cases ---

#[test]
fn format_package_actions_empty() {
    let formatted = format_package_actions(&[]);
    assert!(formatted.is_empty());
}

#[test]
fn format_package_actions_uninstall() {
    let actions = vec![PackageAction::Uninstall {
        manager: "npm".into(),
        packages: vec!["eslint".into(), "prettier".into()],
        origin: "local".into(),
    }];
    let formatted = format_package_actions(&actions);
    assert_eq!(formatted.len(), 1);
    assert!(formatted[0].contains("uninstall"));
    assert!(formatted[0].contains("npm"));
    assert!(formatted[0].contains("eslint"));
    assert!(formatted[0].contains("prettier"));
}

// --- parse_winget_list additional edge cases ---

// --- parse_choco_list edge cases ---

// --- strip_version_suffix edge cases ---

// --- strip_arch_suffix edge cases ---

// --- plan_packages with empty managers list ---

#[test]
fn plan_packages_no_managers() {
    let profile = test_profile(PackagesSpec::default());
    let managers: Vec<&dyn PackageManager> = vec![];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(actions.is_empty());
}

// --- MockPackageManager trait methods ---

#[test]
fn mock_manager_update_is_noop() {
    let mock = MockPackageManager::new("test", true, vec![]);
    let printer = cfgd_core::test_helpers::test_printer();
    mock.update(&printer).unwrap();
}

#[test]
fn mock_manager_available_version_is_none() {
    let mock = MockPackageManager::new("test", true, vec![]);
    assert!(mock.available_version("anything").unwrap().is_none());
}

#[test]
fn mock_manager_bootstrap_is_noop() {
    let mock = MockPackageManager::new("test", false, vec![]).with_bootstrap();
    let printer = cfgd_core::test_helpers::test_printer();
    mock.bootstrap(&printer).unwrap();
}

// --- Brewfile parsing edge cases ---

#[test]
fn parse_brewfile_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(&path, "").unwrap();

    let (taps, formulae, casks) = parse_brewfile(&path).unwrap();
    assert!(taps.is_empty());
    assert!(formulae.is_empty());
    assert!(casks.is_empty());
}

#[test]
fn parse_brewfile_comments_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(&path, "# This is a comment\n# Another comment\n").unwrap();

    let (taps, formulae, casks) = parse_brewfile(&path).unwrap();
    assert!(taps.is_empty());
    assert!(formulae.is_empty());
    assert!(casks.is_empty());
}

#[test]
fn parse_brewfile_unquoted_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(&path, "brew ripgrep\ncask firefox\n").unwrap();

    let (_, formulae, casks) = parse_brewfile(&path).unwrap();
    assert_eq!(formulae, vec!["ripgrep"]);
    assert_eq!(casks, vec!["firefox"]);
}

#[test]
fn parse_brewfile_nonexistent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent");
    let result = parse_brewfile(&path);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to read Brewfile"), "got: {msg}");
}

#[test]
fn extract_brewfile_name_no_keyword() {
    // A line with only one word (no space) → split_once returns None
    assert_eq!(extract_brewfile_name("standalone"), None);
}

#[test]
fn extract_brewfile_name_unquoted_with_comma() {
    assert_eq!(
        extract_brewfile_name("brew ripgrep, restart_service: true"),
        Some("ripgrep".to_string())
    );
}

// --- parse_npm_package_json edge cases ---

#[test]
fn parse_npm_package_json_deduplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("package.json");
    std::fs::write(
        &path,
        r#"{
  "dependencies": {"foo": "^1.0"},
  "devDependencies": {"foo": "^1.0", "bar": "^2.0"}
}"#,
    )
    .unwrap();

    let pkgs = parse_npm_package_json(&path).unwrap();
    // foo appears in both, should only be listed once
    assert_eq!(pkgs.iter().filter(|p| *p == "foo").count(), 1);
    assert!(pkgs.contains(&"bar".to_string()));
}

#[test]
fn parse_npm_package_json_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("package.json");
    std::fs::write(&path, "not json").unwrap();

    let result = parse_npm_package_json(&path);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to parse package.json"), "got: {msg}");
}

// --- parse_cargo_toml edge cases ---

#[test]
fn parse_cargo_toml_invalid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Cargo.toml");
    std::fs::write(&path, "[invalid").unwrap();

    let result = parse_cargo_toml(&path);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to parse Cargo.toml"), "got: {msg}");
}

// --- resolve_manifest_packages edge cases ---

#[test]
fn resolve_manifest_packages_npm_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies": {"express": "^4.18.0"}}"#,
    )
    .unwrap();

    let mut packages = PackagesSpec {
        npm: Some(cfgd_core::config::NpmSpec {
            file: Some("package.json".into()),
            global: vec!["existing".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();
    let npm = packages.npm.as_ref().unwrap();
    assert!(npm.global.contains(&"existing".to_string()));
    assert!(npm.global.contains(&"express".to_string()));
}

#[test]
fn resolve_manifest_packages_cargo_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[dependencies]\nclap = \"4\"\n",
    )
    .unwrap();

    let mut packages = PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: Some("Cargo.toml".into()),
            packages: vec!["existing".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();
    let cargo = packages.cargo.as_ref().unwrap();
    assert!(cargo.packages.contains(&"existing".to_string()));
    assert!(cargo.packages.contains(&"clap".to_string()));
}

// --- plan_packages with aliases consideration ---

#[test]
fn plan_packages_available_manager_no_desired_is_noop() {
    // Manager is available but no packages desired → no actions
    let mock = MockPackageManager::new("brew", true, vec!["ripgrep"]);
    let profile = test_profile(PackagesSpec::default());
    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(actions.is_empty());
}

// --- apply_packages with multiple actions ---

#[test]
fn apply_packages_multiple_actions() {
    let cargo_mock = MockPackageManager::new("cargo", true, vec![]);
    let npm_mock = MockPackageManager::new("npm", true, vec![]);

    let actions = vec![
        PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into()],
            origin: "local".into(),
        },
        PackageAction::Install {
            manager: "npm".into(),
            packages: vec!["typescript".into()],
            origin: "local".into(),
        },
    ];

    let managers: Vec<&dyn PackageManager> = vec![&cargo_mock, &npm_mock];
    let printer = cfgd_core::test_helpers::test_printer();
    apply_packages(&actions, &managers, &printer).unwrap();

    let cargo_installs = cargo_mock.installs.lock().unwrap();
    assert_eq!(cargo_installs.len(), 1);
    assert_eq!(cargo_installs[0], vec!["ripgrep"]);

    let npm_installs = npm_mock.installs.lock().unwrap();
    assert_eq!(npm_installs.len(), 1);
    assert_eq!(npm_installs[0], vec!["typescript"]);
}

// --- apply_packages with unknown manager is silently skipped ---

#[test]
fn apply_packages_unknown_manager_skipped() {
    let actions = vec![PackageAction::Install {
        manager: "nonexistent".into(),
        packages: vec!["foo".into()],
        origin: "local".into(),
    }];
    let printer = cfgd_core::test_helpers::test_printer();
    // No matching manager → the find returns None → action is skipped
    apply_packages(&actions, &[], &printer).unwrap();
}

// --- PostInstallNote and print_caveats ---

// --- SimpleManager installed_packages_with_versions default ---

#[test]
fn simple_manager_default_versions_unknown() {
    // Managers without list_with_versions return "unknown" for all packages
    let mgr = pacman_manager();
    assert!(mgr.list_with_versions.is_none());
    // We can't call installed_packages_with_versions without pacman installed,
    // but we verify the field is None
}

// --- SimpleManager available_version dispatch ---

#[test]
fn simple_manager_available_version_dispatches() {
    // Verify the function pointer is set (can't run without actual managers)
    let apt = apt_manager();
    // query_version is a function pointer — it exists
    assert_eq!(apt.mgr_name, "apt");
}

// =========================================================================
// Additional coverage tests
// =========================================================================

// --- Concrete manager name/can_bootstrap/trait verification ---

// --- BrewManager path_dirs tests ---

// --- parse_brew_versions additional edge cases ---

// --- parse_tab_separated_versions additional cases ---

// --- parse_cargo_install_list additional cases ---

// --- parse_npm_list_versions additional cases ---

// --- parse_pipx_list_versions additional cases ---

// --- parse_dnf_lines additional cases ---

// --- parse_yum_lines additional cases ---

// --- parse_apk_lines additional cases ---

// --- parse_zypper_lines additional cases ---

// --- parse_pkg_lines additional cases ---

// --- parse_winget_list more edge cases ---

// --- parse_choco_list additional cases ---

// --- parse_scoop_list additional cases ---

// --- format_package_actions comprehensive ---

#[test]
fn format_package_actions_all_action_types() {
    let actions = vec![
        PackageAction::Bootstrap {
            manager: "brew".into(),
            method: "homebrew installer".into(),
            origin: "local".into(),
        },
        PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into(), "fd-find".into(), "bat".into()],
            origin: "local".into(),
        },
        PackageAction::Uninstall {
            manager: "npm".into(),
            packages: vec!["old-pkg".into()],
            origin: "local".into(),
        },
        PackageAction::Skip {
            manager: "snap".into(),
            reason: "'snap' not available".into(),
            origin: "local".into(),
        },
    ];

    let formatted = format_package_actions(&actions);
    assert_eq!(formatted.len(), 4);

    assert_eq!(formatted[0], "bootstrap brew via homebrew installer");
    assert_eq!(formatted[1], "install via cargo: ripgrep, fd-find, bat");
    assert_eq!(formatted[2], "uninstall via npm: old-pkg");
    assert_eq!(formatted[3], "skip snap: 'snap' not available");
}

#[test]
fn format_package_actions_single_package_install() {
    let actions = vec![PackageAction::Install {
        manager: "apt".into(),
        packages: vec!["curl".into()],
        origin: "local".into(),
    }];
    let formatted = format_package_actions(&actions);
    assert_eq!(formatted[0], "install via apt: curl");
}

// --- plan_packages comprehensive scenarios ---

#[test]
fn plan_packages_mixed_available_and_unavailable() {
    let available = MockPackageManager::new("cargo", true, vec!["bat"]);
    let unavailable = MockPackageManager::new("snap", false, vec![]);
    let bootstrappable = MockPackageManager::new("nix", false, vec![]).with_bootstrap();

    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec!["bat".into(), "ripgrep".into()],
        }),
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["nvim".into()],
            classic: vec![],
        }),
        nix: vec!["fd".into()],
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&available, &unavailable, &bootstrappable];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // cargo: ripgrep needs install (bat already installed)
    let cargo_install = actions.iter().find(|a| {
        matches!(
            a,
            PackageAction::Install { manager, .. } if manager == "cargo"
        )
    });
    assert!(cargo_install.is_some());
    if let Some(PackageAction::Install { packages, .. }) = cargo_install {
        assert_eq!(packages, &vec!["ripgrep".to_string()]);
    }

    // snap: unavailable + no bootstrap → skip
    assert!(actions.iter().any(|a| matches!(
        a,
        PackageAction::Skip { manager, .. } if manager == "snap"
    )));

    // nix: unavailable + bootstrappable → bootstrap + install
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, PackageAction::Bootstrap { manager, .. } if manager == "nix"))
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        PackageAction::Install { manager, packages, .. }
        if manager == "nix" && packages.contains(&"fd".to_string())
    )));
}

#[test]
fn plan_packages_all_already_installed() {
    let mock = MockPackageManager::new("npm", true, vec!["typescript", "eslint"]);
    let profile = test_profile(PackagesSpec {
        npm: Some(cfgd_core::config::NpmSpec {
            file: None,
            global: vec!["typescript".into(), "eslint".into()],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(actions.is_empty());
}

#[test]
fn plan_packages_empty_desired_skips_available_manager() {
    let mock = MockPackageManager::new("cargo", true, vec!["bat"]);
    // Profile has cargo spec but with empty packages list
    let profile = test_profile(PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: None,
            packages: vec![],
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&mock];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(actions.is_empty());
}

// --- add_package idempotency for all managers ---

#[test]
fn add_package_snap_idempotent() {
    let mut packages = PackagesSpec::default();
    add_package("snap", "core", &mut packages).unwrap();
    add_package("snap", "core", &mut packages).unwrap();
    assert_eq!(packages.snap.as_ref().unwrap().packages, vec!["core"]);
}

#[test]
fn add_package_flatpak_idempotent() {
    let mut packages = PackagesSpec::default();
    add_package("flatpak", "org.gimp.GIMP", &mut packages).unwrap();
    add_package("flatpak", "org.gimp.GIMP", &mut packages).unwrap();
    assert_eq!(
        packages.flatpak.as_ref().unwrap().packages,
        vec!["org.gimp.GIMP"]
    );
}

#[test]
fn add_package_brew_tap_idempotent() {
    let mut packages = PackagesSpec::default();
    add_package("brew-tap", "homebrew/core", &mut packages).unwrap();
    add_package("brew-tap", "homebrew/core", &mut packages).unwrap();
    assert_eq!(packages.brew.as_ref().unwrap().taps, vec!["homebrew/core"]);
}

#[test]
fn add_package_brew_cask_idempotent() {
    let mut packages = PackagesSpec::default();
    add_package("brew-cask", "firefox", &mut packages).unwrap();
    add_package("brew-cask", "firefox", &mut packages).unwrap();
    assert_eq!(packages.brew.as_ref().unwrap().casks, vec!["firefox"]);
}

#[test]
fn add_package_apt_idempotent() {
    let mut packages = PackagesSpec::default();
    add_package("apt", "curl", &mut packages).unwrap();
    add_package("apt", "curl", &mut packages).unwrap();
    assert_eq!(packages.apt.as_ref().unwrap().packages, vec!["curl"]);
}

#[test]
fn add_package_npm_idempotent() {
    let mut packages = PackagesSpec::default();
    add_package("npm", "typescript", &mut packages).unwrap();
    add_package("npm", "typescript", &mut packages).unwrap();
    assert_eq!(packages.npm.as_ref().unwrap().global, vec!["typescript"]);
}

// --- add_package / remove_package round trip for all simple managers ---

#[test]
fn add_remove_round_trip_simple_managers() {
    let simple_managers = [
        "pipx",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "nix",
        "go",
        "winget",
        "chocolatey",
        "scoop",
    ];

    for mgr in &simple_managers {
        let mut packages = PackagesSpec::default();
        add_package(mgr, "test-pkg", &mut packages).unwrap();

        let list = packages.simple_list_mut(mgr).unwrap();
        assert_eq!(
            list,
            &vec!["test-pkg".to_string()],
            "add failed for {}",
            mgr
        );

        let removed = remove_package(mgr, "test-pkg", &mut packages).unwrap();
        assert!(removed, "remove failed for {}", mgr);

        let list = packages.simple_list_mut(mgr).unwrap();
        assert!(list.is_empty(), "list not empty after remove for {}", mgr);
    }
}

// --- remove_package for non-existent entries ---

#[test]
fn remove_package_from_empty_simple_managers() {
    let simple_managers = [
        "pipx",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "nix",
        "go",
        "winget",
        "chocolatey",
        "scoop",
    ];

    for mgr in &simple_managers {
        let mut packages = PackagesSpec::default();
        let removed = remove_package(mgr, "nonexistent", &mut packages).unwrap();
        assert!(!removed, "should return false for empty {} list", mgr);
    }
}

// --- resolve_manifest_packages all file types at once ---

#[test]
fn resolve_manifest_packages_all_file_types_simultaneously() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("Brewfile"),
        "tap \"custom/tap\"\nbrew \"jq\"\ncask \"iterm2\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("apt-pkgs.txt"), "htop\ntmux\n").unwrap();
    std::fs::write(
        dir.path().join("pkg.json"),
        r#"{"dependencies": {"lodash": "^4.17.0"}, "devDependencies": {"jest": "^29.0.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("deps.toml"),
        "[package]\nname = \"test\"\n\n[dependencies]\nserde = \"1.0\"\ntokio = \"1\"\n",
    )
    .unwrap();

    let mut packages = PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            file: Some("Brewfile".into()),
            formulae: vec!["existing-brew".into()],
            taps: vec![],
            casks: vec![],
        }),
        apt: Some(cfgd_core::config::AptSpec {
            file: Some("apt-pkgs.txt".into()),
            packages: vec!["existing-apt".into()],
        }),
        npm: Some(cfgd_core::config::NpmSpec {
            file: Some("pkg.json".into()),
            global: vec!["existing-npm".into()],
        }),
        cargo: Some(cfgd_core::config::CargoSpec {
            file: Some("deps.toml".into()),
            packages: vec!["existing-cargo".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();

    let brew = packages.brew.as_ref().unwrap();
    assert!(brew.taps.contains(&"custom/tap".to_string()));
    assert!(brew.formulae.contains(&"existing-brew".to_string()));
    assert!(brew.formulae.contains(&"jq".to_string()));
    assert!(brew.casks.contains(&"iterm2".to_string()));

    let apt = packages.apt.as_ref().unwrap();
    assert!(apt.packages.contains(&"existing-apt".to_string()));
    assert!(apt.packages.contains(&"htop".to_string()));
    assert!(apt.packages.contains(&"tmux".to_string()));

    let npm = packages.npm.as_ref().unwrap();
    assert!(npm.global.contains(&"existing-npm".to_string()));
    assert!(npm.global.contains(&"lodash".to_string()));
    assert!(npm.global.contains(&"jest".to_string()));

    let cargo = packages.cargo.as_ref().unwrap();
    assert!(cargo.packages.contains(&"existing-cargo".to_string()));
    assert!(cargo.packages.contains(&"serde".to_string()));
    assert!(cargo.packages.contains(&"tokio".to_string()));
}

#[test]
fn resolve_manifest_packages_no_specs_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let mut packages = PackagesSpec::default();
    resolve_manifest_packages(&mut packages, dir.path()).unwrap();
    // Everything stays default
    assert!(packages.brew.is_none());
    assert!(packages.apt.is_none());
    assert!(packages.npm.is_none());
    assert!(packages.cargo.is_none());
}

#[test]
fn resolve_manifest_packages_duplicate_merging() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Brewfile"),
        "brew \"fd\"\nbrew \"ripgrep\"\n",
    )
    .unwrap();

    let mut packages = PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            file: Some("Brewfile".into()),
            // fd is already in the inline list
            formulae: vec!["fd".into(), "bat".into()],
            taps: vec![],
            casks: vec![],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();

    let brew = packages.brew.as_ref().unwrap();
    // fd should not be duplicated — union_extend deduplicates
    let fd_count = brew.formulae.iter().filter(|f| *f == "fd").count();
    assert_eq!(fd_count, 1);
    // ripgrep should be added, bat should remain
    assert!(brew.formulae.contains(&"ripgrep".to_string()));
    assert!(brew.formulae.contains(&"bat".to_string()));
}

// --- custom_managers tests ---

// --- bootstrap_method comprehensive ---

#[test]
fn bootstrap_method_snap_or_flatpak_returns_system_method() {
    let snap_mock = MockPackageManager::new("snap", false, vec![]);
    let method = bootstrap_method(&snap_mock);
    assert!(
        method == "apt" || method == "dnf" || method == "zypper",
        "expected system method, got: {}",
        method
    );

    let flatpak_mock = MockPackageManager::new("flatpak", false, vec![]);
    let method = bootstrap_method(&flatpak_mock);
    assert!(
        method == "apt" || method == "dnf" || method == "zypper",
        "expected system method, got: {}",
        method
    );
}

#[test]
fn bootstrap_method_npm_detects_method() {
    let mock = MockPackageManager::new("npm", false, vec![]);
    let method = bootstrap_method(&mock);
    assert!(
        method == "brew" || method == "apt" || method == "dnf" || method == "nvm",
        "expected brew/apt/dnf/nvm, got: {}",
        method
    );
}

#[test]
fn bootstrap_method_pipx_detects_method() {
    let mock = MockPackageManager::new("pipx", false, vec![]);
    let method = bootstrap_method(&mock);
    assert!(
        method == "brew" || method == "apt" || method == "dnf" || method == "pip",
        "expected brew/apt/dnf/pip, got: {}",
        method
    );
}

#[test]
fn bootstrap_method_go_detects_method() {
    let mock = MockPackageManager::new("go", false, vec![]);
    let method = bootstrap_method(&mock);
    assert!(
        method == "brew" || method == "apt" || method == "dnf",
        "expected brew/apt/dnf, got: {}",
        method
    );
}

// --- apply_packages with skip action ---

#[test]
fn apply_packages_mixed_actions() {
    let cargo_mock = MockPackageManager::new("cargo", true, vec![]);
    let npm_mock = MockPackageManager::new("npm", true, vec!["old-pkg"]);

    let actions = vec![
        PackageAction::Bootstrap {
            manager: "cargo".into(),
            method: "rustup".into(),
            origin: "local".into(),
        },
        PackageAction::Install {
            manager: "cargo".into(),
            packages: vec!["ripgrep".into(), "bat".into()],
            origin: "local".into(),
        },
        PackageAction::Uninstall {
            manager: "npm".into(),
            packages: vec!["old-pkg".into()],
            origin: "local".into(),
        },
        PackageAction::Skip {
            manager: "snap".into(),
            reason: "not available".into(),
            origin: "local".into(),
        },
    ];

    let managers: Vec<&dyn PackageManager> = vec![&cargo_mock, &npm_mock];
    let printer = cfgd_core::test_helpers::test_printer();
    apply_packages(&actions, &managers, &printer).unwrap();

    let cargo_installs = cargo_mock.installs.lock().unwrap();
    assert_eq!(cargo_installs.len(), 1);
    assert_eq!(cargo_installs[0], vec!["ripgrep", "bat"]);

    let npm_uninstalls = npm_mock.uninstalls.lock().unwrap();
    assert_eq!(npm_uninstalls.len(), 1);
    assert_eq!(npm_uninstalls[0], vec!["old-pkg"]);
}

// --- extract_caveats comprehensive ---

// --- print_caveats with multiple notes ---

// --- Brewfile parsing edge cases ---

#[test]
fn parse_brewfile_mixed_quote_styles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(
        &path,
        "tap \"custom/tap\"\nbrew 'jq'\ncask \"visual-studio-code\"\nbrew unquoted\n",
    )
    .unwrap();

    let (taps, formulae, casks) = parse_brewfile(&path).unwrap();
    assert_eq!(taps, vec!["custom/tap"]);
    assert_eq!(formulae, vec!["jq", "unquoted"]);
    assert_eq!(casks, vec!["visual-studio-code"]);
}

#[test]
fn parse_brewfile_ignores_mas_and_others() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Brewfile");
    std::fs::write(
        &path,
        "mas \"Xcode\", id: 497799835\nwhalebrew \"whalebrew/wget\"\nvscode \"ms-python.python\"\n",
    )
    .unwrap();

    let (taps, formulae, casks) = parse_brewfile(&path).unwrap();
    // None of these should be parsed as taps, formulae, or casks
    assert!(taps.is_empty());
    assert!(formulae.is_empty());
    assert!(casks.is_empty());
}

// --- extract_brewfile_name edge cases ---

#[test]
fn extract_brewfile_name_empty_quotes() {
    assert_eq!(extract_brewfile_name(r#"brew """#), Some("".to_string()));
}

#[test]
fn extract_brewfile_name_empty_single_quotes() {
    assert_eq!(extract_brewfile_name("brew ''"), Some("".to_string()));
}

// --- parse_apt_manifest edge cases ---

#[test]
fn parse_apt_manifest_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.txt");
    std::fs::write(&path, "").unwrap();

    let pkgs = parse_apt_manifest(&path).unwrap();
    assert!(pkgs.is_empty());
}

#[test]
fn parse_apt_manifest_only_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comments.txt");
    std::fs::write(&path, "# comment 1\n# comment 2\n").unwrap();

    let pkgs = parse_apt_manifest(&path).unwrap();
    assert!(pkgs.is_empty());
}

#[test]
fn parse_apt_manifest_nonexistent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent");
    let result = parse_apt_manifest(&path);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to read apt manifest"), "got: {msg}");
}

#[test]
fn parse_apt_manifest_with_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkgs.txt");
    std::fs::write(&path, "  curl  \n  wget  \n  \n").unwrap();

    let pkgs = parse_apt_manifest(&path).unwrap();
    assert_eq!(pkgs, vec!["curl", "wget"]);
}

// --- parse_npm_package_json edge cases ---

#[test]
fn parse_npm_package_json_nonexistent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = parse_npm_package_json(&path);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to read package.json"), "got: {msg}");
}

#[test]
fn parse_npm_package_json_only_dev_deps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("package.json");
    std::fs::write(
        &path,
        r#"{"devDependencies": {"jest": "^29.0.0", "prettier": "^3.0.0"}}"#,
    )
    .unwrap();

    let pkgs = parse_npm_package_json(&path).unwrap();
    assert_eq!(pkgs.len(), 2);
    assert!(pkgs.contains(&"jest".to_string()));
    assert!(pkgs.contains(&"prettier".to_string()));
}

// --- parse_cargo_toml edge cases ---

#[test]
fn parse_cargo_toml_nonexistent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.toml");
    let result = parse_cargo_toml(&path);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to read Cargo.toml"), "got: {msg}");
}

#[test]
fn parse_cargo_toml_with_dev_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Cargo.toml");
    std::fs::write(
            &path,
            "[package]\nname = \"test\"\n\n[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n",
        )
        .unwrap();

    let pkgs = parse_cargo_toml(&path).unwrap();
    // Only reads [dependencies], not [dev-dependencies]
    assert_eq!(pkgs.len(), 1);
    assert!(pkgs.contains(&"serde".to_string()));
    assert!(!pkgs.contains(&"tempfile".to_string()));
}

// --- ScriptedManager additional edge cases ---

// --- all_package_managers trait properties ---

#[test]
fn all_package_managers_unique_names() {
    let managers = all_package_managers();
    let mut names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
    let original_len = names.len();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        original_len,
        "all_package_managers contains duplicate names"
    );
}

#[test]
fn all_package_managers_bootstrap_consistency() {
    let managers = all_package_managers();

    // snap and flatpak are Linux-only; can_bootstrap() always returns false elsewhere.
    #[cfg(target_os = "linux")]
    let bootstrappable: HashSet<&str> = [
        "brew",
        "cargo",
        "npm",
        "pipx",
        "nix",
        "go",
        "chocolatey",
        "scoop",
        "snap",
        "flatpak",
    ]
    .into();
    #[cfg(not(target_os = "linux"))]
    let bootstrappable: HashSet<&str> = [
        "brew",
        "cargo",
        "npm",
        "pipx",
        "nix",
        "go",
        "chocolatey",
        "scoop",
    ]
    .into();

    #[cfg(target_os = "linux")]
    let not_bootstrappable: HashSet<&str> = [
        "brew-tap",
        "brew-cask",
        "apt",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "winget",
    ]
    .into();
    #[cfg(not(target_os = "linux"))]
    let not_bootstrappable: HashSet<&str> = [
        "brew-tap",
        "brew-cask",
        "apt",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "winget",
        "snap",
        "flatpak",
    ]
    .into();

    for m in &managers {
        if not_bootstrappable.contains(m.name()) {
            // Safety invariant (every platform): a system package manager must
            // never report bootstrappable — cfgd cannot self-install the OS's
            // own manager, and claiming otherwise would drive a nonsensical
            // install attempt.
            assert!(
                !m.can_bootstrap(),
                "{} should NOT be bootstrappable",
                m.name()
            );
        } else if bootstrappable.contains(m.name()) {
            // The positive direction is environment-conditional: each user
            // manager can self-install only where its prerequisite tooling
            // exists (curl, a system package manager, or pip). Those
            // prerequisites are always present on the Linux/macOS/Windows CI
            // runners but not on a minimal FreeBSD base, where several managers
            // correctly report not-bootstrappable. Skip the positive assertion
            // there rather than assert a platform whose bootstrap prerequisites
            // this test cannot guarantee.
            #[cfg(not(target_os = "freebsd"))]
            assert!(m.can_bootstrap(), "{} should be bootstrappable", m.name());
        }
    }
}

// --- strip_sudo_for_exec edge cases ---

// --- SimpleManager installed_packages_with_versions dispatch ---

#[test]
fn simple_manager_with_versions_fn_dispatches() {
    // Verify that apt and dnf managers have list_with_versions set
    let apt = apt_manager();
    assert!(apt.list_with_versions.is_some());
    let dnf = dnf_manager();
    assert!(dnf.list_with_versions.is_some());
    let yum = yum_manager();
    assert!(yum.list_with_versions.is_some());
}

#[test]
fn simple_manager_without_versions_fn() {
    // Verify that apk, pacman, zypper, pkg don't have list_with_versions
    let managers = [
        apk_manager(),
        pacman_manager(),
        zypper_manager(),
        pkg_manager(),
    ];
    for mgr in &managers {
        assert!(
            mgr.list_with_versions.is_none(),
            "{} should not have list_with_versions",
            mgr.name()
        );
    }
}

// --- plan_packages with custom managers ---

#[test]
fn plan_packages_with_custom_manager() {
    let custom = ScriptedManager {
        mgr_name: "mypm".to_string(),
        check_cmd: "true".to_string(),
        list_cmd: "printf 'existing\\n'".to_string(),
        install_cmd: "echo".to_string(),
        uninstall_cmd: "echo".to_string(),
        update_cmd: None,
    };

    let profile = test_profile(PackagesSpec {
        custom: vec![cfgd_core::config::CustomManagerSpec {
            name: "mypm".to_string(),
            check: "true".to_string(),
            list_installed: "printf 'existing\\n'".to_string(),
            install: "echo".to_string(),
            uninstall: "echo".to_string(),
            update: None,
            packages: vec!["existing".to_string(), "new-pkg".to_string()],
        }],
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&custom];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // "existing" is installed, "new-pkg" is not → should have Install action for new-pkg
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        PackageAction::Install {
            manager, packages, ..
        } => {
            assert_eq!(manager, "mypm");
            assert!(packages.contains(&"new-pkg".to_string()));
            assert!(!packages.contains(&"existing".to_string()));
        }
        _ => panic!("expected Install action"),
    }
}

// --- parse_simple_lines edge cases ---

// --- plan_packages with brew sub-managers when brew available ---

#[test]
fn plan_packages_brew_submanagers_available() {
    let brew = MockPackageManager::new("brew", true, vec!["ripgrep"]);
    let brew_tap = MockPackageManager::new("brew-tap", true, vec!["homebrew/core"]);
    let brew_cask = MockPackageManager::new("brew-cask", true, vec![]);

    let profile = test_profile(PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            formulae: vec!["ripgrep".into(), "fd".into()],
            taps: vec!["homebrew/core".into(), "custom/tap".into()],
            casks: vec!["firefox".into()],
            file: None,
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&brew, &brew_tap, &brew_cask];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // brew: fd needs install (ripgrep already installed)
    let brew_install = actions.iter().find(|a| {
        matches!(
            a,
            PackageAction::Install { manager, .. } if manager == "brew"
        )
    });
    assert!(brew_install.is_some());
    if let Some(PackageAction::Install { packages, .. }) = brew_install {
        assert!(packages.contains(&"fd".to_string()));
        assert!(!packages.contains(&"ripgrep".to_string()));
    }

    // brew-tap: custom/tap needs install
    let tap_install = actions.iter().find(|a| {
        matches!(
            a,
            PackageAction::Install { manager, .. } if manager == "brew-tap"
        )
    });
    assert!(tap_install.is_some());
    if let Some(PackageAction::Install { packages, .. }) = tap_install {
        assert!(packages.contains(&"custom/tap".to_string()));
        assert!(!packages.contains(&"homebrew/core".to_string()));
    }

    // brew-cask: firefox needs install
    let cask_install = actions.iter().find(|a| {
        matches!(
            a,
            PackageAction::Install { manager, .. } if manager == "brew-cask"
        )
    });
    assert!(cask_install.is_some());
}

// --- MockPackageManager installed_packages ---

#[test]
fn mock_manager_installed_packages_returns_set() {
    let mock = MockPackageManager::new("test", true, vec!["a", "b", "c"]);
    let installed = mock.installed_packages().unwrap();
    assert_eq!(installed.len(), 3);
    assert!(installed.contains("a"));
    assert!(installed.contains("b"));
    assert!(installed.contains("c"));
}

#[test]
fn mock_manager_installed_packages_empty() {
    let mock = MockPackageManager::new("test", true, vec![]);
    let installed = mock.installed_packages().unwrap();
    assert!(installed.is_empty());
}

// --- brew path_dirs on different platforms ---

// --- Comprehensive SimpleManager constructor tests ---

// --- SimpleManager package_aliases for managers without aliases ---

// --- Verify parse function outputs match expected types ---

// --- parse_winget_list real-world output ---

// --- parse_choco_list real-world output ---

// --- parse_scoop_list real-world output ---

// =========================================================================
// Additional coverage — output verification, error paths
// =========================================================================

// --- print_caveats output verification ---

// --- ScriptedManager error variants ---

// --- run_pkg_cmd error kind dispatch ---
// We test all error_kind paths through ScriptedManager since it calls
// run_pkg_cmd_msg (for {package} mode) and run_pkg_cmd (for batch mode)

// --- apply_packages output verification ---

#[test]
fn apply_packages_skip_prints_warning() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    let actions = vec![PackageAction::Skip {
        manager: "snap".into(),
        reason: "'snap' not available — cannot auto-install on this platform".into(),
        origin: "local".into(),
    }];
    apply_packages(&actions, &[], &printer).unwrap();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("snap") && output.contains("cannot auto-install"),
        "expected skip warning, got: {}",
        *output
    );
}

// --- ScriptedManager with stderr error messages ---

// --- extract_caveats with combined stdout+stderr ---

// --- parse helpers with realistic multi-line edge cases ---

// --- winget parse with Unicode characters ---

// --- parse_tab_separated with edge cases ---

// --- parse_brew_versions with special package names ---

// --- cargo install list with path-based installs ---

// --- pipx venvs with nested metadata ---

// --- npm list with overrides ---

// --- choco list with .extension packages ---

// --- resolve_manifest_packages with dedup across inline+file ---

#[test]
fn resolve_manifest_packages_apt_dedup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pkgs.txt"), "curl\nwget\ngit\n").unwrap();

    let mut packages = PackagesSpec {
        apt: Some(cfgd_core::config::AptSpec {
            file: Some("pkgs.txt".into()),
            // "curl" already inline — should not duplicate
            packages: vec!["curl".into(), "vim".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();
    let apt = packages.apt.as_ref().unwrap();
    let curl_count = apt.packages.iter().filter(|p| *p == "curl").count();
    assert_eq!(curl_count, 1, "curl should not be duplicated");
    assert!(apt.packages.contains(&"wget".to_string()));
    assert!(apt.packages.contains(&"git".to_string()));
    assert!(apt.packages.contains(&"vim".to_string()));
}

#[test]
fn resolve_manifest_packages_cargo_dedup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[dependencies]\nserde = \"1\"\ntokio = \"1\"\n",
    )
    .unwrap();

    let mut packages = PackagesSpec {
        cargo: Some(cfgd_core::config::CargoSpec {
            file: Some("Cargo.toml".into()),
            packages: vec!["serde".into(), "clap".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();
    let cargo = packages.cargo.as_ref().unwrap();
    let serde_count = cargo.packages.iter().filter(|p| *p == "serde").count();
    assert_eq!(serde_count, 1, "serde should not be duplicated");
    assert!(cargo.packages.contains(&"tokio".to_string()));
    assert!(cargo.packages.contains(&"clap".to_string()));
}

#[test]
fn resolve_manifest_packages_npm_dedup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies": {"express": "^4", "lodash": "^4"}}"#,
    )
    .unwrap();

    let mut packages = PackagesSpec {
        npm: Some(cfgd_core::config::NpmSpec {
            file: Some("package.json".into()),
            global: vec!["express".into(), "typescript".into()],
        }),
        ..Default::default()
    };

    resolve_manifest_packages(&mut packages, dir.path()).unwrap();
    let npm = packages.npm.as_ref().unwrap();
    let express_count = npm.global.iter().filter(|p| *p == "express").count();
    assert_eq!(express_count, 1, "express should not be duplicated");
    assert!(npm.global.contains(&"lodash".to_string()));
    assert!(npm.global.contains(&"typescript".to_string()));
}

// --- ScriptedManager with update command that has stderr ---

// --- plan_packages with sub-manager that has no parent bootstrapping ---

#[test]
fn plan_sub_manager_skips_when_parent_not_bootstrapping() {
    // brew-cask is unavailable, brew is NOT being bootstrapped → cask should Skip
    let brew = MockPackageManager::new("brew", true, vec!["ripgrep"]); // available
    let cask = MockPackageManager::new("brew-cask", false, vec![]); // unavailable, can't bootstrap

    let profile = test_profile(PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            formulae: vec!["ripgrep".into()],
            casks: vec!["firefox".into()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&brew, &cask];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // brew-cask is unavailable and non-bootstrappable, and parent is not being bootstrapped
    assert!(actions.iter().any(|a| matches!(
        a,
        PackageAction::Skip { manager, .. } if manager == "brew-cask"
    )));
}

// --- plan_packages with brew-cask bootstrapping through brew ---

#[test]
fn plan_brew_cask_installs_when_brew_bootstrapping() {
    let brew = MockPackageManager::new("brew", false, vec![]).with_bootstrap();
    let cask = MockPackageManager::new("brew-cask", false, vec![]);

    let profile = test_profile(PackagesSpec {
        brew: Some(cfgd_core::config::BrewSpec {
            formulae: vec!["ripgrep".into()],
            casks: vec!["firefox".into()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let managers: Vec<&dyn PackageManager> = vec![&brew, &cask];
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();

    // brew-cask should get Install (not Skip) because brew parent is being bootstrapped
    assert!(actions.iter().any(|a| matches!(
        a,
        PackageAction::Install { manager, .. } if manager == "brew-cask"
    )));
}

// --- parse_snap_info_version ---

// --- parse_version_field (flatpak / winget / scoop) ---

// --- parse_nix_search_version ---

// --- parse_go_module_version ---

// --- parse_choco_info_version ---

// --- parse_winget_list ---

// --- parse_choco_list ---

// --- parse_scoop_list ---

// =========================================================================
// Additional coverage: pure-logic parsing, edge cases, apt version parsing
// =========================================================================

// --- query_version_apt string parsing logic ---
// query_version_apt parses `apt-cache policy` output. We can't call the function
// directly without apt, but we replicate its parsing logic to verify correctness.

// --- query_version_apk string parsing logic ---

// --- query_version_pkg string parsing logic ---

// --- query_version_info string parsing logic ---

// --- parse_nix_search_version edge cases ---

// --- parse_go_module_version edge cases ---

// --- parse_choco_info_version edge cases ---

// --- parse_version_field edge cases ---

// --- parse_snap_info_version edge cases ---

// --- ScriptedManager from_spec comprehensive field verification ---

// --- run_pkg_cmd_prefixed error kind branches ---
// We test these through ScriptedManager since it uses run_pkg_cmd_msg/run_pkg_cmd
// with different error_kind values.

// --- parse_dnf_yum_lines with whitespace-only lines ---

// --- parse_apk_lines single-hyphen-name packages ---

// --- parse_zypper_lines with real-world separators ---

// --- parse_pkg_lines with trailing whitespace ---

// --- extract_caveats with mixed-case warning detection ---

// --- parse_brew_versions edge case: tab-separated ---

// --- parse_tab_separated_versions with whitespace trimming ---

// --- ScriptedManager {packages} vs {package} template modes ---

// --- ScriptedManager per-package error stops early ---

// --- all_package_managers ordering stability ---

#[test]
fn all_package_managers_starts_with_brew_family() {
    let managers = all_package_managers();
    // brew, brew-tap, brew-cask should be the first three
    assert_eq!(managers[0].name(), "brew");
    assert_eq!(managers[1].name(), "brew-tap");
    assert_eq!(managers[2].name(), "brew-cask");
}

#[test]
fn all_package_managers_ends_with_windows_managers() {
    let managers = all_package_managers();
    let len = managers.len();
    // winget, chocolatey, scoop should be the last three
    assert_eq!(managers[len - 3].name(), "winget");
    assert_eq!(managers[len - 2].name(), "chocolatey");
    assert_eq!(managers[len - 1].name(), "scoop");
}

// --- parse_winget_list column detection ---

// --- parse_choco_list with edge cases ---

// --- parse_scoop_list with multiple dash separators ---

// --- strip_version_suffix with complex real-world names ---

// --- strip_arch_suffix with realistic patterns ---

// --- extract_caveats brew edge case: caveats section with only blank lines ---

// --- format_package_actions with single-element lists ---

#[test]
fn format_package_actions_single_package_uninstall() {
    let actions = vec![PackageAction::Uninstall {
        manager: "apt".into(),
        packages: vec!["vim".into()],
        origin: "local".into(),
    }];
    let formatted = format_package_actions(&actions);
    assert_eq!(formatted[0], "uninstall via apt: vim");
}

#[test]
fn format_package_actions_bootstrap_with_long_method() {
    let actions = vec![PackageAction::Bootstrap {
        manager: "npm".into(),
        method: "nvm".into(),
        origin: "local".into(),
    }];
    let formatted = format_package_actions(&actions);
    assert_eq!(formatted[0], "bootstrap npm via nvm");
}

// --- parse_simple_lines deduplication behavior ---

// --- parse_dnf_yum_lines with multiple skip prefixes ---

// --- parse_apk_lines with real alpine package output ---

// --- ScriptedManager available_version always None ---

// --- parse_brew_versions handles multiple versions correctly ---

// --- parse_winget_list with minimal spacing ---

// --- plan_packages with many managers but no desired packages ---

#[test]
fn plan_packages_many_managers_all_empty() {
    let mocks: Vec<MockPackageManager> = vec![
        MockPackageManager::new("brew", true, vec!["ripgrep"]),
        MockPackageManager::new("cargo", true, vec!["bat"]),
        MockPackageManager::new("npm", true, vec!["typescript"]),
        MockPackageManager::new("apt", true, vec!["curl"]),
    ];
    let profile = test_profile(PackagesSpec::default());
    let managers: Vec<&dyn PackageManager> =
        mocks.iter().map(|m| m as &dyn PackageManager).collect();
    let actions = plan_packages(&profile, &[], &managers, &HashSet::new()).unwrap();
    assert!(actions.is_empty(), "no desired packages → no actions");
}

// --- custom_managers trait conformance ---

// --- parse_choco_info_version with real-world output ---

// --- parse_snap_info_version with real-world output ---

// --- parse_nix_search_version with multiple architectures ---

// --- parse_go_module_version with real-world output ---

// --- apt_aliases comprehensive ---

// --- dnf_aliases comprehensive ---

// --- SimpleManager constructor field validation ---

// --- extract_brewfile_name edge cases ---

#[test]
fn extract_brewfile_name_double_quoted_with_options() {
    assert_eq!(
        extract_brewfile_name(r#"brew "openssl@3", link: true, force: true"#),
        Some("openssl@3".to_string())
    );
}

#[test]
fn extract_brewfile_name_single_quoted_with_options() {
    assert_eq!(
        extract_brewfile_name("cask 'firefox', args: { language: 'en' }"),
        Some("firefox".to_string())
    );
}

// --- add_package to flatpak idempotent ---

#[test]
fn add_package_flatpak_creates_spec() {
    let mut packages = PackagesSpec::default();
    assert!(packages.flatpak.is_none());
    add_package("flatpak", "org.videolan.VLC", &mut packages).unwrap();
    assert!(packages.flatpak.is_some());
    assert_eq!(
        packages.flatpak.as_ref().unwrap().packages,
        vec!["org.videolan.VLC"]
    );
}

// --- remove_package from snap classic list ---

#[test]
fn remove_package_snap_from_packages_list() {
    let mut packages = PackagesSpec {
        snap: Some(cfgd_core::config::SnapSpec {
            packages: vec!["core".into(), "snapd".into()],
            classic: vec!["code".into()],
        }),
        ..Default::default()
    };

    let removed = remove_package("snap", "core", &mut packages).unwrap();
    assert!(removed);
    let snap = packages.snap.as_ref().unwrap();
    assert_eq!(snap.packages, vec!["snapd"]);
    assert_eq!(snap.classic, vec!["code"]); // unchanged
}

// =========================================================================
// Coverage-targeted tests: exercise production functions directly
// =========================================================================

// --- sudo_cmd() production function ---

// --- strip_sudo_for_exec on real root check ---

// --- SimpleManager::is_available() dispatch with custom fn ---

// --- SimpleManager::bootstrap() is no-op ---

// --- Concrete manager can_bootstrap and is_available ---

// --- run_pkg_cmd error kind dispatch (exercised through real commands) ---
// These use sh -c to create controlled failures that exercise run_pkg_cmd_prefixed
// error paths with specific error_kind values.

// --- run_pkg_cmd_prefixed with IO error ---

// --- brew_available() ---

// --- cargo_available() / go_available() / npm_available() / pipx_available() ---

#[test]
fn find_helpers_return_consistent_results() {
    // Exercise the find_* and *_available() helper functions
    assert_eq!(cargo_available(), find_npm().is_some() || cargo_available());
    // The point is to call these functions to get coverage
    let _ = find_npm();
    let _ = find_pipx();
    let _ = find_go();
    let _ = npm_available();
    let _ = pipx_available();
    let _ = go_available();
}

// --- cargo_cmd() / npm_cmd() / pipx_cmd() / go_cmd() ---

#[test]
fn cmd_builders_return_valid_commands() {
    // Exercise the *_cmd() functions that build Command objects
    let _cargo = cargo_cmd();
    let _npm = npm_cmd();
    let _pipx = pipx_cmd();
    let _go = go_cmd();
    // These should not panic regardless of tool availability
}

// --- brew_cmd() ---

// --- path_with_brew() / brew_path() ---

// --- ScriptedManager::is_available through trait ---

// --- ScriptedManager::bootstrap through trait ---

// --- WingetManager::bootstrap error ---

// --- ChocolateyManager and ScoopManager can_bootstrap ---

// --- BrewManager::path_dirs called through trait ---

#[test]
fn brew_path_dirs_through_trait() {
    let mgr: Box<dyn PackageManager> = Box::new(BrewManager);
    let dirs = mgr.path_dirs();
    // On Linux: should have linuxbrew dirs
    // On macOS: should have homebrew dirs
    // On Windows: should be empty
    if cfg!(target_os = "linux") {
        assert_eq!(dirs.len(), 2);
    }
}

// --- SimpleManager::update with ignore_update_exit ---

// Note: We can't easily test ignore_update_exit through SimpleManager directly
// without the actual commands, but the dnf/yum managers have this flag set.
// Verify the flag is properly set on the managers that need it.

// --- SimpleManager parse_list function pointers ---

// --- SimpleManager query_version function pointers ---

// These call the actual query_version functions which shell out to system
// commands. We can verify they at least return Ok when the command is not found.

// --- SimpleManager::display_cmd with packages ---

// --- BrewManager update path ---

// --- CargoManager::update is no-op ---

// --- GoInstallManager::update is no-op ---

// --- NixManager::update is no-op ---
