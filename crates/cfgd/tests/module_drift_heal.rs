#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation

//! The round trip a module-file drift row has to survive: the daemon tick's
//! own producer RECORDS it, `cfgd apply` HEALS it, and the next `cfgd status`
//! reads a converged machine.
//!
//! The bug this guards: the tick recorded a bare `("module", "dev")`
//! aggregate while the apply resolved per-file `("module", "dev/<target>")`
//! rows. Nothing a person could run cleared the aggregate, so a module that
//! had just been deployed reported drift forever and `cfgd status
//! --exit-code` never returned to 0. The seeded row therefore comes from
//! `reconciler::action_drift_rows` — the tick's producer — rather than being
//! spelled here, so a producer that goes back to minting an aggregate takes
//! this test red instead of quietly seeding its own new grammar.
//!
//! The verbs run as the real binary, because they end in `std::process::exit`.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use cfgd_core::modules::ResolvedFile;
use cfgd_core::providers::ProviderRegistry;
use cfgd_core::reconciler::{Action, ModuleAction, ModuleActionKind, action_drift_rows};
use cfgd_core::state::StateStore;

/// A config dir holding one module that deploys one file to a target which
/// does not exist yet, so the first scan finds drift.
fn module_fixture(dir: &Path) -> std::path::PathBuf {
    let module_dir = dir.join("modules").join("dev");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(module_dir.join("app.conf"), "from the module\n").unwrap();
    let target = dir.join("deploy").join("app.conf");
    std::fs::write(
        module_dir.join("module.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dev\nspec:\n  files:\n    - source: app.conf\n      target: {}\n      strategy: Copy\n",
            target.display()
        ),
    )
    .unwrap();

    let profiles_dir = dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("tiny.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  modules:\n    - dev\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();
    target
}

fn run(args: &[&str], config: &Path, state: &Path, home: &Path) -> std::process::Output {
    let mut cmd = Command::cargo_bin("cfgd").unwrap();
    cmd.args(args)
        .arg("--config")
        .arg(config.join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state)
        .env("HOME", home)
        // Windows resolves `~` from USERPROFILE first, so a child left holding
        // the invoking account's profile would write to the real home.
        .env("USERPROFILE", home)
        // `directories` reads Windows' known folders rather than the env, so
        // nothing but this seam keeps a child's module cache out of the real profile.
        .env("CFGD_CACHE_DIR", home.join("cache"))
        .env("CFGD_COLOR", "never");
    cmd.output().unwrap()
}

/// The rows a reconcile tick records for the deploy this fixture plans, taken
/// from the tick's own producer so the test cannot spell an id no daemon
/// writes.
fn tick_rows(target: &Path) -> Vec<(String, String)> {
    let action = Action::Module(ModuleAction::local(
        "dev",
        ModuleActionKind::DeployFiles {
            files: vec![ResolvedFile {
                source: PathBuf::from("app.conf"),
                target: target.to_path_buf(),
                is_git_source: false,
                strategy: None,
                encryption: None,
                permissions: None,
                patch: None,
            }],
            declared_total: 1,
        },
    ));
    action_drift_rows(&action, &ProviderRegistry::new())
        .into_iter()
        .map(|row| (row.resource_type, row.resource_id))
        .collect()
}

#[test]
fn an_apply_heals_the_module_file_row_a_tick_recorded() {
    let config = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let target = module_fixture(config.path());

    let rows = tick_rows(&target);
    assert_eq!(
        rows.len(),
        1,
        "the deploy stands for its one file: {rows:?}"
    );
    {
        let store = StateStore::open(&state.path().join("state.db")).unwrap();
        for (rtype, rid) in &rows {
            store
                .record_drift(rtype, rid, None, Some("drift detected"), "daemon")
                .unwrap();
        }
    }

    let apply = run(&["apply", "-y"], config.path(), state.path(), home.path());
    assert!(
        apply.status.success(),
        "apply failed:\n{}\n{}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(target.exists(), "the apply deployed the module's file");

    // No `--scan`: this reads what the store holds, which is exactly what the
    // tick's producer wrote and the apply had to clear.
    let after = run(
        &["status", "--exit-code"],
        config.path(),
        state.path(),
        home.path(),
    );
    let rendered = String::from_utf8_lossy(&after.stdout);
    assert_eq!(
        after.status.code(),
        Some(0),
        "the deployed module still reports drift:\n{rendered}"
    );
    assert!(
        !rendered.contains("Drifted"),
        "a converged module reads Synced:\n{rendered}"
    );

    let store = StateStore::open(&state.path().join("state.db")).unwrap();
    assert!(
        store.unresolved_drift().unwrap().is_empty(),
        "the apply cleared the row the tick recorded: {:?}",
        store.unresolved_drift().unwrap()
    );
}
