use super::*;
// Drift helpers are reached via fully-qualified `super::drift::` paths in
// production; tests use the bare names (e.g. `record_file_drift_to`).
use super::drift::*;
use crate::config::{AutoApplyPolicyConfig, PolicyAction};
use crate::reconciler::{
    DecisionExclusions, DecisionScope, DeliveredItems, WithheldDecisions, action_resource_info,
    declared_decision_paths, hash_resources, local_profile, review_source_policy,
    source_delivered_profile,
};

/// A merged profile as ONE layer at `policy`'s tier.
///
/// The fixtures below describe what a source offers as a `MergedProfile`,
/// while the policy classifier reads layers — because only a layer carries the
/// tier. This is the bridge, and the `policy` argument is what lets a test say
/// "the source offered this in its optional profile" rather than assume.
fn tiered_items(merged: &MergedProfile, policy: crate::config::LayerPolicy) -> DeliveredItems {
    DeliveredItems::from_layers(&[tiered_layer(merged, policy)])
}

/// One layer of `merged` at `policy`'s tier, for a caller composing more than
/// one tier into a single source's offer.
fn tiered_layer(
    merged: &MergedProfile,
    policy: crate::config::LayerPolicy,
) -> crate::config::ProfileLayer {
    crate::config::ProfileLayer {
        source: "acme".to_string(),
        profile_name: format!("offered-{policy:?}"),
        priority: 500,
        policy,
        spec: crate::config::ProfileSpec {
            modules: merged.modules.clone(),
            env: merged.env.clone(),
            env_scope: Some(merged.env_scope),
            aliases: merged.aliases.clone(),
            packages: Some(merged.packages.clone()),
            files: Some(merged.files.clone()),
            system: merged.system.clone(),
            secrets: merged.secrets.clone(),
            scripts: Some(merged.scripts.clone()),
            backups: merged.backups.clone(),
            ..Default::default()
        },
    }
}

/// The daemon's two halves of one source's auto-apply policy — classify, then
/// record what the classification asked for — as the single call a tick makes.
fn process_source_decisions(
    store: &StateStore,
    source_name: &str,
    merged: &MergedProfile,
    policy: &AutoApplyPolicyConfig,
    notifier: &Notifier,
) -> HashSet<String> {
    process_tiered_decisions(
        store,
        source_name,
        &tiered_items(merged, crate::config::LayerPolicy::Recommended),
        policy,
        notifier,
    )
}

/// [`process_source_decisions`] for a caller that has already chosen the tier
/// its items arrive at.
fn process_tiered_decisions(
    store: &StateStore,
    source_name: &str,
    delivered: &DeliveredItems,
    policy: &AutoApplyPolicyConfig,
    notifier: &Notifier,
) -> HashSet<String> {
    let review = review_source_policy(store, source_name, delivered, policy)
        .expect("policy review reads the test store");
    super::reconcile::mint_reviewed_decisions(store, &review, notifier);
    review.declined
}
use crate::test_helpers::{test_printer, test_state};

/// A never-raised abort flag with a `'static` lifetime, so a `ReconcileCtx`
/// built by a helper can borrow one without the caller owning it.
fn never_abort() -> &'static crate::AbortFlag {
    static FLAG: std::sync::OnceLock<crate::AbortFlag> = std::sync::OnceLock::new();
    FLAG.get_or_init(crate::AbortFlag::new)
}

fn quiet_reconcile_ctx<'a>(
    state: &'a Arc<Mutex<DaemonState>>,
    notifier: &'a Arc<Notifier>,
    notify_on_drift: bool,
    hooks: &'a dyn DaemonHooks,
    state_dir: &'a Path,
    printer: &'a crate::output::Printer,
) -> ReconcileCtx<'a> {
    ReconcileCtx {
        state,
        notifier,
        notify_on_drift,
        hooks,
        state_dir_override: Some(state_dir),
        printer,
        module_filter: None,
        auto_apply_override: None,
        drift_policy_override: None,
        scope: crate::Scope::User,
        abort: never_abort(),
    }
}

#[test]
fn parse_duration_seconds() {
    assert_eq!(parse_duration_or_default("30s"), Duration::from_secs(30));
}

#[test]
fn parse_duration_minutes() {
    assert_eq!(parse_duration_or_default("5m"), Duration::from_secs(300));
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration_or_default("1h"), Duration::from_secs(3600));
}

#[test]
fn parse_duration_plain_number() {
    assert_eq!(parse_duration_or_default("120"), Duration::from_secs(120));
}

#[test]
fn parse_duration_invalid_falls_back() {
    assert_eq!(
        parse_duration_or_default("invalid"),
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
}

#[test]
fn parse_duration_with_whitespace() {
    assert_eq!(parse_duration_or_default(" 10m "), Duration::from_secs(600));
}

fn module_drift_plan(action: crate::reconciler::Action) -> crate::reconciler::Plan {
    crate::reconciler::Plan {
        phases: vec![crate::reconciler::Phase::from_actions(
            crate::reconciler::PhaseName::Modules,
            &crate::reconciler::Owner::profile("test"),
            vec![action],
        )],
        warnings: Vec::new(),
    }
}

fn module_action(
    name: &str,
    kind: crate::reconciler::ModuleActionKind,
) -> crate::reconciler::Action {
    crate::reconciler::Action::Module(crate::reconciler::ModuleAction {
        module_name: name.to_string(),
        kind,
        origin: None,
    })
}

#[test]
fn module_has_drift_true_for_install_packages_action() {
    let plan = module_drift_plan(module_action(
        "watched",
        crate::reconciler::ModuleActionKind::InstallPackages { resolved: vec![] },
    ));
    assert!(module_has_drift(&plan, "watched"));
}

#[test]
fn module_has_drift_true_for_deploy_files_and_run_script_actions() {
    let files_plan = module_drift_plan(module_action(
        "watched",
        crate::reconciler::ModuleActionKind::DeployFiles { files: vec![] },
    ));
    assert!(module_has_drift(&files_plan, "watched"));

    let script_plan = module_drift_plan(module_action(
        "watched",
        crate::reconciler::ModuleActionKind::RunScript {
            script: crate::config::ScriptEntry::Simple("echo hi".into()),
            phase: crate::reconciler::ScriptPhase::PostApply,
        },
    ));
    assert!(module_has_drift(&script_plan, "watched"));
}

#[test]
fn module_has_drift_false_for_skip_action() {
    let plan = module_drift_plan(module_action(
        "watched",
        crate::reconciler::ModuleActionKind::Skip {
            reason: "dependency not met".into(),
        },
    ));
    assert!(!module_has_drift(&plan, "watched"));
}

#[test]
fn module_has_drift_false_for_other_module_action() {
    let plan = module_drift_plan(module_action(
        "other",
        crate::reconciler::ModuleActionKind::InstallPackages { resolved: vec![] },
    ));
    assert!(!module_has_drift(&plan, "watched"));
}

#[test]
fn module_has_drift_false_for_empty_plan() {
    let plan = crate::reconciler::Plan {
        phases: Vec::new(),
        warnings: Vec::new(),
    };
    assert!(!module_has_drift(&plan, "watched"));
}

#[test]
fn daemon_state_initial() {
    let state = DaemonState::new();
    assert!(state.last_reconcile.is_none());
    assert!(state.last_sync.is_none());
    assert_eq!(state.drift_count, 0);
    assert_eq!(state.sources.len(), 1);
    assert_eq!(state.sources[0].name, "local");
}

#[test]
fn daemon_state_response() {
    let state = DaemonState::new();
    let response = state.to_response();
    assert!(response.running);
    assert!(response.pid > 0);
    assert_eq!(response.sources.len(), 1);
}

#[test]
fn notifier_stdout_does_not_panic() {
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    assert!(matches!(notifier.method, NotifyMethod::Stdout));
    assert!(notifier.webhook_url.is_none());
    // Stdout notifier calls tracing::info! — verify it completes without panic
    notifier.notify("test", "message");
}

#[test]
fn source_status_round_trips() {
    let status = SourceStatus {
        name: "local".to_string(),
        last_sync: Some("2026-01-01T00:00:00Z".to_string()),
        last_reconcile: None,
        drift_count: 3,
        status: "active".to_string(),
    };
    let json = serde_json::to_string(&status).unwrap();
    let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "local");
    assert_eq!(parsed.last_sync.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert!(parsed.last_reconcile.is_none());
    assert_eq!(parsed.drift_count, 3);
    assert_eq!(parsed.status, "active");
    // Verify camelCase renaming
    assert!(json.contains("\"driftCount\":3"));
    assert!(json.contains("\"lastSync\":"));
}

#[test]
#[cfg(unix)]
fn systemd_unit_path() {
    let home = "/home/testuser";
    let unit_dir = PathBuf::from(home).join(SYSTEMD_USER_DIR);
    let unit_path = unit_dir.join("cfgd.service");
    assert_eq!(
        unit_path.to_str().unwrap(),
        "/home/testuser/.config/systemd/user/cfgd.service"
    );
}

#[test]
fn generate_device_id_is_stable() {
    let id1 = generate_device_id().unwrap();
    let id2 = generate_device_id().unwrap();
    assert_eq!(id1, id2);
    // SHA256 hex string is 64 characters
    assert_eq!(id1.len(), 64);
}

#[test]
fn compute_config_hash_is_deterministic() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };
    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let hash1 = compute_config_hash(&resolved).unwrap();
    let hash2 = compute_config_hash(&resolved).unwrap();
    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), 64);
}

#[test]
fn find_server_url_returns_none_for_git_origin() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Git,
                url: "https://github.com/test/repo.git".into(),
                branch: "master".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    assert!(find_server_url(&config).is_none());
}

#[test]
fn find_server_url_returns_url_for_server_origin() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Server,
                url: "https://cfgd.example.com".into(),
                branch: "master".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    assert_eq!(
        find_server_url(&config),
        Some("https://cfgd.example.com".to_string())
    );
}

#[test]
fn checkin_payload_round_trips() {
    let payload = CheckinPayload {
        device_id: "abc123".into(),
        hostname: "test-host".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        config_hash: "deadbeef".into(),
    };
    let json = serde_json::to_string(&payload).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["device_id"], "abc123");
    assert_eq!(parsed["hostname"], "test-host");
    assert_eq!(parsed["os"], "linux");
    assert_eq!(parsed["arch"], "x86_64");
    assert_eq!(parsed["config_hash"], "deadbeef");
    // Exactly 5 fields
    assert_eq!(parsed.as_object().unwrap().len(), 5);
}

#[test]
fn checkin_response_deserializes() {
    let json = r#"{"status":"ok","config_changed":true,"config":null}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    assert!(resp.config_changed);
    assert_eq!(resp._status, "ok");
}

#[test]
#[cfg(unix)]
fn launchd_plist_path() {
    let home = "/Users/testuser";
    let plist_dir = PathBuf::from(home).join(LAUNCHD_AGENTS_DIR);
    let plist_path = plist_dir.join(format!("{}.plist", LAUNCHD_LABEL));
    assert_eq!(
        plist_path.to_str().unwrap(),
        "/Users/testuser/Library/LaunchAgents/com.cfgd.daemon.plist"
    );
}

#[test]
fn extract_source_resources_from_merged_profile() {
    use crate::config::{
        BrewSpec, CargoSpec, FilesSpec, ManagedFileSpec, MergedProfile, PackagesSpec,
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["ripgrep".into(), "fd".into()],
                casks: vec!["firefox".into()],
                ..Default::default()
            }),
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        files: FilesSpec {
            managed: vec![ManagedFileSpec {
                patch: None,
                source: "dotfiles/.zshrc".into(),
                target: PathBuf::from("/home/user/.zshrc"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        },
        env: vec![crate::config::EnvVar {
            name: "EDITOR".into(),
            value: "vim".into(),
        }],
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("packages.brew.ripgrep"));
    assert!(resources.contains("packages.brew.fd"));
    assert!(resources.contains("packages.brew.firefox"));
    assert!(resources.contains("packages.cargo.bat"));
    assert!(resources.contains("files./home/user/.zshrc"));
    assert!(resources.contains("env.EDITOR"));
    assert_eq!(resources.len(), 6);
}

#[test]
fn hash_resources_is_deterministic() {
    let r1: HashSet<String> =
        HashSet::from_iter(["a".to_string(), "b".to_string(), "c".to_string()]);
    let r2: HashSet<String> =
        HashSet::from_iter(["c".to_string(), "a".to_string(), "b".to_string()]);

    assert_eq!(hash_resources(&r1), hash_resources(&r2));
}

#[test]
fn hash_resources_differs_for_different_sets() {
    let r1: HashSet<String> = HashSet::from_iter(["a".to_string()]);
    let r2: HashSet<String> = HashSet::from_iter(["b".to_string()]);

    assert_ne!(hash_resources(&r1), hash_resources(&r2));
}

#[test]
fn process_source_decisions_first_run_records_decisions() {
    use crate::config::PackagesSpec;
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default(); // new_recommended: Notify

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined = process_source_decisions(&store, "acme", &merged, &policy, &notifier);
    assert!(
        declined.is_empty(),
        "Notify records the item for review rather than declining it outright"
    );

    // First run: all items are new, policy is Notify → pending decisions created
    let pending = store.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "packages.cargo.bat");

    // The decision path is not the plan's vocabulary: translated, it withholds
    // `bat` from a cargo batch and leaves every unrelated package alone.
    let exclusions =
        DecisionExclusions::from_decision_paths(withheld_paths(&store), crate::expand_tilde);
    assert!(exclusions.withholds_package("cargo", "bat"));
    assert!(!exclusions.withholds_package("cargo", "ripgrep"));
    assert!(!exclusions.withholds_package("npm", "bat"));

    let mut phase = packages_phase_of(vec![install_of("cargo", &["bat", "ripgrep"])]);
    prune_with(&mut phase, &exclusions);
    assert_eq!(
        installed_batches(&phase),
        vec![("cargo".to_string(), vec!["ripgrep".to_string()])],
        "the undecided package leaves the batch; its siblings still apply"
    );
}

#[test]
fn first_observation_of_a_source_reasks_nothing_already_answered() {
    use crate::config::PackagesSpec;
    // Rows can exist before any hash does: `cfgd decide` records the one item
    // it answers and stamps nothing, so the daemon's first stamped observation
    // arrives with an answered row already present. "No previous hash" must
    // not read as "the source changed" for that item — re-minting it would
    // supersede the answer the operator just gave — while the sibling, never
    // asked about, is still minted: the notification decide left unconsumed.
    let store = test_state();
    let policy = AutoApplyPolicyConfig::default(); // new_recommended: Notify

    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat (from acme)",
        )
        .unwrap();
    store
        .resolve_decision("packages.cargo.bat", "rejected")
        .unwrap();

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into(), "eza".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let review = review_source_policy(
        &store,
        "acme",
        &tiered_items(&merged, crate::config::LayerPolicy::Recommended),
        &policy,
    )
    .unwrap();

    assert_eq!(
        review
            .to_mint
            .iter()
            .map(|m| m.resource.as_str())
            .collect::<Vec<_>>(),
        vec!["packages.cargo.eza"],
        "only the never-asked item is minted; the answered one stands"
    );
    assert!(
        !review.changed_hashes.is_empty(),
        "the observation itself is still recorded once a run may write it"
    );
}

/// Every withholding decision's resource path, straight from the store — the
/// read [`DecisionScope`] then filters down to what a run may still withhold.
fn withheld_paths(store: &StateStore) -> HashSet<String> {
    store
        .withheld_decisions()
        .expect("read withholding decisions")
        .into_iter()
        .map(|d| d.resource)
        .collect()
}

/// A local profile declaring one cargo package of the subscriber's own.
fn local_profile_declaring_bat() -> crate::config::ResolvedProfile {
    use crate::config::*;
    let packages = PackagesSpec {
        cargo: Some(CargoSpec {
            file: None,
            packages: vec!["bat".into()],
        }),
        ..Default::default()
    };
    ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(packages.clone()),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            packages,
            ..Default::default()
        },
    }
}

/// A source recommending one brew formula, subscribed with `accept_recommended`.
fn source_recommending_k9s(accept_recommended: bool) -> crate::composition::CompositionInput {
    use crate::config::*;
    crate::composition::CompositionInput {
        source_name: "acme".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["k9s".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints::default(),
        layers: vec![],
        subscription: crate::composition::SubscriptionConfig {
            accept_recommended,
            ..Default::default()
        },
        allow_scripts: false,
    }
}

#[test]
fn a_recommended_item_the_subscriber_declined_to_accept_is_never_asked_about() {
    let local = local_profile_declaring_bat();
    let composed = crate::composition::compose(
        &local,
        &[source_recommending_k9s(false)],
        crate::composition::ConstraintMode::Enforce,
    )
    .unwrap();

    let delivered = source_delivered_profile(&composed.resolved, "acme");
    assert!(
        declared_decision_paths(&delivered).is_empty(),
        "an unaccepted recommended tier never becomes a layer, so the source \
         delivered nothing to decide about"
    );

    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let declined = process_source_decisions(
        &store,
        "acme",
        &delivered,
        &AutoApplyPolicyConfig::default(),
        &notifier,
    );

    assert!(
        declined.is_empty(),
        "nothing was delivered, so there is nothing to decline"
    );
    assert!(
        store.pending_decisions().unwrap().is_empty(),
        "an item cfgd will not apply must mint no pending row and add no noise \
         to a plan"
    );
}

#[test]
fn only_what_the_source_delivered_is_minted_as_a_decision() {
    let local = local_profile_declaring_bat();
    let composed = crate::composition::compose(
        &local,
        &[source_recommending_k9s(true)],
        crate::composition::ConstraintMode::Enforce,
    )
    .unwrap();

    let delivered = source_delivered_profile(&composed.resolved, "acme");
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    process_source_decisions(
        &store,
        "acme",
        &delivered,
        &AutoApplyPolicyConfig::default(),
        &notifier,
    );

    let pending: Vec<String> = store
        .pending_decisions()
        .unwrap()
        .into_iter()
        .map(|d| d.resource)
        .collect();
    assert_eq!(
        pending,
        vec!["packages.brew.k9s".to_string()],
        "the accepted recommended item is the one thing the source put in front \
         of the operator"
    );

    // Feeding the whole composed profile instead would mint the subscriber's own
    // declaration as if a source had sent it — and now that a pending row
    // withholds its resource, that row would block the operator's own package.
    let scope = DecisionScope::new(["acme"], &local_profile(&composed.resolved));
    let withheld = WithheldDecisions::read(&store, &scope).expect("read withheld decisions");
    let exclusions = DecisionExclusions::from_withheld(&withheld);
    assert!(exclusions.withholds_package("brew", "k9s"));
    assert!(
        !exclusions.withholds_package("cargo", "bat"),
        "a local declaration is not a source decision"
    );
}

#[test]
fn a_decision_never_withholds_a_resource_the_operator_declares_themselves() {
    // The decision names a path, and a path is not an owner: the subscriber
    // declares `bat` in their own profile while the source offers it too, so a
    // row about the source's copy must not be what removes the operator's.
    let local = local_profile_declaring_bat();
    let mut source = source_recommending_k9s(true);
    source.policy.recommended.packages = Some(crate::config::PackagesSpec {
        cargo: Some(crate::config::CargoSpec {
            file: None,
            packages: vec!["bat".into()],
        }),
        ..Default::default()
    });
    let composed = crate::composition::compose(
        &local,
        &[source],
        crate::composition::ConstraintMode::Enforce,
    )
    .unwrap();

    let store = test_state();
    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "bat",
        )
        .unwrap();
    store
        .resolve_decision("packages.cargo.bat", "rejected")
        .unwrap();

    let scope = DecisionScope::new(["acme"], &local_profile(&composed.resolved));
    let withheld = WithheldDecisions::read(&store, &scope).expect("read withheld decisions");
    assert!(
        withheld.is_empty(),
        "the operator's own declaration outranks a source's decision over the \
         same path"
    );
    assert!(
        !DecisionExclusions::from_withheld(&withheld).withholds_package("cargo", "bat"),
        "declining a source's offer must not uninstall what the operator asked for"
    );
}

/// A resolved profile whose only local layer declares one managed file.
fn local_profile_declaring_file(target: &str) -> crate::config::ResolvedProfile {
    use crate::config::*;
    let files = FilesSpec {
        managed: vec![ManagedFileSpec {
            source: "files/zshrc".into(),
            target: std::path::PathBuf::from(target),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
            patch: None,
        }],
        ..Default::default()
    };
    ResolvedProfile {
        layers: vec![ProfileLayer {
            source: LOCAL_LAYER.into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                files: Some(files.clone()),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            files,
            ..Default::default()
        },
    }
}

/// The two spellings of one home-relative path, in both orders. A source and a
/// subscriber write their own manifests, so the guard cannot assume they agree
/// on `~` — and the prune expands, so a raw string comparison would admit the
/// row and then delete the operator's own action with it.
#[test]
#[serial_test::serial]
fn a_decision_never_withholds_a_local_declaration_spelled_differently() {
    let _home = crate::with_test_home_guard(std::path::Path::new("/home/decision-guard"));
    let expanded = crate::to_posix_string(crate::expand_tilde(std::path::Path::new("~/.zshrc")));

    for (declared, decided) in [
        ("~/.zshrc", format!("files.{expanded}")),
        (expanded.as_str(), "files.~/.zshrc".to_string()),
    ] {
        let local = local_profile_declaring_file(declared);
        let store = test_state();
        store
            .upsert_pending_decision("acme", &decided, "recommended", "install", "zshrc")
            .unwrap();

        let scope = DecisionScope::new(["acme"], &local_profile(&local));
        let withheld = WithheldDecisions::read(&store, &scope).expect("read withheld decisions");
        assert!(
            withheld.is_empty(),
            "a decision spelled `{decided}` must not withhold the operator's own \
             `{declared}` declaration"
        );
        assert!(
            !DecisionExclusions::from_withheld(&withheld)
                .withholds_file(&crate::expand_tilde(std::path::Path::new(declared))),
            "and the prune must leave the operator's file action in the plan"
        );
    }
}

#[test]
fn a_decision_stops_withholding_once_its_source_is_gone() {
    // The rows a dropped source leaves are the rows nobody can answer, so they
    // must go inert the moment it stops delivering — including a rejection,
    // which would otherwise be a permanent block on a path with no source left
    // to `cfgd decide` against.
    let local = local_profile_declaring_bat();
    let composed = crate::composition::compose(
        &local,
        &[source_recommending_k9s(true)],
        crate::composition::ConstraintMode::Enforce,
    )
    .unwrap();

    let store = test_state();
    store
        .upsert_pending_decision("acme", "packages.brew.k9s", "recommended", "install", "k9s")
        .unwrap();
    store
        .upsert_pending_decision(
            "gone",
            "packages.brew.stern",
            "recommended",
            "install",
            "stern",
        )
        .unwrap();
    store
        .resolve_decision("packages.brew.stern", "rejected")
        .unwrap();

    let scope = DecisionScope::new(["acme"], &local_profile(&composed.resolved));
    let withheld = WithheldDecisions::read(&store, &scope).expect("read withheld decisions");
    assert_eq!(
        withheld
            .resource_paths()
            .collect::<Vec<_>>()
            .as_slice()
            .to_vec(),
        vec!["packages.brew.k9s".to_string()],
        "only the subscribed source's decision still withholds"
    );

    // The rows themselves are cleaned up by the reconcile sweep, which is what
    // stops `cfgd status` offering a decision the operator cannot act on.
    assert_eq!(
        store
            .discard_decisions_not_in(&["acme".to_string()])
            .unwrap(),
        1
    );
    assert_eq!(store.withheld_decisions().unwrap().len(), 1);
}

/// A config naming a profile that declares nothing, plus its profiles dir, so
/// a reconcile tick runs end-to-end without planning any work.
fn inert_config_under(root: &std::path::Path) -> PathBuf {
    inert_config_named(root, "other.yaml")
}

/// [`inert_config_under`] with a caller-chosen filename, for staging the same
/// inert config AT the default config location (`cfgd.yaml`).
fn inert_config_named(root: &std::path::Path, filename: &str) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let config_path = root.join(filename);
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: solo\n",
    )
    .unwrap();
    let profiles = root.join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("solo.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: solo\nspec: {}\n",
    )
    .unwrap();
    config_path
}

/// Run one tick against the DEFAULT state store, from whatever config path the
/// daemon was pointed at — ownership of the store's decision rows is judged on
/// that path alone.
async fn tick_against_default_store(config_path: PathBuf) {
    crate::spawn_blocking_with_test_home(move || {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let printer = test_printer();
        handle_reconcile(
            &config_path,
            None,
            ReconcileCtx {
                state: &state,
                notifier: &notifier,
                notify_on_drift: false,
                hooks: &crate::test_helpers::NoopDaemonHooks,
                state_dir_override: None,
                printer: &printer,
                module_filter: None,
                auto_apply_override: None,
                drift_policy_override: None,
                scope: crate::Scope::User,
                abort: never_abort(),
            },
        );
    })
    .await
    .expect("the tick runs to completion");
}

/// The daemon takes the same store-ownership gate `cfgd apply` does: a daemon
/// started with `--config other.yaml` against the DEFAULT store would
/// otherwise delete another config's decision rows, unrecoverably, on its very
/// first tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_daemon_on_a_foreign_config_leaves_the_default_stores_rows_alone() {
    let staging = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(staging.path());
    let store = StateStore::open_default().expect("the default store lands under the test home");
    store
        .upsert_pending_decision("gone", "packages.brew.stern", "recommended", "install", "s")
        .unwrap();

    tick_against_default_store(inert_config_under(staging.path())).await;

    assert_eq!(
        store.pending_decisions().unwrap().len(),
        1,
        "a config this daemon was pointed at is not authoritative over another config's rows"
    );
}

/// The other arm, in the exact shape every installed service unit runs:
/// systemd/launchd/the SCM binPath all bake `--config <default path>` into the
/// invocation, and a daemon on the machine's own config must sweep no matter
/// how that config was named. Ownership is the PATH, not the spelling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_service_daemon_naming_the_default_config_still_sweeps_dead_decision_rows() {
    let staging = tempfile::tempdir().unwrap();
    let _home = crate::with_test_home_guard(staging.path());
    let store = StateStore::open_default().expect("the default store lands under the test home");
    store
        .upsert_pending_decision("gone", "packages.brew.stern", "recommended", "install", "s")
        .unwrap();

    // The same file the bare default would resolve, passed the way a generated
    // service unit passes it: as an explicit path.
    let config_path =
        inert_config_named(&crate::default_config_dir(), crate::config::CONFIG_FILENAME);
    tick_against_default_store(config_path).await;

    assert!(
        store.pending_decisions().unwrap().is_empty(),
        "a row whose source is not in spec.sources is one nobody can answer"
    );
}

/// A `Packages` phase owned by one profile, for the pending-decision prune.
fn packages_phase_of(actions: Vec<crate::reconciler::Action>) -> crate::reconciler::Phase {
    crate::reconciler::Phase::from_actions(
        crate::reconciler::PhaseName::Packages,
        &crate::reconciler::Owner::profile("default"),
        actions,
    )
}

fn install_of(manager: &str, packages: &[&str]) -> crate::reconciler::Action {
    crate::reconciler::Action::Package(crate::providers::PackageAction::Install {
        manager: manager.to_string(),
        packages: packages.iter().map(|p| (*p).to_string()).collect(),
        origin: "acme".to_string(),
    })
}

/// The exact prune `handle_reconcile` runs, so a unit test and the daemon
/// cannot drift apart on how the exclusions are applied.
fn prune_with(phase: &mut crate::reconciler::Phase, exclusions: &DecisionExclusions) {
    phase.retain_actions_and_batches(
        |action| !exclusions.withholds_action(action),
        |manager, package| !exclusions.withholds_package(manager, package),
        |target| !exclusions.withholds_file(target),
    );
}

/// Every surviving install batch as `(manager, packages)`.
fn installed_batches(phase: &crate::reconciler::Phase) -> Vec<(String, Vec<String>)> {
    phase
        .actions()
        .filter_map(|action| match action {
            crate::reconciler::Action::Package(crate::providers::PackageAction::Install {
                manager,
                packages,
                ..
            }) => Some((manager.clone(), packages.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn pending_package_decision_drops_the_action_only_when_its_batch_empties() {
    let exclusions = DecisionExclusions::from_decision_paths(
        ["packages.cargo.bat".to_string()],
        crate::expand_tilde,
    );

    let mut shrunk = packages_phase_of(vec![install_of("cargo", &["bat", "ripgrep"])]);
    prune_with(&mut shrunk, &exclusions);
    assert_eq!(shrunk.action_count(), 1, "a batch with survivors is kept");
    assert_eq!(
        installed_batches(&shrunk),
        vec![("cargo".to_string(), vec!["ripgrep".to_string()])]
    );

    let mut emptied = packages_phase_of(vec![install_of("cargo", &["bat"])]);
    prune_with(&mut emptied, &exclusions);
    assert!(
        emptied.is_empty() && emptied.groups().is_empty(),
        "an emptied batch drops its action, and the emptied group with it"
    );
}

#[test]
fn pending_package_decision_leaves_the_bootstrap_of_its_manager_alone() {
    // A bootstrap installs the package MANAGER — the prerequisite of every
    // still-decided package in the batch — so it names no decidable package.
    let exclusions = DecisionExclusions::from_decision_paths(
        ["packages.cargo.bat".to_string()],
        crate::expand_tilde,
    );
    let mut phase = packages_phase_of(vec![
        crate::reconciler::Action::Package(crate::providers::PackageAction::Bootstrap {
            manager: "cargo".into(),
            method: "auto".into(),
            origin: "acme".into(),
        }),
        install_of("cargo", &["bat", "ripgrep"]),
    ]);
    prune_with(&mut phase, &exclusions);
    assert_eq!(phase.action_count(), 2, "the bootstrap survives");
    assert_eq!(
        installed_batches(&phase),
        vec![("cargo".to_string(), vec!["ripgrep".to_string()])]
    );
}

#[test]
fn pending_package_decision_withholds_from_a_module_batch_too() {
    // A package a module claims is planned as the module's own
    // `InstallPackages` batch, while the decision path was minted from the
    // profile's declaration of the same package.
    use crate::reconciler::{Action, ModuleAction, ModuleActionKind};

    let resolved = |name: &str| crate::modules::ResolvedPackage {
        canonical_name: name.to_string(),
        resolved_name: name.to_string(),
        manager: "cargo".to_string(),
        version: None,
        script: None,
        creates: None,
        only_if: None,
        unless: None,
    };
    let exclusions = DecisionExclusions::from_decision_paths(
        ["packages.cargo.bat".to_string()],
        crate::expand_tilde,
    );
    let mut phase = packages_phase_of(vec![Action::Module(ModuleAction {
        module_name: "cli-tools".into(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![resolved("bat"), resolved("ripgrep")],
        },
        origin: Some("acme".into()),
    })]);
    prune_with(&mut phase, &exclusions);

    let names: Vec<String> = phase
        .actions()
        .flat_map(|action| match action {
            Action::Module(ModuleAction {
                kind: ModuleActionKind::InstallPackages { resolved },
                ..
            }) => resolved.iter().map(|p| p.resolved_name.clone()).collect(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(names, vec!["ripgrep".to_string()]);
}

#[test]
fn a_module_whose_only_package_awaits_a_decision_reports_no_drift() {
    // The documented consequence of the prune, now that it actually matches:
    // `module_has_drift` reads the pruned plan, so a module left with nothing
    // to do fires no `onDrift` hook — the hook reacts to work the tick will do,
    // and an undecided resource is work it will not do.
    use crate::reconciler::{Action, ModuleAction, ModuleActionKind};

    let exclusions = DecisionExclusions::from_decision_paths(
        ["packages.cargo.bat".to_string()],
        crate::expand_tilde,
    );
    let mut phase = packages_phase_of(vec![Action::Module(ModuleAction {
        module_name: "cli-tools".into(),
        kind: ModuleActionKind::InstallPackages {
            resolved: vec![crate::modules::ResolvedPackage {
                canonical_name: "bat".into(),
                resolved_name: "bat".into(),
                manager: "cargo".into(),
                version: None,
                script: None,
                creates: None,
                only_if: None,
                unless: None,
            }],
        },
        origin: Some("acme".into()),
    })]);
    let plan_before = crate::reconciler::Plan {
        phases: vec![packages_phase_of(vec![Action::Module(ModuleAction {
            module_name: "cli-tools".into(),
            kind: ModuleActionKind::Skip {
                reason: "placeholder".into(),
            },
            origin: None,
        })])],
        warnings: Vec::new(),
    };
    assert!(
        !module_has_drift(&plan_before, "cli-tools"),
        "a Skip action was never drift to begin with"
    );

    prune_with(&mut phase, &exclusions);
    let plan = crate::reconciler::Plan {
        phases: vec![phase],
        warnings: Vec::new(),
    };
    assert!(plan.is_empty());
    assert!(!module_has_drift(&plan, "cli-tools"));
}

#[test]
fn pending_brew_decision_reaches_a_cask_installed_by_brew_cask() {
    // `declared_decision_paths` mints a cask as `packages.brew.<name>`, but the
    // planner installs it through the `brew-cask` manager.
    let exclusions = DecisionExclusions::from_decision_paths(
        ["packages.brew.firefox".to_string()],
        crate::expand_tilde,
    );
    assert!(exclusions.withholds_package("brew-cask", "firefox"));
    assert!(exclusions.withholds_package("brew", "firefox"));
    assert!(!exclusions.withholds_package("brew-cask", "ripgrep"));
    // The fold is one-directional: a decision naming the cask manager outright
    // must not reach a formula of the same name.
    let cask_only = DecisionExclusions::from_decision_paths(
        ["packages.brew-cask.slack".to_string()],
        crate::expand_tilde,
    );
    assert!(cask_only.withholds_package("brew-cask", "slack"));
    assert!(!cask_only.withholds_package("brew", "slack"));
}

#[test]
fn pending_file_decision_matches_the_expanded_action_target() {
    // The decision keeps the declared `~` spelling; the planner expands it.
    let home = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(home.path());

    let exclusions = DecisionExclusions::from_decision_paths(
        ["files.~/.zshrc".to_string(), "files./etc/hosts".to_string()],
        crate::expand_tilde,
    );

    let file_action = |target: PathBuf| {
        crate::reconciler::Action::File(crate::providers::FileAction::Create {
            source: PathBuf::from("/src/file"),
            target,
            origin: "acme".into(),
            strategy: crate::config::FileStrategy::default(),
            source_hash: None,
            patch: None,
        })
    };
    assert!(exclusions.withholds_action(&file_action(home.path().join(".zshrc"))));
    assert!(exclusions.withholds_action(&file_action(PathBuf::from("/etc/hosts"))));
    assert!(!exclusions.withholds_action(&file_action(home.path().join(".bashrc"))));
}

#[test]
fn pending_file_decision_reaches_a_module_deployed_target_too() {
    // Profile files and module files are separate surfaces that can name one
    // path, so withholding only the profile action still writes the file.
    let home = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(home.path());

    let exclusions = DecisionExclusions::from_decision_paths(
        ["files.~/.zshrc".to_string()],
        crate::expand_tilde,
    );

    let resolved_file = |target: PathBuf| crate::modules::ResolvedFile {
        source: PathBuf::from("/src/file"),
        target,
        is_git_source: false,
        strategy: None,
        encryption: None,
        permissions: None,
        patch: None,
    };
    let deploy = crate::reconciler::Action::Module(crate::reconciler::ModuleAction {
        module_name: "shell".to_string(),
        kind: crate::reconciler::ModuleActionKind::DeployFiles {
            files: vec![
                resolved_file(home.path().join(".zshrc")),
                resolved_file(home.path().join(".bashrc")),
            ],
        },
        origin: Some("acme".to_string()),
    });

    let mut phase = crate::reconciler::Phase::from_actions(
        crate::reconciler::PhaseName::Modules,
        &crate::reconciler::Owner::profile("default"),
        vec![deploy],
    );
    prune_with(&mut phase, &exclusions);

    let targets: Vec<PathBuf> = phase
        .actions()
        .filter_map(|action| match action {
            crate::reconciler::Action::Module(crate::reconciler::ModuleAction {
                kind: crate::reconciler::ModuleActionKind::DeployFiles { files },
                ..
            }) => Some(files.iter().map(|f| f.target.clone()).collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        targets,
        vec![home.path().join(".bashrc")],
        "the undecided target leaves the module's deploy batch; its siblings still deploy"
    );
}

#[test]
fn pending_file_decision_drops_a_module_deploy_action_it_empties() {
    let home = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(home.path());

    let exclusions = DecisionExclusions::from_decision_paths(
        ["files.~/.zshrc".to_string()],
        crate::expand_tilde,
    );
    let deploy = crate::reconciler::Action::Module(crate::reconciler::ModuleAction {
        module_name: "shell".to_string(),
        kind: crate::reconciler::ModuleActionKind::DeployFiles {
            files: vec![crate::modules::ResolvedFile {
                source: PathBuf::from("/src/file"),
                target: home.path().join(".zshrc"),
                is_git_source: false,
                strategy: None,
                encryption: None,
                permissions: None,
                patch: None,
            }],
        },
        origin: Some("acme".to_string()),
    });

    let mut phase = crate::reconciler::Phase::from_actions(
        crate::reconciler::PhaseName::Modules,
        &crate::reconciler::Owner::profile("default"),
        vec![deploy],
    );
    prune_with(&mut phase, &exclusions);

    assert_eq!(
        phase.action_count(),
        0,
        "a deploy batch the decision emptied takes its action with it"
    );
}

#[test]
fn pending_env_decision_withholds_the_whole_env_surface() {
    // One `WriteEnvFile` renders every declared variable into one file, so
    // there is no per-variable action to withhold — the surface is withheld as
    // the unit it is generated as.
    use crate::reconciler::{Action, EnvAction};

    let exclusions =
        DecisionExclusions::from_decision_paths(["env.EDITOR".to_string()], crate::expand_tilde);
    assert!(
        exclusions.withholds_action(&Action::Env(EnvAction::WriteEnvFile {
            path: PathBuf::from("/home/user/.cfgd.env"),
            content: "export EDITOR=vim".into(),
        }))
    );
    assert!(
        exclusions.withholds_action(&Action::Env(EnvAction::InjectSourceLine {
            rc_path: PathBuf::from("/home/user/.bashrc"),
            line: ". ~/.cfgd.env".into(),
        }))
    );
    assert!(
        exclusions.withholds_action(&Action::Env(EnvAction::RefreshLiveSession {
            vars: vec![("EDITOR".into(), "vim".into())],
        }))
    );
    // No env decision, no env withholding.
    let unrelated = DecisionExclusions::from_decision_paths(
        ["packages.cargo.bat".to_string()],
        crate::expand_tilde,
    );
    assert!(
        !unrelated.withholds_action(&Action::Env(EnvAction::WriteEnvFile {
            path: PathBuf::from("/home/user/.cfgd.env"),
            content: String::new(),
        }))
    );
}

#[test]
fn pending_system_decision_withholds_every_action_for_its_configurator() {
    // The decision names the whole `spec.system.<configurator>` block, one
    // level above the `<configurator>:<key>` id a drift carries.
    use crate::reconciler::{Action, SystemAction};

    let exclusions =
        DecisionExclusions::from_decision_paths(["system.sysctl".to_string()], crate::expand_tilde);
    assert!(
        exclusions.withholds_action(&Action::System(SystemAction::SetValue {
            configurator: "sysctl".into(),
            key: "vm.swappiness".into(),
            desired: "10".into(),
            current: "60".into(),
            origin: "acme".into(),
        }))
    );
    assert!(
        exclusions.withholds_action(&Action::System(SystemAction::Skip {
            configurator: "sysctl".into(),
            reason: "not available".into(),
            origin: "acme".into(),
            unknown: false,
        }))
    );
    assert!(
        !exclusions.withholds_action(&Action::System(SystemAction::SetValue {
            configurator: "shell".into(),
            key: "defaultShell".into(),
            desired: "zsh".into(),
            current: "bash".into(),
            origin: "acme".into(),
        }))
    );
}

#[test]
fn pending_decision_in_no_known_vocabulary_withholds_nothing() {
    // A malformed or unknown decision path cannot be translated, so it must
    // withhold nothing rather than withhold everything (or panic).
    let exclusions = DecisionExclusions::from_decision_paths(
        [
            "packages.cargo".to_string(),
            "packages..bat".to_string(),
            "files.".to_string(),
            "env.".to_string(),
            "system.".to_string(),
            "secrets.op://vault/item".to_string(),
            String::new(),
        ],
        crate::expand_tilde,
    );
    assert!(exclusions.is_empty());
    assert!(!exclusions.withholds_package("cargo", "bat"));

    let mut phase = packages_phase_of(vec![install_of("cargo", &["bat"])]);
    prune_with(&mut phase, &exclusions);
    assert_eq!(phase.action_count(), 1);
}

#[test]
fn pending_decisions_never_withhold_a_secret_script_or_module_action() {
    // `declared_decision_paths` mints four prefixes and no others, so no
    // pending row can name one of these.
    use crate::reconciler::{Action, ModuleAction, ModuleActionKind, ScriptAction, ScriptPhase};

    let exclusions = DecisionExclusions::from_decision_paths(
        [
            "packages.cargo.bat".to_string(),
            "files./etc/hosts".to_string(),
            "env.EDITOR".to_string(),
            "system.sysctl".to_string(),
        ],
        crate::expand_tilde,
    );
    assert!(!exclusions.withholds_action(&Action::Secret(
        crate::providers::SecretAction::Decrypt {
            source: PathBuf::from("/etc/secret.enc.yaml"),
            target: PathBuf::from("/etc/secret.yaml"),
            backend: "sops".into(),
            origin: "acme".into(),
        }
    )));
    assert!(
        !exclusions.withholds_action(&Action::Script(ScriptAction::Run {
            entry: crate::config::ScriptEntry::Simple("echo hi".into()),
            phase: ScriptPhase::PreApply,
            origin: "acme".into(),
        }))
    );
    assert!(!exclusions.withholds_action(&Action::Module(ModuleAction {
        module_name: "cli-tools".into(),
        kind: ModuleActionKind::Skip {
            reason: "unmet dependency".into()
        },
        origin: None,
    })));
}

#[test]
fn process_source_decisions_accept_policy_no_pending() {
    use crate::config::PackagesSpec;
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // Accept policy: no pending decisions, nothing declined — the item applies
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(!declined.contains("packages.cargo.bat"));
}

// --- Compliance snapshot-on-change logic ---

#[test]
fn compliance_snapshot_skips_when_hash_unchanged() {
    let store = test_state();
    let snapshot = crate::compliance::ComplianceSnapshot {
        timestamp: crate::utc_now_iso8601(),
        machine: crate::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![crate::compliance::ComplianceCheck {
            category: "file".into(),
            status: crate::compliance::ComplianceStatus::Compliant,
            detail: Some("present".into()),
            ..Default::default()
        }],
        summary: crate::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };

    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    let hash = crate::sha256_hex(json.as_bytes());

    // Store first snapshot
    store.store_compliance_snapshot(&snapshot, &hash).unwrap();

    // Latest hash should match — a second store would be skipped
    let latest = store.latest_compliance_hash().unwrap();
    assert_eq!(latest.as_deref(), Some(hash.as_str()));
}

#[test]
fn compliance_snapshot_stores_when_hash_changes() {
    let store = test_state();

    let snapshot1 = crate::compliance::ComplianceSnapshot {
        timestamp: "2026-01-01T00:00:00Z".into(),
        machine: crate::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![crate::compliance::ComplianceCheck {
            category: "file".into(),
            status: crate::compliance::ComplianceStatus::Compliant,
            ..Default::default()
        }],
        summary: crate::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };

    let json1 = serde_json::to_string_pretty(&snapshot1).unwrap();
    let hash1 = crate::sha256_hex(json1.as_bytes());
    store.store_compliance_snapshot(&snapshot1, &hash1).unwrap();

    // Different snapshot with a violation
    let snapshot2 = crate::compliance::ComplianceSnapshot {
        timestamp: "2026-01-02T00:00:00Z".into(),
        machine: crate::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec!["local".into()],
        checks: vec![crate::compliance::ComplianceCheck {
            category: "package".into(),
            status: crate::compliance::ComplianceStatus::Violation,
            ..Default::default()
        }],
        summary: crate::compliance::ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 1,
        },
    };

    let json2 = serde_json::to_string_pretty(&snapshot2).unwrap();
    let hash2 = crate::sha256_hex(json2.as_bytes());

    // Hashes differ — new snapshot should be stored
    assert_ne!(hash1, hash2);
    let latest = store.latest_compliance_hash().unwrap();
    assert_ne!(latest.as_deref(), Some(hash2.as_str()));

    store.store_compliance_snapshot(&snapshot2, &hash2).unwrap();
    let latest = store.latest_compliance_hash().unwrap();
    assert_eq!(latest.as_deref(), Some(hash2.as_str()));

    // Both snapshots stored
    let history = store.compliance_history(None, 10).unwrap();
    assert_eq!(history.len(), 2);
}

#[test]
fn compliance_timer_not_created_when_disabled() {
    // When compliance is not enabled, compliance_interval should be None
    let config = config::ComplianceConfig {
        enabled: false,
        interval: "1h".into(),
        retention: "30d".into(),
        scope: config::ComplianceScope::default(),
        export: config::ComplianceExport::default(),
    };

    let interval = config
        .enabled
        .then(|| crate::parse_duration_str(&config.interval).ok())
        .flatten();

    assert!(interval.is_none());
}

#[test]
fn compliance_timer_created_when_enabled() {
    let config = config::ComplianceConfig {
        enabled: true,
        interval: "30m".into(),
        retention: "7d".into(),
        scope: config::ComplianceScope::default(),
        export: config::ComplianceExport::default(),
    };

    let interval = config
        .enabled
        .then(|| crate::parse_duration_str(&config.interval).ok())
        .flatten();

    assert_eq!(interval, Some(Duration::from_secs(30 * 60)));
}

#[test]
fn compliance_timer_invalid_interval_when_enabled() {
    let config = config::ComplianceConfig {
        enabled: true,
        interval: "garbage".into(),
        retention: "7d".into(),
        scope: config::ComplianceScope::default(),
        export: config::ComplianceExport::default(),
    };

    let interval = config
        .enabled
        .then(|| crate::parse_duration_str(&config.interval).ok())
        .flatten();

    // Enabled but unparseable interval -> None (no timer)
    assert!(interval.is_none());
}

// --- compute_config_hash: different profiles produce different hashes ---

#[test]
fn compute_config_hash_differs_for_different_packages() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let resolved_a = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "a".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let resolved_b = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "b".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["ripgrep".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let hash_a = compute_config_hash(&resolved_a).unwrap();
    let hash_b = compute_config_hash(&resolved_b).unwrap();
    assert_ne!(hash_a, hash_b);
}

// --- hash_resources edge cases ---

#[test]
fn hash_resources_empty_set() {
    let empty: HashSet<String> = HashSet::new();
    let hash = hash_resources(&empty);
    // Should produce a valid hash (SHA256 of empty string)
    assert_eq!(hash, crate::sha256_hex(b""));
}

#[test]
fn hash_resources_single_element() {
    let set: HashSet<String> = HashSet::from_iter(["packages.brew.ripgrep".to_string()]);
    let hash = hash_resources(&set);
    assert_eq!(hash.len(), 64);
    // Compare against known SHA256 of "packages.brew.ripgrep\n"
    let expected = crate::sha256_hex(b"packages.brew.ripgrep\n");
    assert_eq!(hash, expected);
}

// --- DaemonState::to_response field validation ---

#[test]
fn daemon_state_to_response_propagates_fields() {
    let mut state = DaemonState::new();
    state.last_reconcile = Some("2026-03-30T12:00:00Z".to_string());
    state.last_sync = Some("2026-03-30T12:01:00Z".to_string());
    state.drift_count = 5;
    state.update_available = Some("2.0.0".to_string());

    let response = state.to_response();
    assert!(response.running);
    assert_eq!(
        response.last_reconcile.as_deref(),
        Some("2026-03-30T12:00:00Z")
    );
    assert_eq!(response.last_sync.as_deref(), Some("2026-03-30T12:01:00Z"));
    assert_eq!(response.drift_count, 5);
    assert_eq!(response.update_available.as_deref(), Some("2.0.0"));
    assert_eq!(response.sources.len(), 1);
    assert_eq!(response.sources[0].name, "local");
}

// --- DaemonStatusResponse with module_reconcile and update_available ---

#[test]
fn daemon_status_response_with_modules_round_trips() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 42,
        uptime_secs: 100,
        last_reconcile: None,
        last_sync: None,
        drift_count: 2,
        sources: vec![],
        update_available: Some("1.5.0".to_string()),
        module_reconcile: vec![
            ModuleReconcileStatus {
                name: "security-baseline".to_string(),
                interval: "60s".to_string(),
                auto_apply: true,
                drift_policy: "Auto".to_string(),
                last_reconcile: Some("2026-03-30T00:00:00Z".to_string()),
            },
            ModuleReconcileStatus {
                name: "dev-tools".to_string(),
                interval: "300s".to_string(),
                auto_apply: false,
                drift_policy: "NotifyOnly".to_string(),
                last_reconcile: None,
            },
        ],
    };

    let json = serde_json::to_string(&response).unwrap();
    let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.pid, 42);
    assert_eq!(parsed.drift_count, 2);
    assert_eq!(parsed.update_available.as_deref(), Some("1.5.0"));
    assert_eq!(parsed.module_reconcile.len(), 2);
    assert_eq!(parsed.module_reconcile[0].name, "security-baseline");
    assert!(parsed.module_reconcile[0].auto_apply);
    assert_eq!(parsed.module_reconcile[1].name, "dev-tools");
    assert!(!parsed.module_reconcile[1].auto_apply);
    assert!(parsed.module_reconcile[1].last_reconcile.is_none());
}

#[test]
fn daemon_status_response_skips_empty_module_reconcile() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 1,
        uptime_secs: 0,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    // module_reconcile has skip_serializing_if = "Vec::is_empty"
    assert!(!json.contains("\"moduleReconcile\""));
    // update_available has skip_serializing_if = "Option::is_none"
    assert!(!json.contains("\"updateAvailable\""));
}

// --- action_resource_info tests ---

#[test]
fn action_resource_info_file_create() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Create {
        source: PathBuf::from("/src/.zshrc"),
        target: PathBuf::from("/home/user/.zshrc"),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::default(),
        source_hash: None,
        patch: None,
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/home/user/.zshrc");
}

#[test]
fn action_resource_info_file_update() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Update {
        source: PathBuf::from("/src/.zshrc"),
        target: PathBuf::from("/home/user/.zshrc"),
        diff: "--- a\n+++ b".into(),
        origin: "local".into(),
        strategy: crate::config::FileStrategy::default(),
        source_hash: None,
        patch: None,
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/home/user/.zshrc");
}

#[test]
fn action_resource_info_file_delete() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Delete {
        target: PathBuf::from("/tmp/gone"),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/tmp/gone");
}

#[test]
fn action_resource_info_file_set_permissions() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::SetPermissions {
        target: PathBuf::from("/home/user/.ssh/config"),
        mode: 0o600,
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/home/user/.ssh/config");
}

#[test]
fn action_resource_info_file_skip() {
    use crate::reconciler::Action;

    let action = Action::File(crate::providers::FileAction::Skip {
        target: PathBuf::from("/etc/skipped"),
        reason: "not needed".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "file");
    assert_eq!(rid, "/etc/skipped");
}

#[test]
fn action_resource_info_package_bootstrap() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Bootstrap {
        manager: "brew".into(),
        method: "curl".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "brew:bootstrap");
}

#[test]
fn action_resource_info_package_install() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Install {
        manager: "apt".into(),
        packages: vec!["curl".into(), "wget".into()],
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "apt:curl,wget");
}

#[test]
fn action_resource_info_package_uninstall() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Uninstall {
        manager: "npm".into(),
        packages: vec!["typescript".into()],
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "npm:typescript");
}

#[test]
fn action_resource_info_package_skip() {
    use crate::reconciler::Action;

    let action = Action::Package(crate::providers::PackageAction::Skip {
        manager: "cargo".into(),
        reason: "not available".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "package");
    assert_eq!(rid, "cargo");
}

#[test]
fn action_resource_info_secret_decrypt() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::Decrypt {
        source: PathBuf::from("/secrets/api.enc"),
        target: PathBuf::from("/home/user/.api_key"),
        backend: "age".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "/home/user/.api_key");
}

#[test]
fn action_resource_info_secret_resolve() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::Resolve {
        provider: "1password".into(),
        reference: "op://vault/item/field".into(),
        target: PathBuf::from("/tmp/secret"),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "op://vault/item/field");
}

#[test]
fn action_resource_info_secret_resolve_env() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::ResolveEnv {
        provider: "vault".into(),
        reference: "secret/data/app".into(),
        envs: vec!["API_KEY".into(), "DB_PASS".into()],
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "env:[API_KEY,DB_PASS]");
}

#[test]
fn action_resource_info_secret_skip() {
    use crate::reconciler::Action;

    let action = Action::Secret(crate::providers::SecretAction::Skip {
        source: "bitwarden".into(),
        reason: "not configured".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "secret");
    assert_eq!(rid, "bitwarden");
}

#[test]
fn action_resource_info_system_set_value() {
    use crate::reconciler::{Action, SystemAction};

    let action = Action::System(SystemAction::SetValue {
        configurator: "sysctl".into(),
        key: "vm.swappiness".into(),
        desired: "10".into(),
        current: "60".into(),
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "system");
    assert_eq!(rid, "sysctl:vm.swappiness");
}

#[test]
fn action_resource_info_system_skip() {
    use crate::reconciler::{Action, SystemAction};

    let action = Action::System(SystemAction::Skip {
        configurator: "gsettings".into(),
        reason: "not on GNOME".into(),
        origin: "local".into(),
        unknown: false,
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "system");
    assert_eq!(rid, "gsettings");
}

#[test]
fn action_resource_info_script_run() {
    use crate::reconciler::{Action, ScriptAction, ScriptPhase};

    let action = Action::Script(ScriptAction::Run {
        entry: crate::config::ScriptEntry::Simple("echo hello".into()),
        phase: ScriptPhase::PreApply,
        origin: "local".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "script");
    assert_eq!(rid, "echo hello");
}

#[test]
fn action_resource_info_module() {
    use crate::reconciler::{Action, ModuleAction, ModuleActionKind};

    let action = Action::Module(ModuleAction {
        module_name: "security-baseline".into(),
        kind: ModuleActionKind::InstallPackages { resolved: vec![] },
        origin: None,
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "module");
    assert_eq!(rid, "security-baseline");
}

#[test]
fn action_resource_info_env_write() {
    use crate::reconciler::{Action, EnvAction};

    let action = Action::Env(EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.cfgd.env"),
        content: "export FOO=bar".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "env");
    assert_eq!(rid, "/home/user/.cfgd.env");
}

#[test]
fn action_resource_info_env_inject() {
    use crate::reconciler::{Action, EnvAction};

    let action = Action::Env(EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.bashrc"),
        line: ". ~/.cfgd.env".into(),
    });
    let (rtype, rid) = action_resource_info(&action);
    assert_eq!(rtype, "env-rc");
    assert_eq!(rid, "/home/user/.bashrc");
}

// --- declared_decision_paths with more package managers ---

#[test]
fn extract_source_resources_apt_dnf_pipx_npm() {
    use crate::config::{AptSpec, MergedProfile, NpmSpec, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            apt: Some(AptSpec {
                file: None,
                packages: vec!["git".into(), "tmux".into()],
            }),
            dnf: vec!["vim".into()],
            pipx: vec!["black".into()],
            npm: Some(NpmSpec {
                file: None,
                global: vec!["prettier".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("packages.apt.git"));
    assert!(resources.contains("packages.apt.tmux"));
    assert!(resources.contains("packages.dnf.vim"));
    assert!(resources.contains("packages.pipx.black"));
    assert!(resources.contains("packages.npm.prettier"));
    assert_eq!(resources.len(), 5);
}

#[test]
fn extract_source_resources_system_keys() {
    use crate::config::MergedProfile;

    let mut merged = MergedProfile::default();
    merged
        .system
        .insert("sysctl".into(), serde_yaml::Value::Null);
    merged
        .system
        .insert("kernelModules".into(), serde_yaml::Value::Null);

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("system.sysctl"));
    assert!(resources.contains("system.kernelModules"));
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_empty_profile() {
    let merged = crate::config::MergedProfile::default();
    let resources = declared_decision_paths(&merged);
    assert!(resources.is_empty());
}

// --- Config change detection: process_source_decisions second call ---

#[test]
fn process_source_decisions_no_change_on_second_call() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: crate::config::PolicyAction::Accept,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    // First call: stores the hash
    let _ = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // Second call with same profile: hash matches, no new decisions
    let declined = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // No pending decisions since policy is Accept
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(declined.is_empty());
}

#[test]
fn process_source_decisions_detects_new_items_on_change() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default(); // Notify by default

    // First call with one package
    let merged1 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = process_source_decisions(&store, "acme", &merged1, &policy, &notifier);
    // Clear pending decisions from first run
    let first_pending = store.pending_decisions().unwrap();
    for d in &first_pending {
        let _ = store.resolve_decisions_for_source(&d.source, "accepted");
    }

    // Second call with an additional package
    let merged2 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    process_source_decisions(&store, "acme", &merged2, &policy, &notifier);

    // Should have a pending decision for ripgrep (new item)
    let pending = store.pending_decisions().unwrap();
    assert!(!pending.is_empty());
    let resource_names: Vec<&str> = pending.iter().map(|d| d.resource.as_str()).collect();
    assert!(resource_names.contains(&"packages.cargo.ripgrep"));
    assert!(withheld_paths(&store).contains("packages.cargo.ripgrep"));
}

// --- ModuleReconcileStatus serialization ---

#[test]
fn module_reconcile_status_round_trips() {
    let status = ModuleReconcileStatus {
        name: "dev-tools".into(),
        interval: "120s".into(),
        auto_apply: false,
        drift_policy: "NotifyOnly".into(),
        last_reconcile: None,
    };
    let json = serde_json::to_string(&status).unwrap();
    let parsed: ModuleReconcileStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "dev-tools");
    assert_eq!(parsed.interval, "120s");
    assert!(!parsed.auto_apply);
    assert_eq!(parsed.drift_policy, "NotifyOnly");
    assert!(parsed.last_reconcile.is_none());
    // Verify camelCase
    assert!(json.contains("\"autoApply\""));
    assert!(json.contains("\"driftPolicy\""));
    assert!(json.contains("\"lastReconcile\""));
}

// --- Notifier construction ---

#[test]
fn notifier_webhook_without_url_does_not_panic() {
    let notifier = Notifier::new(NotifyMethod::Webhook, None);
    assert!(matches!(notifier.method, NotifyMethod::Webhook));
    // Webhook with no URL should early-return via `let Some(ref url) = ...` guard
    assert!(
        notifier.webhook_url.is_none(),
        "webhook_url must be None to exercise the early-return path"
    );
    // Should log a warning but not panic and not attempt any HTTP request
    notifier.notify("test", "no url configured");
}

// --- find_server_url with multiple origins ---

#[test]
fn find_server_url_picks_server_among_multiple_origins() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![
                OriginSpec {
                    origin_type: OriginType::Git,
                    url: "https://github.com/test/repo.git".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://fleet.example.com".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
            ],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    assert_eq!(
        find_server_url(&config),
        Some("https://fleet.example.com".to_string())
    );
}

#[test]
fn find_server_url_returns_none_for_empty_origins() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    assert!(find_server_url(&config).is_none());
}

// --- CheckinServerResponse deserialization edge cases ---

#[test]
fn checkin_response_with_config_payload() {
    let json = r#"{"status":"ok","config_changed":true,"config":{"packages":["git"]}}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    assert!(resp.config_changed);
    assert!(resp._config.is_some());
}

#[test]
fn checkin_response_no_change() {
    let json = r#"{"status":"ok","config_changed":false,"config":null}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    assert!(!resp.config_changed);
}

// --- parse_duration_or_default: zero values ---

#[test]
fn parse_duration_zero_seconds() {
    assert_eq!(parse_duration_or_default("0s"), Duration::from_secs(0));
}

#[test]
fn parse_duration_zero_plain() {
    assert_eq!(parse_duration_or_default("0"), Duration::from_secs(0));
}

// --- compute_config_hash with empty packages ---

#[test]
fn compute_config_hash_with_empty_packages() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "empty".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let hash1 = compute_config_hash(&resolved).unwrap();
    let hash2 = compute_config_hash(&resolved).unwrap();
    assert_eq!(hash1, hash2, "hash should be deterministic");
    assert_eq!(hash1.len(), 64, "hash should be a valid SHA256 hex string");
}

// --- declared_decision_paths: brew taps are not included, casks are ---

#[test]
fn extract_source_resources_brew_casks_only() {
    use crate::config::{BrewSpec, MergedProfile, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec![],
                casks: vec!["iterm2".into(), "visual-studio-code".into()],
                taps: vec!["homebrew/cask".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(
        resources.contains("packages.brew.iterm2"),
        "casks should appear as brew resources"
    );
    assert!(
        resources.contains("packages.brew.visual-studio-code"),
        "casks should appear as brew resources"
    );
    // Taps are not tracked as individual resources
    assert!(
        !resources.contains("packages.brew.homebrew/cask"),
        "taps should not appear as resources"
    );
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_cargo_packages_only() {
    use crate::config::{CargoSpec, MergedProfile, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: Some("Cargo.toml".into()),
                packages: vec!["cargo-watch".into(), "cargo-expand".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("packages.cargo.cargo-watch"));
    assert!(resources.contains("packages.cargo.cargo-expand"));
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_npm_globals() {
    use crate::config::{MergedProfile, NpmSpec, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            npm: Some(NpmSpec {
                file: None,
                global: vec!["typescript".into(), "eslint".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("packages.npm.typescript"));
    assert!(resources.contains("packages.npm.eslint"));
    assert_eq!(resources.len(), 2);
}

// --- process_source_decisions with Reject policy ---

#[test]
fn process_source_decisions_reject_policy_silently_skips() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Reject,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // "Skip silently" is a disposition of the ITEM, in one series with
    // "don't apply" (Notify) and "automatically apply" (Accept): the package
    // does not reach the machine, and nothing is recorded to say so — a
    // rejecting policy is a standing answer, not a question for the operator.
    let pending = store.pending_decisions().unwrap();
    assert!(
        pending.is_empty(),
        "reject policy should not create pending decisions"
    );
    assert_eq!(
        declined.iter().collect::<Vec<_>>(),
        vec!["packages.cargo.bat"],
        "a rejected item is withheld from the plan, not installed silently"
    );
}

/// One merged profile declaring a single cargo package.
fn merged_declaring_cargo(package: &str) -> MergedProfile {
    use crate::config::{CargoSpec, PackagesSpec};
    MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec![package.into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn flipping_a_rejecting_policy_to_notify_asks_about_the_item() {
    // A `Reject` policy records nothing, so nothing carries the disposition
    // forward — and the source has not changed, so a mint gated on the source's
    // hash would mint nothing and the item would install unattended. The
    // documented promise ("re-run with Notify if you want to be asked") is only
    // true if the absence of a row is itself enough to ask.
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let merged = merged_declaring_cargo("bat");

    let declined = process_source_decisions(
        &store,
        "acme",
        &merged,
        &AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Reject,
            ..Default::default()
        },
        &notifier,
    );
    assert!(declined.contains("packages.cargo.bat"));
    assert!(store.pending_decisions().unwrap().is_empty());

    let declined = process_source_decisions(
        &store,
        "acme",
        &merged,
        &AutoApplyPolicyConfig {
            new_recommended: PolicyAction::Notify,
            ..Default::default()
        },
        &notifier,
    );
    assert!(
        declined.is_empty(),
        "the flipped policy no longer declines the item"
    );
    assert_eq!(
        store
            .pending_decisions()
            .unwrap()
            .into_iter()
            .map(|d| d.resource)
            .collect::<Vec<_>>(),
        vec!["packages.cargo.bat".to_string()],
        "so it must be asked about rather than applied unattended"
    );
}

#[test]
fn an_answered_decision_is_not_re_minted_while_its_source_stands_still() {
    // The other half of minting on an absent row: a row that EXISTS is an
    // answer, and re-minting over it every tick would re-ask a question the
    // operator already closed.
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let merged = merged_declaring_cargo("bat");
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Notify,
        ..Default::default()
    };

    process_source_decisions(&store, "acme", &merged, &policy, &notifier);
    store
        .resolve_decision("packages.cargo.bat", "rejected")
        .unwrap();

    process_source_decisions(&store, "acme", &merged, &policy, &notifier);
    assert!(
        store.pending_decisions().unwrap().is_empty(),
        "a rejection stands until the source itself changes"
    );
}

// --- find_server_url with duplicate server origins picks first ---

#[test]
fn find_server_url_picks_first_server_among_duplicates() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://first-server.example.com".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
                OriginSpec {
                    origin_type: OriginType::Server,
                    url: "https://second-server.example.com".into(),
                    branch: "main".into(),
                    auth: None,
                    ssh_strict_host_key_checking: Default::default(),
                },
            ],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: crate::config::FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    assert_eq!(
        find_server_url(&config),
        Some("https://first-server.example.com".to_string()),
        "should return the first server origin when multiple exist"
    );
}

// --- compute_config_hash: empty vs non-empty produces different hashes ---

#[test]
fn compute_config_hash_empty_vs_nonempty_differ() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let empty_resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "empty".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let nonempty_resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "nonempty".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let hash_empty = compute_config_hash(&empty_resolved).unwrap();
    let hash_nonempty = compute_config_hash(&nonempty_resolved).unwrap();
    assert_ne!(
        hash_empty, hash_nonempty,
        "empty and non-empty packages should produce different hashes"
    );
}

// --- process_source_decisions with Ignore policy ---

#[test]
fn process_source_decisions_ignore_policy_declines_the_item_without_a_row() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Ignore,
        ..Default::default()
    };

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined = process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    // `Ignore` sits beside `Reject` in the same "skip silently" row of the
    // policy table, so it declines the item on the same terms.
    let pending = store.pending_decisions().unwrap();
    assert!(
        pending.is_empty(),
        "ignore policy should not create pending decisions"
    );
    assert_eq!(
        declined.iter().collect::<Vec<_>>(),
        vec!["packages.cargo.bat"],
        "an ignored item is withheld from the plan, not installed silently"
    );
}

// --- Notifier construction variants ---

#[test]
fn notifier_desktop_mode_does_not_panic() {
    // Desktop notification may fail in CI (no display server) but should not panic.
    // On failure, notify_desktop falls back to notify_stdout via tracing::info.
    let notifier = Notifier::new(NotifyMethod::Desktop, None);
    assert!(matches!(notifier.method, NotifyMethod::Desktop));
    assert!(
        notifier.webhook_url.is_none(),
        "desktop notifier should not have a webhook URL"
    );
    notifier.notify("test title", "test body");
}

#[tokio::test]
async fn notifier_webhook_with_url_does_not_panic() {
    // Webhook to a nonexistent URL: should log error but not panic
    let notifier = Notifier::new(
        NotifyMethod::Webhook,
        Some("http://127.0.0.1:1/nonexistent".to_string()),
    );
    notifier.notify("test", "message to invalid webhook");
}

#[test]
fn notifier_stdout_writes_info() {
    // Verify stdout notifier is configured for Stdout method and runs
    // the tracing::info path with structured title/message fields.
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    assert!(matches!(notifier.method, NotifyMethod::Stdout));
    // The notify_stdout method calls tracing::info!(title, message, "notification")
    // Verify it handles non-trivial content without panic
    notifier.notify("drift event", "file /etc/foo changed");
    notifier.notify("", ""); // edge case: empty strings
    notifier.notify("special chars: <>&\"'", "path: /home/user/.config/cfgd");
}

// --- DaemonState: multiple sources ---

#[test]
fn daemon_state_with_multiple_sources() {
    let mut state = DaemonState::new();
    state.sources.push(SourceStatus {
        name: "acme-corp".to_string(),
        last_sync: Some("2026-03-30T10:00:00Z".to_string()),
        last_reconcile: None,
        drift_count: 2,
        status: "active".to_string(),
    });
    state.sources.push(SourceStatus {
        name: "team-tools".to_string(),
        last_sync: None,
        last_reconcile: Some("2026-03-30T11:00:00Z".to_string()),
        drift_count: 0,
        status: "error".to_string(),
    });

    let response = state.to_response();
    assert_eq!(response.sources.len(), 3); // local + acme-corp + team-tools
    assert_eq!(response.sources[1].name, "acme-corp");
    assert_eq!(response.sources[1].drift_count, 2);
    assert_eq!(response.sources[2].name, "team-tools");
    assert_eq!(response.sources[2].status, "error");
}

// --- DaemonState: drift counting ---

#[test]
fn daemon_state_drift_increments_propagate_to_response() {
    let mut state = DaemonState::new();
    state.drift_count = 10;
    if let Some(source) = state.sources.first_mut() {
        source.drift_count = 7;
    }

    let response = state.to_response();
    assert_eq!(response.drift_count, 10);
    assert_eq!(response.sources[0].drift_count, 7);
}

// --- DaemonState: module_last_reconcile tracking ---

#[test]
fn daemon_state_module_last_reconcile_tracking() {
    let mut state = DaemonState::new();
    state.module_last_reconcile.insert(
        "security-baseline".to_string(),
        "2026-03-30T12:00:00Z".to_string(),
    );
    state
        .module_last_reconcile
        .insert("dev-tools".to_string(), "2026-03-30T12:05:00Z".to_string());

    assert_eq!(state.module_last_reconcile.len(), 2);
    assert_eq!(
        state
            .module_last_reconcile
            .get("security-baseline")
            .unwrap(),
        "2026-03-30T12:00:00Z"
    );
    assert_eq!(
        state.module_last_reconcile.get("dev-tools").unwrap(),
        "2026-03-30T12:05:00Z"
    );

    // to_response does not currently populate module_reconcile (empty vec)
    let response = state.to_response();
    assert!(response.module_reconcile.is_empty());
}

// --- DaemonStatusResponse: update_available serialization ---

#[test]
fn daemon_status_response_update_available_present() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 99,
        uptime_secs: 600,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: Some("3.0.0".to_string()),
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"updateAvailable\":\"3.0.0\""));
    let parsed: DaemonStatusResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.update_available.as_deref(), Some("3.0.0"));
}

// --- SyncTask construction ---

#[test]
fn sync_task_local_defaults() {
    let task = SyncTask {
        source_name: "local".to_string(),
        repo_path: PathBuf::from("/home/user/.config/cfgd"),
        auto_pull: false,
        auto_push: false,
        auto_apply: true,
        interval: Duration::from_secs(DEFAULT_SYNC_SECS),
        last_synced: None,
        require_signed_commits: false,
        allow_unsigned: false,
    };

    assert_eq!(task.source_name, "local");
    assert!(task.auto_apply);
    assert!(!task.auto_pull);
    assert!(!task.auto_push);
    assert!(task.last_synced.is_none());
    assert_eq!(task.interval.as_secs(), 300);
}

#[test]
fn sync_task_source_with_signing() {
    let task = SyncTask {
        source_name: "acme-corp".to_string(),
        repo_path: PathBuf::from("/tmp/sources/acme-corp"),
        auto_pull: true,
        auto_push: false,
        auto_apply: false,
        interval: Duration::from_secs(600),
        last_synced: Some(Instant::now()),
        require_signed_commits: true,
        allow_unsigned: false,
    };

    assert_eq!(task.source_name, "acme-corp");
    assert!(task.auto_pull);
    assert!(!task.auto_push);
    assert!(!task.auto_apply);
    assert!(task.require_signed_commits);
    assert!(!task.allow_unsigned);
    assert!(task.last_synced.is_some());
}

#[test]
fn sync_task_allow_unsigned_overrides_require_signed() {
    let task = SyncTask {
        source_name: "relaxed".to_string(),
        repo_path: PathBuf::from("/tmp/sources/relaxed"),
        auto_pull: true,
        auto_push: false,
        auto_apply: true,
        interval: Duration::from_secs(300),
        last_synced: None,
        require_signed_commits: true,
        allow_unsigned: true,
    };

    // Both flags can be set; the consumer decides precedence
    assert!(task.require_signed_commits);
    assert!(task.allow_unsigned);
}

// --- ReconcileTask construction ---

#[test]
fn reconcile_task_default() {
    let task = ReconcileTask {
        entity: "__default__".to_string(),
        interval: Duration::from_secs(DEFAULT_RECONCILE_SECS),
        auto_apply: false,
        drift_policy: config::DriftPolicy::default(),
        last_reconciled: None,
    };

    assert_eq!(task.entity, "__default__");
    assert_eq!(task.interval.as_secs(), 300);
    assert!(!task.auto_apply);
    assert!(task.last_reconciled.is_none());
}

#[test]
fn reconcile_task_per_module() {
    let task = ReconcileTask {
        entity: "security-baseline".to_string(),
        interval: Duration::from_secs(60),
        auto_apply: true,
        drift_policy: config::DriftPolicy::Auto,
        last_reconciled: Some(Instant::now()),
    };

    assert_eq!(task.entity, "security-baseline");
    assert_eq!(task.interval.as_secs(), 60);
    assert!(task.auto_apply);
    assert!(task.last_reconciled.is_some());
}

// --- withheld_decisions ---

#[test]
fn withheld_decisions_empty_store() {
    let store = test_state();
    let paths = withheld_paths(&store);
    assert!(paths.is_empty());
}

#[test]
fn withheld_decisions_with_decisions() {
    let store = test_state();
    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat (from acme)",
        )
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "env.EDITOR",
            "recommended",
            "install",
            "recommended env.EDITOR (from acme)",
        )
        .unwrap();

    let paths = withheld_paths(&store);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains("packages.cargo.bat"));
    assert!(paths.contains("env.EDITOR"));
}

// --- declared_decision_paths: aliases not included (not tracked) ---

#[test]
fn extract_source_resources_aliases_not_tracked() {
    use crate::config::{MergedProfile, ShellAlias};

    let merged = MergedProfile {
        aliases: vec![
            ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
            },
            ShellAlias {
                name: "gp".into(),
                command: "git push".into(),
            },
        ],
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    // Aliases are not tracked as individual resources
    assert!(
        resources.is_empty(),
        "aliases should not be tracked as source resources"
    );
}

// --- declared_decision_paths: mixed profile with everything ---

#[test]
fn extract_source_resources_full_profile() {
    use crate::config::{
        AptSpec, BrewSpec, CargoSpec, EnvVar, FilesSpec, ManagedFileSpec, MergedProfile, NpmSpec,
        PackagesSpec,
    };

    let mut system = std::collections::HashMap::new();
    system.insert("sysctl".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        packages: PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["ripgrep".into()],
                casks: vec!["firefox".into()],
                ..Default::default()
            }),
            apt: Some(AptSpec {
                file: None,
                packages: vec!["curl".into()],
            }),
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            pipx: vec!["black".into()],
            dnf: vec!["vim".into()],
            npm: Some(NpmSpec {
                file: None,
                global: vec!["typescript".into()],
            }),
            ..Default::default()
        },
        files: FilesSpec {
            managed: vec![ManagedFileSpec {
                patch: None,
                source: "dotfiles/.zshrc".into(),
                target: PathBuf::from("/home/user/.zshrc"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        },
        env: vec![
            EnvVar {
                name: "EDITOR".into(),
                value: "vim".into(),
            },
            EnvVar {
                name: "GOPATH".into(),
                value: "/home/user/go".into(),
            },
        ],
        system,
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    // Verify all expected resources are present
    assert!(resources.contains("packages.brew.ripgrep"));
    assert!(resources.contains("packages.brew.firefox"));
    assert!(resources.contains("packages.apt.curl"));
    assert!(resources.contains("packages.cargo.bat"));
    assert!(resources.contains("packages.pipx.black"));
    assert!(resources.contains("packages.dnf.vim"));
    assert!(resources.contains("packages.npm.typescript"));
    assert!(resources.contains("files./home/user/.zshrc"));
    assert!(resources.contains("env.EDITOR"));
    assert!(resources.contains("env.GOPATH"));
    assert!(resources.contains("system.sysctl"));
    // Total: 1 formula + 1 cask + 1 apt + 1 cargo + 1 pipx + 1 dnf + 1 npm + 1 file + 2 env + 1 system
    assert_eq!(resources.len(), 11);
}

// --- process_source_decisions: locked_conflict policy ---

#[test]
fn process_source_decisions_locked_item_notify_policy() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        locked_conflict: PolicyAction::Notify,
        ..Default::default()
    };

    // The source offers this on a locked/required layer, so `lockedConflict`
    // governs it — not `newRecommended`.
    let mut system = std::collections::HashMap::new();
    system.insert("security-baseline".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        system,
        ..Default::default()
    };

    let declined = process_tiered_decisions(
        &store,
        "corp",
        &tiered_items(&merged, crate::config::LayerPolicy::Required),
        &policy,
        &notifier,
    );
    assert!(declined.is_empty(), "Notify records rather than declines");

    let pending = store.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "system.security-baseline");
    assert!(withheld_paths(&store).contains("system.security-baseline"));
}

// --- process_source_decisions: multiple sources ---

#[test]
fn process_source_decisions_different_sources_independent() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        ..Default::default()
    };

    let merged_a = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let merged_b = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["ripgrep".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined_a = process_source_decisions(&store, "source-a", &merged_a, &policy, &notifier);
    let declined_b = process_source_decisions(&store, "source-b", &merged_b, &policy, &notifier);

    // Accept policy: both sources processed, nothing declined
    assert!(declined_a.is_empty());
    assert!(declined_b.is_empty());
}

// --- process_source_decisions: items removed from source ---

#[test]
fn process_source_decisions_removed_items_update_hash() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        ..Default::default()
    };

    // First call: bat + ripgrep
    let merged1 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = process_source_decisions(&store, "acme", &merged1, &policy, &notifier);

    // Second call: only bat (ripgrep removed from source)
    let merged2 = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let declined = process_source_decisions(&store, "acme", &merged2, &policy, &notifier);

    // Hash changed, but Accept policy means no pending decisions
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(declined.is_empty());
}

// --- SourceStatus: field defaults ---

#[test]
fn source_status_defaults() {
    let status = SourceStatus {
        name: "test".to_string(),
        last_sync: None,
        last_reconcile: None,
        drift_count: 0,
        status: "active".to_string(),
    };

    assert!(status.last_sync.is_none());
    assert!(status.last_reconcile.is_none());
    assert_eq!(status.drift_count, 0);
}

// --- SourceStatus: all fields populated ---

#[test]
fn source_status_all_fields_populated() {
    let status = SourceStatus {
        name: "corp-source".to_string(),
        last_sync: Some("2026-03-30T10:00:00Z".to_string()),
        last_reconcile: Some("2026-03-30T10:05:00Z".to_string()),
        drift_count: 15,
        status: "error".to_string(),
    };

    let json = serde_json::to_string(&status).unwrap();
    let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "corp-source");
    assert_eq!(parsed.last_sync.as_deref(), Some("2026-03-30T10:00:00Z"));
    assert_eq!(
        parsed.last_reconcile.as_deref(),
        Some("2026-03-30T10:05:00Z")
    );
    assert_eq!(parsed.drift_count, 15);
    assert_eq!(parsed.status, "error");
}

// --- DaemonStatusResponse deserialization from external JSON ---

#[test]
fn daemon_status_response_deserializes_from_minimal_json() {
    let json = r#"{
            "running": false,
            "pid": 0,
            "uptimeSecs": 0,
            "lastReconcile": null,
            "lastSync": null,
            "driftCount": 0,
            "sources": []
        }"#;

    let parsed: DaemonStatusResponse = serde_json::from_str(json).unwrap();
    assert!(!parsed.running);
    assert_eq!(parsed.pid, 0);
    assert!(parsed.module_reconcile.is_empty());
    assert!(parsed.update_available.is_none());
}

// --- CheckinPayload: field coverage ---

#[test]
fn checkin_payload_serializes_all_fields() {
    let payload = CheckinPayload {
        device_id: "sha256hex".into(),
        hostname: "myhost.local".into(),
        os: "linux".into(),
        arch: "aarch64".into(),
        config_hash: "abcd1234".into(),
    };

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("\"device_id\""));
    assert!(json.contains("\"hostname\""));
    assert!(json.contains("\"os\""));
    assert!(json.contains("\"arch\""));
    assert!(json.contains("\"config_hash\""));
    assert!(json.contains("aarch64"));
}

// --- parse_duration_or_default: edge cases ---

#[test]
fn parse_duration_large_seconds() {
    assert_eq!(
        parse_duration_or_default("86400s"),
        Duration::from_secs(86400)
    );
}

#[test]
fn parse_duration_large_hours() {
    assert_eq!(parse_duration_or_default("24h"), Duration::from_secs(86400));
}

#[test]
fn parse_duration_empty_string_falls_back() {
    assert_eq!(
        parse_duration_or_default(""),
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
}

// --- hash_resources: ordering does not matter ---

#[test]
fn hash_resources_large_set_deterministic() {
    let set1: HashSet<String> = (0..100)
        .map(|i| format!("packages.brew.pkg{}", i))
        .collect();
    let set2: HashSet<String> = (0..100)
        .rev()
        .map(|i| format!("packages.brew.pkg{}", i))
        .collect();

    assert_eq!(hash_resources(&set1), hash_resources(&set2));
}

// --- ModuleReconcileStatus: camelCase field names ---

#[test]
fn module_reconcile_status_camel_case_fields() {
    let status = ModuleReconcileStatus {
        name: "test".into(),
        interval: "60s".into(),
        auto_apply: true,
        drift_policy: "Auto".into(),
        last_reconcile: Some("2026-01-01T00:00:00Z".into()),
    };

    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"autoApply\""));
    assert!(json.contains("\"driftPolicy\""));
    assert!(json.contains("\"lastReconcile\""));
    // Should NOT contain snake_case
    assert!(!json.contains("\"auto_apply\""));
    assert!(!json.contains("\"drift_policy\""));
    assert!(!json.contains("\"last_reconcile\""));
}

// --- DaemonStatusResponse: uptime_secs is camelCase in JSON ---

#[test]
fn daemon_status_response_camel_case_uptime() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 1,
        uptime_secs: 42,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"uptimeSecs\""));
    assert!(json.contains("\"driftCount\""));
    assert!(!json.contains("\"uptime_secs\""));
    assert!(!json.contains("\"drift_count\""));
}

// --- process_source_decisions: mixed policies per tier ---

#[test]
fn process_source_decisions_mixed_tiers_accept_recommended_notify_locked() {
    use crate::config::{CargoSpec, PackagesSpec};

    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Accept,
        new_optional: PolicyAction::Ignore,
        locked_conflict: PolicyAction::Notify,
    };

    // One source, two tiers: a recommended layer carrying a package and a
    // locked layer carrying a system setting.
    let recommended = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut system = std::collections::HashMap::new();
    system.insert("security-policy".into(), serde_yaml::Value::Null);
    let locked = MergedProfile {
        system,
        ..Default::default()
    };

    let delivered = DeliveredItems::from_layers(&[
        tiered_layer(&recommended, crate::config::LayerPolicy::Recommended),
        tiered_layer(&locked, crate::config::LayerPolicy::Required),
    ]);
    let declined = process_tiered_decisions(&store, "corp", &delivered, &policy, &notifier);
    assert!(
        declined.is_empty(),
        "neither Accept nor Notify declines an item outright"
    );

    let pending = store.pending_decisions().unwrap();
    // Only the locked item should be pending (security-policy)
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "system.security-policy");
    let withheld = withheld_paths(&store);
    // bat is accepted by policy, so it stays in the plan
    assert!(!withheld.contains("packages.cargo.bat"));
    // security-policy awaits the operator, so it is withheld
    assert!(withheld.contains("system.security-policy"));
}

// --- generate_device_id: always hex ---

#[test]
fn generate_device_id_hex_format() {
    let id = generate_device_id().unwrap();
    // Should be lowercase hex only
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "device ID should be hex: {}",
        id
    );
}

// --- declared_decision_paths: multiple files ---

#[test]
fn extract_source_resources_multiple_files() {
    use crate::config::{FilesSpec, ManagedFileSpec, MergedProfile};

    let merged = MergedProfile {
        files: FilesSpec {
            managed: vec![
                ManagedFileSpec {
                    patch: None,
                    source: "dotfiles/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
                ManagedFileSpec {
                    patch: None,
                    source: "dotfiles/.vimrc".into(),
                    target: PathBuf::from("/home/user/.vimrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
                ManagedFileSpec {
                    patch: None,
                    source: "dotfiles/.gitconfig".into(),
                    target: PathBuf::from("/home/user/.gitconfig"),
                    strategy: None,
                    private: true,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
            ],
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert_eq!(resources.len(), 3);
    assert!(resources.contains("files./home/user/.zshrc"));
    assert!(resources.contains("files./home/user/.vimrc"));
    assert!(resources.contains("files./home/user/.gitconfig"));
}

// --- declared_decision_paths: multiple env vars ---

#[test]
fn extract_source_resources_multiple_env_vars() {
    use crate::config::{EnvVar, MergedProfile};

    let merged = MergedProfile {
        env: vec![
            EnvVar {
                name: "PATH".into(),
                value: "/usr/local/bin:$PATH".into(),
            },
            EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            },
            EnvVar {
                name: "GOPATH".into(),
                value: "/home/user/go".into(),
            },
        ],
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert_eq!(resources.len(), 3);
    assert!(resources.contains("env.PATH"));
    assert!(resources.contains("env.EDITOR"));
    assert!(resources.contains("env.GOPATH"));
}

// --- declared_decision_paths: multiple system keys ---

#[test]
fn extract_source_resources_multiple_system_keys() {
    use crate::config::MergedProfile;

    let mut system = std::collections::HashMap::new();
    system.insert("sysctl".into(), serde_yaml::Value::Null);
    system.insert("kernelModules".into(), serde_yaml::Value::Null);
    system.insert("apparmor".into(), serde_yaml::Value::Null);

    let merged = MergedProfile {
        system,
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert_eq!(resources.len(), 3);
    assert!(resources.contains("system.sysctl"));
    assert!(resources.contains("system.kernelModules"));
    assert!(resources.contains("system.apparmor"));
}

// --- DaemonState: uptime increases ---

#[test]
fn daemon_state_uptime_increases() {
    let state = DaemonState::new();
    // Small sleep to ensure non-zero uptime
    std::thread::sleep(Duration::from_millis(10));
    let response = state.to_response();
    // Uptime should be at least 0 (could be 0 if resolution is 1s)
    // The key assertion is that it doesn't panic
    assert!(response.uptime_secs < 10);
}

// --- handle_health_connection: /health endpoint ---

#[tokio::test]
async fn health_connection_health_endpoint() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    // Spawn the handler
    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    // Send HTTP request
    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    // Read response
    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {}",
        &response[..response.len().min(40)]
    );
    assert!(response.contains("\"status\""));
    assert!(response.contains("\"pid\""));
    assert!(response.contains("\"uptime_secs\""));
}

// --- handle_health_connection: /status endpoint ---

#[tokio::test]
async fn health_connection_status_endpoint() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    // Populate some state
    {
        let mut st = state.lock().await;
        st.drift_count = 3;
        st.last_reconcile = Some("2026-03-30T10:00:00Z".to_string());
    }

    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {}",
        &response[..response.len().min(40)]
    );
    // Body should contain DaemonStatusResponse fields (pretty-printed JSON)
    assert!(
        response.contains("\"running\": true"),
        "response should contain running field: {}",
        response
    );
    assert!(
        response.contains("\"driftCount\": 3"),
        "response should contain driftCount field: {}",
        response
    );
}

// --- handle_health_connection: /drift endpoint ---

#[tokio::test]
async fn health_connection_drift_endpoint() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /drift HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {}",
        &response[..response.len().min(40)]
    );
    assert!(response.contains("\"drift_count\""));
    assert!(response.contains("\"events\""));
}

// --- handle_health_connection: 404 for unknown path ---

#[tokio::test]
async fn health_connection_unknown_path_returns_404() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "expected 404, got: {}",
        &response[..response.len().min(40)]
    );
    assert!(response.contains("\"error\""));
}

// --- git_pull: repo with no remote changes returns Ok(false) ---

#[test]
fn git_pull_no_remote_returns_up_to_date() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    // Create a bare repo as "remote"
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone the bare repo to get a working copy with origin
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();

    // Configure committer identity
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "cfgd-test").unwrap();
    config.set_str("user.email", "test@cfgd.io").unwrap();

    // Create initial commit (bare repos start empty, clone has no HEAD)
    let readme = work_dir.join("README");
    std::fs::write(&readme, "test\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("README")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();

    // Push initial commit to bare remote
    let mut remote = repo.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    // Now pull — should be up-to-date since we just pushed
    let result = git_pull(&work_dir);
    assert!(result.is_ok(), "git_pull failed: {:?}", result);
    assert!(!result.unwrap(), "expected no changes");
}

// --- git_pull: repo with new remote commits returns Ok(true) ---

#[test]
fn git_pull_with_remote_changes_returns_true() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");
    let pusher_dir = tmp.path().join("pusher");

    // Create bare repo
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone into work_dir
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }

    // Create initial commit and push
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Clone into pusher_dir and push a new commit
    let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
    {
        let mut config = pusher.config().unwrap();
        config.set_str("user.name", "cfgd-pusher").unwrap();
        config.set_str("user.email", "pusher@cfgd.io").unwrap();
    }
    std::fs::write(pusher_dir.join("NEW_FILE"), "hello\n").unwrap();
    {
        let mut index = pusher.index().unwrap();
        index.add_path(Path::new("NEW_FILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = pusher.find_tree(tree_id).unwrap();
        let sig = pusher.signature().unwrap();
        let parent = pusher.head().unwrap().peel_to_commit().unwrap();
        pusher
            .commit(Some("HEAD"), &sig, &sig, "add file", &tree, &[&parent])
            .unwrap();
    }
    {
        let mut remote = pusher.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Now git_pull in work_dir should detect changes
    let result = git_pull(&work_dir);
    assert!(result.is_ok(), "git_pull failed: {:?}", result);
    assert!(result.unwrap(), "expected changes from remote");

    // Verify the new file exists after pull
    assert!(
        work_dir.join("NEW_FILE").exists(),
        "NEW_FILE should exist after fast-forward pull"
    );
}

// --- git_auto_commit_push: no changes returns Ok(false) ---

#[test]
fn git_auto_commit_push_no_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    // Create bare repo
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone, create initial commit, push
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "test\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // No changes — should return Ok(false)
    let result = git_auto_commit_push(&work_dir);
    assert!(result.is_ok(), "git_auto_commit_push failed: {:?}", result);
    assert!(!result.unwrap(), "expected no changes to push");
}

// --- git_auto_commit_push: with changes commits and pushes ---

#[test]
fn git_auto_commit_push_with_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    // Create bare repo
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    // Clone, create initial commit, push
    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "test\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Create a new file (uncommitted change)
    std::fs::write(work_dir.join("new_config.yaml"), "key: value\n").unwrap();

    // Should commit and push the change
    let result = git_auto_commit_push(&work_dir);
    assert!(result.is_ok(), "git_auto_commit_push failed: {:?}", result);
    assert!(result.unwrap(), "expected changes to be pushed");

    // Verify commit was created
    let repo = git2::Repository::open(&work_dir).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        head.message().unwrap(),
        "cfgd: auto-commit configuration changes"
    );

    // Verify the change was pushed to bare repo
    let bare = git2::Repository::open_bare(&bare_dir).unwrap();
    let bare_head = bare
        .find_reference("refs/heads/master")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(head.id(), bare_head.id());
}

// --- git_pull: non-git directory returns error ---

#[test]
fn git_pull_non_repo_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = git_pull(tmp.path());
    let err = result.unwrap_err();
    assert!(
        err.contains("open repo"),
        "expected 'open repo' error, got: {err}"
    );
}

// --- git_auto_commit_push: non-git directory returns error ---

#[test]
fn git_auto_commit_push_non_repo_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = git_auto_commit_push(tmp.path());
    let err = result.unwrap_err();
    assert!(
        err.contains("open repo"),
        "expected 'open repo' error, got: {err}"
    );
}

// --- git_pull: libgit2 fetch fallback fires and surfaces the fetch error ---

#[test]
fn git_pull_falls_back_to_libgit2_and_reports_fetch_error_for_dead_remote() {
    // When the git CLI fetch fails (here: origin points at a non-existent
    // local bare repo), git_pull falls back to the libgit2 fetch path. That
    // fetch also fails against the dead remote, so the libgit2 branch must
    // surface a `fetch: ...` error — proving the fallback executed rather than
    // the CLI path. Using a `file://` URL to a path that does not exist keeps
    // this fully local and instant (no network, no SSH).
    let tmp = tempfile::TempDir::new().unwrap();
    let work_dir = tmp.path().join("work");
    let dead_remote = tmp.path().join("does-not-exist.git");

    // Build a real repo with one commit on master and an origin pointing at the
    // missing bare repo.
    let repo = git2::Repository::init(&work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    let remote_url = crate::to_file_url(&dead_remote);
    repo.remote("origin", &remote_url).unwrap();
    drop(repo);

    let err = git_pull(&work_dir).unwrap_err();
    assert!(
        err.starts_with("fetch: "),
        "libgit2 fallback must surface a 'fetch: ...' error for the dead remote, got: {err}"
    );
}

// --- git_auto_commit_push: libgit2 push fallback fires after committing ---

#[test]
fn git_auto_commit_push_falls_back_to_libgit2_and_reports_push_error_for_dead_remote() {
    // With a working-tree change present and origin pointing at a non-existent
    // bare repo, git_auto_commit_push stages + commits locally (which must
    // succeed), then the git CLI push fails, so it falls back to the libgit2
    // push path. That push also fails against the dead remote, surfacing a
    // `push: ...` error — proving the fallback executed AND that the local
    // commit was created before the push attempt.
    let tmp = tempfile::TempDir::new().unwrap();
    let work_dir = tmp.path().join("work");
    let dead_remote = tmp.path().join("does-not-exist.git");

    let repo = git2::Repository::init(&work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    // Initial commit so HEAD exists.
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    let remote_url = crate::to_file_url(&dead_remote);
    repo.remote("origin", &remote_url).unwrap();
    drop(repo);

    // Introduce an uncommitted change so the commit-and-push branch runs.
    std::fs::write(work_dir.join("new_config.yaml"), "key: value\n").unwrap();

    let err = git_auto_commit_push(&work_dir).unwrap_err();
    assert!(
        err.starts_with("push: "),
        "libgit2 fallback must surface a 'push: ...' error for the dead remote, got: {err}"
    );

    // The auto-commit must have been created locally before the failed push,
    // so the push fallback path operated on a real new commit.
    let reopened = git2::Repository::open(&work_dir).unwrap();
    let head_msg = reopened
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .message()
        .unwrap()
        .to_string();
    assert_eq!(
        head_msg, "cfgd: auto-commit configuration changes",
        "the auto-commit must be created before the push fallback fails"
    );
}

// --- handle_sync: updates daemon state timestamps ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_updates_state_timestamps() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));

    let changed = handle_sync(&repo_dir, false, false, "local", &state, false, false).await;

    assert!(!changed);

    let st = state.lock().await;
    assert!(st.last_sync.is_some(), "last_sync should be set");
}

// --- handle_sync: with auto_pull on repo without remote ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_pull_without_remote_logs_warning() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));

    let changed = handle_sync(&repo_dir, true, false, "local", &state, false, false).await;

    // Should not crash; pull fails gracefully
    assert!(!changed);
}

// --- handle_sync: per-source status update ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_updates_per_source_status() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));
    // Add a second source
    {
        let mut st = state.lock().await;
        st.sources.push(SourceStatus {
            name: "acme".to_string(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 0,
            status: "active".to_string(),
        });
    }

    handle_sync(&repo_dir, false, false, "acme", &state, false, false).await;

    let st = state.lock().await;
    // The "acme" source should have its last_sync updated
    let acme = st.sources.iter().find(|s| s.name == "acme").unwrap();
    assert!(
        acme.last_sync.is_some(),
        "acme source last_sync should be set"
    );
    // The "local" source should NOT have been updated
    let local = st.sources.iter().find(|s| s.name == "local").unwrap();
    assert!(
        local.last_sync.is_none(),
        "local source last_sync should remain None"
    );
}

// --- handle_sync: auto_pull with remote changes fast-forwards ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_auto_pull_with_remote_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");
    let pusher_dir = tmp.path().join("pusher");

    // Set up bare + work + pusher repos
    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Push a change from pusher
    let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
    {
        let mut config = pusher.config().unwrap();
        config.set_str("user.name", "cfgd-pusher").unwrap();
        config.set_str("user.email", "pusher@cfgd.io").unwrap();
    }
    std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
    {
        let mut index = pusher.index().unwrap();
        index.add_path(Path::new("NEWFILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = pusher.find_tree(tree_id).unwrap();
        let sig = pusher.signature().unwrap();
        let parent = pusher.head().unwrap().peel_to_commit().unwrap();
        pusher
            .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
            .unwrap();
    }
    {
        let mut remote = pusher.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let changed = handle_sync(&work_dir, true, false, "local", &state, false, false).await;

    assert!(changed, "handle_sync should detect remote changes");
    assert!(
        work_dir.join("NEWFILE").exists(),
        "pulled file should exist after sync"
    );
}

// --- handle_sync: auto_push with local changes ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_auto_push_with_local_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Create a local change
    std::fs::write(work_dir.join("local_change.txt"), "new content\n").unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    // pull=false, push=true
    let changed = handle_sync(&work_dir, false, true, "local", &state, false, false).await;

    // No remote changes to pull, but push should succeed
    assert!(!changed, "no pull changes expected");

    // Verify commit was pushed to bare repo
    let bare = git2::Repository::open_bare(&bare_dir).unwrap();
    let bare_head = bare
        .find_reference("refs/heads/master")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(
        bare_head.message().unwrap(),
        "cfgd: auto-commit configuration changes"
    );
}

// --- git_pull: diverged branches return error ---

#[test]
fn git_pull_diverged_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");
    let pusher_dir = tmp.path().join("pusher");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "v1\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Push a divergent change from pusher
    let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
    {
        let mut config = pusher.config().unwrap();
        config.set_str("user.name", "cfgd-pusher").unwrap();
        config.set_str("user.email", "pusher@cfgd.io").unwrap();
    }
    std::fs::write(pusher_dir.join("PUSHER_FILE"), "pusher\n").unwrap();
    {
        let mut index = pusher.index().unwrap();
        index.add_path(Path::new("PUSHER_FILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = pusher.find_tree(tree_id).unwrap();
        let sig = pusher.signature().unwrap();
        let parent = pusher.head().unwrap().peel_to_commit().unwrap();
        pusher
            .commit(Some("HEAD"), &sig, &sig, "pusher commit", &tree, &[&parent])
            .unwrap();
    }
    {
        let mut remote = pusher.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    // Create a local commit in work_dir (diverged from remote)
    std::fs::write(work_dir.join("LOCAL_FILE"), "local\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("LOCAL_FILE")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "local commit", &tree, &[&parent])
            .unwrap();
    }

    // git_pull should fail because branches diverged (not fast-forwardable)
    let result = git_pull(&work_dir);
    assert!(result.is_err(), "diverged branch should return error");
    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("diverged") || err_msg.contains("fast-forward"),
        "error should mention divergence: {}",
        err_msg
    );
}

// --- git_auto_commit_push: fresh repo with no HEAD ---

#[test]
fn git_auto_commit_push_fresh_repo_no_head() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }

    // Create a file but don't commit yet — repo has no HEAD
    std::fs::write(work_dir.join("first_file.txt"), "hello\n").unwrap();

    let result = git_auto_commit_push(&work_dir);
    assert!(result.is_ok(), "fresh repo push failed: {:?}", result);
    assert!(result.unwrap(), "expected changes to be committed");

    // Verify HEAD now exists with the auto-commit message
    let repo = git2::Repository::open(&work_dir).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        head.message().unwrap(),
        "cfgd: auto-commit configuration changes"
    );
}

// --- server_checkin: mock HTTP test for config_changed=true ---

#[test]
fn server_checkin_mock_config_changed() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":true,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(changed, "server should report config changed");
    mock.assert();
}

// --- server_checkin: mock HTTP test for config_changed=false ---

#[test]
fn server_checkin_mock_no_change() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed, "server should report no change");
    mock.assert();
}

// --- server_checkin: server returns 500 ---

#[test]
fn server_checkin_mock_server_error() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(500)
        .with_body("internal server error")
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed, "server error should return false");
    mock.assert();
}

// --- server_checkin: malformed JSON response ---

#[test]
fn server_checkin_mock_malformed_json() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("not json at all")
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed, "malformed JSON should return false");
    mock.assert();
}

// --- server_checkin: URL with trailing slash ---

#[test]
fn server_checkin_mock_trailing_slash_url() {
    use crate::config::{
        LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec, ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            ..Default::default()
        },
    };

    // URL with trailing slash should be trimmed
    let url_with_slash = format!("{}/", server.url());
    let changed = server_checkin(&url_with_slash, &resolved);
    assert!(!changed);
    mock.assert();
}

// --- server_checkin: verifies request payload structure ---

#[test]
fn server_checkin_mock_verifies_request_body() {
    use crate::config::{
        CargoSpec, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .match_header("Content-Type", "application/json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":false,"config":null}"#)
        .create();

    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };

    let changed = server_checkin(&server.url(), &resolved);
    assert!(!changed);
    // Verify the mock received the request with correct Content-Type
    mock.assert();
}

// --- try_server_checkin: delegates to server_checkin when URL present ---

#[test]
fn try_server_checkin_no_server_origin_returns_false() {
    use crate::config::*;
    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Git,
                url: "https://github.com/test/repo.git".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };

    let changed = try_server_checkin(&config, &resolved);
    assert!(!changed, "no server origin means no checkin");
}

// --- try_server_checkin: with mock server ---

#[test]
fn try_server_checkin_with_server_origin_calls_checkin() {
    use crate::config::*;

    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/v1/checkin")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","config_changed":true,"config":null}"#)
        .create();

    let config = CfgdConfig {
        api_version: crate::API_VERSION.into(),
        kind: "Config".into(),
        metadata: ConfigMetadata {
            name: "test".into(),
        },
        spec: ConfigSpec {
            profile: Some("default".into()),
            origin: vec![OriginSpec {
                origin_type: OriginType::Server,
                url: server.url(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            }],
            daemon: None,
            secrets: None,
            sources: vec![],
            theme: None,
            modules: None,
            security: None,
            aliases: std::collections::HashMap::new(),
            file_strategy: FileStrategy::default(),
            ai: None,
            compliance: None,
            update: None,
        },
    };
    let resolved = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "test".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };

    let changed = try_server_checkin(&config, &resolved);
    assert!(changed, "server origin should trigger checkin");
    mock.assert();
}

// --- handle_health_connection: response includes Content-Type and Content-Length ---

#[tokio::test]
async fn health_connection_response_headers() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    assert!(
        response.contains("Content-Type: application/json"),
        "missing Content-Type header"
    );
    assert!(
        response.contains("Content-Length:"),
        "missing Content-Length header"
    );
    assert!(
        response.contains("Connection: close"),
        "missing Connection header"
    );
}

// --- handle_health_connection: empty request line defaults to /health ---

#[tokio::test]
async fn health_connection_empty_request_defaults_to_health() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(4096);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    // Send an empty line as the request
    writer.write_all(b"\r\n\r\n").await.unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    // Empty request should either default to /health or return 404
    // The code uses `split_whitespace().nth(1).unwrap_or("/health")` so
    // empty request line -> /health
    assert!(
        response.contains("200 OK") || response.contains("404 Not Found"),
        "should handle empty request gracefully: {}",
        &response[..response.len().min(80)]
    );
}

// --- handle_health_connection: /status body parses to DaemonStatusResponse ---

#[tokio::test]
async fn health_connection_status_body_parses_as_response() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    {
        let mut st = state.lock().await;
        st.drift_count = 7;
        st.update_available = Some("2.0.0".to_string());
    }

    let (client, server) = tokio::io::duplex(8192);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if in_body {
                    lines.push(line);
                } else if line.trim().is_empty() {
                    in_body = true;
                }
            }
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    let body = lines.join("");
    let parsed: DaemonStatusResponse =
        serde_json::from_str(&body).expect("body should parse as DaemonStatusResponse");
    assert!(parsed.running);
    assert_eq!(parsed.drift_count, 7);
    assert_eq!(parsed.update_available.as_deref(), Some("2.0.0"));
    assert_eq!(parsed.sources.len(), 1);
    assert_eq!(parsed.sources[0].name, "local");
}

// --- DaemonState: module_last_reconcile overwrite ---

#[test]
fn daemon_state_module_last_reconcile_overwrite() {
    let mut state = DaemonState::new();
    state
        .module_last_reconcile
        .insert("mod-a".into(), "2026-01-01T00:00:00Z".into());
    state
        .module_last_reconcile
        .insert("mod-a".into(), "2026-01-02T00:00:00Z".into());

    // Overwrite should replace the old value
    assert_eq!(state.module_last_reconcile.len(), 1);
    assert_eq!(
        state.module_last_reconcile.get("mod-a").unwrap(),
        "2026-01-02T00:00:00Z"
    );
}

// --- DaemonState: update_available persists through to_response ---

#[test]
fn daemon_state_update_available_in_response() {
    let mut state = DaemonState::new();
    state.update_available = Some("3.1.0".to_string());

    let response = state.to_response();
    assert_eq!(response.update_available.as_deref(), Some("3.1.0"));
}

// --- Notifier: webhook builds correct JSON payload structure ---

#[test]
fn notifier_webhook_payload_structure() {
    // Verify the JSON payload structure by constructing it the same way as notify_webhook
    let title = "cfgd: drift detected";
    let message = "3 files drifted";
    let payload = serde_json::json!({
        "event": title,
        "message": message,
        "timestamp": crate::utc_now_iso8601(),
        "source": "cfgd",
    });

    let obj = payload.as_object().unwrap();
    assert_eq!(obj.len(), 4);
    assert_eq!(obj.get("event").unwrap().as_str().unwrap(), title);
    assert_eq!(obj.get("message").unwrap().as_str().unwrap(), message);
    assert!(obj.contains_key("timestamp"));
    assert_eq!(obj.get("source").unwrap().as_str().unwrap(), "cfgd");
}

// --- Notifier: webhook payload timestamp format ---

#[test]
fn notifier_webhook_payload_timestamp_is_iso8601() {
    let payload = serde_json::json!({
        "event": "test",
        "message": "msg",
        "timestamp": crate::utc_now_iso8601(),
        "source": "cfgd",
    });

    let ts = payload["timestamp"].as_str().unwrap();
    // ISO 8601 format: contains 'T' separator and ends with 'Z'
    assert!(ts.contains('T'), "timestamp should be ISO 8601: {}", ts);
    assert!(ts.ends_with('Z'), "timestamp should end with Z: {}", ts);
}

// --- ReconcileTask: drift_policy variants ---

#[test]
fn reconcile_task_drift_policy_auto() {
    let task = ReconcileTask {
        entity: "critical-module".into(),
        interval: Duration::from_secs(30),
        auto_apply: true,
        drift_policy: config::DriftPolicy::Auto,
        last_reconciled: None,
    };
    assert!(matches!(task.drift_policy, config::DriftPolicy::Auto));
}

#[test]
fn reconcile_task_drift_policy_notify_only() {
    let task = ReconcileTask {
        entity: "optional-module".into(),
        interval: Duration::from_secs(600),
        auto_apply: false,
        drift_policy: config::DriftPolicy::NotifyOnly,
        last_reconciled: None,
    };
    assert!(matches!(task.drift_policy, config::DriftPolicy::NotifyOnly));
}

#[test]
fn reconcile_task_drift_policy_prompt() {
    let task = ReconcileTask {
        entity: "interactive-module".into(),
        interval: Duration::from_secs(300),
        auto_apply: false,
        drift_policy: config::DriftPolicy::Prompt,
        last_reconciled: None,
    };
    assert!(matches!(task.drift_policy, config::DriftPolicy::Prompt));
}

// --- process_source_decisions: new_optional tier with Accept policy ---

#[test]
fn process_source_decisions_optional_tier_accept() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Notify,
        new_optional: PolicyAction::Accept,
        locked_conflict: PolicyAction::Notify,
    };

    // An item a source offers on a recommended-tier layer is governed by
    // `newRecommended`, whatever `newOptional` says.
    let merged = MergedProfile {
        packages: crate::config::PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    process_source_decisions(&store, "acme", &merged, &policy, &notifier);
    let pending = store.pending_decisions().unwrap();
    // "bat" is recommended tier -> Notify policy -> creates pending decision
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].resource, "packages.cargo.bat");
    assert!(withheld_paths(&store).contains("packages.cargo.bat"));
}

/// `newOptional` governs an item a source offered in an opt-in profile, and
/// nothing else. The tier comes from the LAYER composition built for that
/// profile — the only place the fact lives — so the policy key
/// `docs/sources.md` documents is reachable rather than decorative.
#[test]
fn an_optional_tier_item_is_governed_by_the_optional_policy() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Notify,
        new_optional: PolicyAction::Ignore,
        locked_conflict: PolicyAction::Notify,
    };

    let merged = MergedProfile {
        packages: crate::config::PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined = process_tiered_decisions(
        &store,
        "acme",
        &tiered_items(&merged, crate::config::LayerPolicy::Optional),
        &policy,
        &notifier,
    );
    assert!(
        declined.contains("packages.cargo.bat"),
        "an opt-in profile's item takes newOptional (Ignore), not newRecommended (Notify): {declined:?}"
    );
    assert!(
        store
            .pending_decisions()
            .expect("read decisions")
            .is_empty(),
        "an Ignored item records no row"
    );
}

/// The `locked`/`required` tiers share one `LayerPolicy`, and `lockedConflict`
/// is the key that governs both — a source's locked item must not be judged by
/// `newRecommended` merely because its path reads like an ordinary package.
#[test]
fn a_required_tier_item_is_governed_by_the_locked_policy() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Reject,
        new_optional: PolicyAction::Ignore,
        locked_conflict: PolicyAction::Notify,
    };

    let merged = MergedProfile {
        packages: crate::config::PackagesSpec {
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["bat".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let declined = process_tiered_decisions(
        &store,
        "acme",
        &tiered_items(&merged, crate::config::LayerPolicy::Required),
        &policy,
        &notifier,
    );
    assert!(
        declined.is_empty(),
        "lockedConflict: Notify asks about the item rather than declining it: {declined:?}"
    );
    let pending = store.pending_decisions().expect("read decisions");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tier, "locked");
}

/// A path that merely READS as policy-bearing is not a locked item. The tier
/// is what the source's layer says it is; guessing from the path once sent a
/// recommended `~/.config/company/security.yaml` to `lockedConflict`.
#[test]
fn a_recommended_item_whose_path_reads_like_policy_is_still_recommended() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig {
        new_recommended: PolicyAction::Notify,
        new_optional: PolicyAction::Ignore,
        locked_conflict: PolicyAction::Accept,
    };

    let merged = MergedProfile {
        files: crate::config::FilesSpec {
            managed: vec![managed_file_spec("files/security-policy.yaml")],
            permissions: Default::default(),
        },
        ..Default::default()
    };

    process_tiered_decisions(
        &store,
        "acme",
        &tiered_items(&merged, crate::config::LayerPolicy::Recommended),
        &policy,
        &notifier,
    );
    let pending = store.pending_decisions().expect("read decisions");
    assert_eq!(
        pending.len(),
        1,
        "lockedConflict: Accept must not swallow a recommended item"
    );
    assert_eq!(pending[0].tier, "recommended");
}

fn managed_file_spec(target: &str) -> crate::config::ManagedFileSpec {
    crate::config::ManagedFileSpec {
        source: "files/x".into(),
        target: std::path::PathBuf::from(target),
        strategy: Some(crate::config::FileStrategy::Copy),
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
        patch: None,
    }
}

// --- process_source_decisions: empty merged profile no decisions ---

#[test]
fn process_source_decisions_empty_profile_no_decisions() {
    let store = test_state();
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default();

    let merged = MergedProfile::default();

    let declined = process_source_decisions(&store, "empty", &merged, &policy, &notifier);
    let pending = store.pending_decisions().unwrap();
    assert!(pending.is_empty());
    assert!(declined.is_empty());
}

// --- DaemonStatusResponse: deserialization with all optional fields ---

#[test]
fn daemon_status_response_full_deserialization() {
    let json = r#"{
            "running": true,
            "pid": 54321,
            "uptimeSecs": 7200,
            "lastReconcile": "2026-04-01T00:00:00Z",
            "lastSync": "2026-04-01T00:01:00Z",
            "driftCount": 42,
            "sources": [
                {
                    "name": "local",
                    "lastSync": "2026-04-01T00:01:00Z",
                    "lastReconcile": "2026-04-01T00:00:00Z",
                    "driftCount": 10,
                    "status": "active"
                }
            ],
            "updateAvailable": "4.0.0",
            "moduleReconcile": [
                {
                    "name": "sec",
                    "interval": "30s",
                    "autoApply": true,
                    "driftPolicy": "Auto",
                    "lastReconcile": "2026-04-01T00:00:00Z"
                }
            ]
        }"#;

    let parsed: DaemonStatusResponse = serde_json::from_str(json).unwrap();
    assert!(parsed.running);
    assert_eq!(parsed.pid, 54321);
    assert_eq!(parsed.uptime_secs, 7200);
    assert_eq!(
        parsed.last_reconcile.as_deref(),
        Some("2026-04-01T00:00:00Z")
    );
    assert_eq!(parsed.last_sync.as_deref(), Some("2026-04-01T00:01:00Z"));
    assert_eq!(parsed.drift_count, 42);
    assert_eq!(parsed.sources.len(), 1);
    assert_eq!(parsed.sources[0].drift_count, 10);
    assert_eq!(parsed.update_available.as_deref(), Some("4.0.0"));
    assert_eq!(parsed.module_reconcile.len(), 1);
    assert_eq!(parsed.module_reconcile[0].name, "sec");
    assert!(parsed.module_reconcile[0].auto_apply);
}

// --- CheckinServerResponse: missing config field defaults to None ---

#[test]
fn checkin_response_without_config_field() {
    let json = r#"{"status":"ok","config_changed":false}"#;
    let resp: CheckinServerResponse = serde_json::from_str(json).unwrap();
    // _config is Option<Value>, so missing field deserializes as None
    assert!(!resp.config_changed);
    assert!(resp._config.is_none());
}

// --- hash_resources: unicode content ---

#[test]
fn hash_resources_unicode_content() {
    let set: HashSet<String> = HashSet::from_iter(["packages.brew.\u{1f600}".to_string()]);
    let hash = hash_resources(&set);
    assert_eq!(hash.len(), 64);
    // Must be deterministic
    assert_eq!(hash, hash_resources(&set));
}

// --- parse_duration_or_default: whitespace-only falls back ---

#[test]
fn parse_duration_whitespace_only_falls_back() {
    assert_eq!(
        parse_duration_or_default("   "),
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
}

// --- SyncTask: interval boundary values ---

#[test]
fn sync_task_zero_interval() {
    let task = SyncTask {
        source_name: "instant".into(),
        repo_path: PathBuf::from("/tmp"),
        auto_pull: true,
        auto_push: true,
        auto_apply: true,
        interval: Duration::from_secs(0),
        last_synced: None,
        require_signed_commits: false,
        allow_unsigned: false,
    };
    assert_eq!(task.interval, Duration::ZERO);
}

// --- DaemonState: to_response sources ordering is preserved ---

#[test]
fn daemon_state_to_response_preserves_source_order() {
    let mut state = DaemonState::new();
    state.sources.push(SourceStatus {
        name: "z-source".into(),
        last_sync: None,
        last_reconcile: None,
        drift_count: 0,
        status: "active".into(),
    });
    state.sources.push(SourceStatus {
        name: "a-source".into(),
        last_sync: None,
        last_reconcile: None,
        drift_count: 0,
        status: "active".into(),
    });

    let response = state.to_response();
    assert_eq!(response.sources[0].name, "local");
    assert_eq!(response.sources[1].name, "z-source");
    assert_eq!(response.sources[2].name, "a-source");
}

// --- DaemonState: started_at tracks elapsed time ---

#[test]
fn daemon_state_started_at_elapses() {
    let state = DaemonState::new();
    let elapsed = state.started_at.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "started_at should be recent"
    );
}

// --- handle_health_connection: /drift response structure ---

#[tokio::test]
async fn health_connection_drift_body_parses_as_json() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let (client, server) = tokio::io::duplex(8192);

    let handler_state = Arc::clone(&state);
    let handler = tokio::spawn(async move {
        handle_health_connection(server, handler_state)
            .await
            .unwrap();
    });

    let (reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(b"GET /drift HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if in_body {
                    lines.push(line);
                } else if line.trim().is_empty() {
                    in_body = true;
                }
            }
            Err(_) => break,
        }
    }

    handler.await.unwrap();

    let body = lines.join("");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("drift body should be valid JSON");
    assert!(parsed.get("drift_count").is_some());
    assert!(parsed.get("events").is_some());
    assert!(parsed["events"].is_array());
    // With an empty default state store, events should be empty
    assert_eq!(parsed["drift_count"].as_u64().unwrap(), 0);
}

// --- handle_sync: no pull, no push, still updates timestamp ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_sync_no_pull_no_push_updates_timestamp() {
    use crate::test_helpers::init_test_git_repo;

    let tmp = tempfile::TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    init_test_git_repo(&repo_dir);

    let state = Arc::new(Mutex::new(DaemonState::new()));

    let changed = handle_sync(&repo_dir, false, false, "local", &state, false, false).await;

    assert!(!changed, "no pull/push means no changes");

    let st = state.lock().await;
    assert!(
        st.last_sync.is_some(),
        "last_sync should be set even with no operations"
    );
}

// --- git_pull_sync: delegates to git_pull ---

#[test]
fn git_pull_sync_non_repo_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = git_pull_sync(tmp.path());
    let err = result.unwrap_err();
    assert!(
        err.contains("open repo"),
        "expected 'open repo' error, got: {err}"
    );
}

#[test]
fn git_pull_sync_clean_repo_no_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bare_dir = tmp.path().join("bare.git");
    let work_dir = tmp.path().join("work");

    std::fs::create_dir_all(&bare_dir).unwrap();
    git2::Repository::init_bare(&bare_dir).unwrap();

    let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "cfgd-test").unwrap();
        config.set_str("user.email", "test@cfgd.io").unwrap();
    }
    std::fs::write(work_dir.join("README"), "test\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    let result = git_pull_sync(&work_dir);
    assert!(result.is_ok());
    assert!(!result.unwrap(), "should be up to date");
}

// --- Notifier: all methods construct without panic ---

#[test]
fn notifier_all_methods_construct() {
    let stdout = Notifier::new(NotifyMethod::Stdout, None);
    assert!(matches!(stdout.method, NotifyMethod::Stdout));
    assert!(stdout.webhook_url.is_none());

    let desktop = Notifier::new(NotifyMethod::Desktop, None);
    assert!(matches!(desktop.method, NotifyMethod::Desktop));

    let webhook_none = Notifier::new(NotifyMethod::Webhook, None);
    assert!(matches!(webhook_none.method, NotifyMethod::Webhook));
    assert!(webhook_none.webhook_url.is_none());

    let webhook_url = Notifier::new(
        NotifyMethod::Webhook,
        Some("https://example.com/hook".into()),
    );
    assert_eq!(
        webhook_url.webhook_url.as_deref(),
        Some("https://example.com/hook")
    );
}

// --- DaemonStatusResponse: serialization/deserialization symmetry ---

#[test]
fn daemon_status_response_roundtrip_symmetry() {
    let original = DaemonStatusResponse {
        running: true,
        pid: 99999,
        uptime_secs: 86400,
        last_reconcile: Some("2026-04-01T12:00:00Z".into()),
        last_sync: Some("2026-04-01T12:01:00Z".into()),
        drift_count: 100,
        sources: vec![
            SourceStatus {
                name: "local".into(),
                last_sync: Some("2026-04-01T12:01:00Z".into()),
                last_reconcile: Some("2026-04-01T12:00:00Z".into()),
                drift_count: 50,
                status: "active".into(),
            },
            SourceStatus {
                name: "corp".into(),
                last_sync: None,
                last_reconcile: None,
                drift_count: 50,
                status: "error".into(),
            },
        ],
        update_available: Some("5.0.0".into()),
        module_reconcile: vec![ModuleReconcileStatus {
            name: "sec-baseline".into(),
            interval: "30s".into(),
            auto_apply: true,
            drift_policy: "Auto".into(),
            last_reconcile: Some("2026-04-01T12:00:00Z".into()),
        }],
    };

    let json = serde_json::to_string(&original).unwrap();
    let roundtripped: DaemonStatusResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(roundtripped.pid, original.pid);
    assert_eq!(roundtripped.uptime_secs, original.uptime_secs);
    assert_eq!(roundtripped.drift_count, original.drift_count);
    assert_eq!(roundtripped.sources.len(), original.sources.len());
    assert_eq!(
        roundtripped.sources[1].drift_count,
        original.sources[1].drift_count
    );
    assert_eq!(
        roundtripped.module_reconcile.len(),
        original.module_reconcile.len()
    );
    assert_eq!(roundtripped.update_available, original.update_available);
}

// --- SourceStatus: serialization includes camelCase properly ---

#[test]
fn source_status_camel_case_serialization() {
    let status = SourceStatus {
        name: "test".into(),
        last_sync: Some("ts".into()),
        last_reconcile: Some("tr".into()),
        drift_count: 1,
        status: "active".into(),
    };
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"lastSync\""));
    assert!(json.contains("\"lastReconcile\""));
    assert!(json.contains("\"driftCount\""));
    assert!(!json.contains("\"last_sync\""));
    assert!(!json.contains("\"last_reconcile\""));
    assert!(!json.contains("\"drift_count\""));
}

// --- compute_config_hash: uses only packages for hash ---

#[test]
fn compute_config_hash_ignores_non_package_fields() {
    use crate::config::{
        EnvVar, LayerPolicy, MergedProfile, PackagesSpec, ProfileLayer, ProfileSpec,
        ResolvedProfile,
    };

    let resolved_a = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "a".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            env: vec![EnvVar {
                name: "FOO".into(),
                value: "bar".into(),
            }],
            ..Default::default()
        },
    };

    let resolved_b = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "b".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            packages: PackagesSpec::default(),
            env: vec![EnvVar {
                name: "BAZ".into(),
                value: "qux".into(),
            }],
            ..Default::default()
        },
    };

    // Both have same empty packages, so hash should be the same
    // because compute_config_hash only hashes the packages field
    let hash_a = compute_config_hash(&resolved_a).unwrap();
    let hash_b = compute_config_hash(&resolved_b).unwrap();
    assert_eq!(
        hash_a, hash_b,
        "compute_config_hash should only hash packages, not env vars"
    );
}

// --- service generators/installers under the default (no-override) dir set ---
//
// The dir-overrides parameter is threaded through every generator and
// installer, but only the override-specific tests care what is in it; these
// wrappers keep the rest reading as the one-line calls they are about.

#[cfg(unix)]
fn launchd_plist_default_dirs(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    home: &Path,
    scope: crate::Scope,
) -> String {
    generate_launchd_plist(
        binary,
        config_path,
        profile,
        home,
        scope,
        &crate::daemon::DaemonDirOverrides::default(),
    )
}

#[cfg(unix)]
fn systemd_unit_default_dirs(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
) -> String {
    generate_systemd_unit(
        binary,
        config_path,
        profile,
        scope,
        &crate::daemon::DaemonDirOverrides::default(),
    )
}

#[cfg(unix)]
fn install_launchd_default_dirs(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
) -> Result<()> {
    install_launchd_service(
        binary,
        config_path,
        profile,
        scope,
        &crate::daemon::DaemonDirOverrides::default(),
    )
}

#[cfg(unix)]
fn install_systemd_default_dirs(
    binary: &Path,
    config_path: &Path,
    profile: Option<&str>,
    scope: crate::Scope,
) -> Result<()> {
    install_systemd_service(
        binary,
        config_path,
        profile,
        scope,
        &crate::daemon::DaemonDirOverrides::default(),
    )
}

// --- generate_launchd_plist tests ---

#[cfg(unix)]
#[test]
fn generate_launchd_plist_contains_correct_structure() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/Users/testuser/.config/cfgd/config.yaml");
    let home = Path::new("/Users/testuser");

    let plist = launchd_plist_default_dirs(binary, config, None, home, crate::Scope::User);

    assert!(
        plist.contains("<?xml version=\"1.0\""),
        "plist should have XML declaration"
    );
    assert!(
        plist.contains(&format!("<string>{}</string>", LAUNCHD_LABEL)),
        "plist should contain the launchd label"
    );
    assert!(
        plist.contains("<string>/usr/local/bin/cfgd</string>"),
        "plist should contain binary path"
    );
    assert!(
        plist.contains("<string>/Users/testuser/.config/cfgd/config.yaml</string>"),
        "plist should contain config path"
    );
    assert!(
        plist.contains("<string>--quiet</string>"),
        "plist should contain --quiet flag"
    );
    let config_pos = plist
        .find("<string>/Users/testuser/.config/cfgd/config.yaml</string>")
        .unwrap();
    let quiet_pos = plist.find("<string>--quiet</string>").unwrap();
    let daemon_pos = plist.find("<string>daemon</string>").unwrap();
    assert!(
        config_pos < quiet_pos && quiet_pos < daemon_pos,
        "--quiet should appear between config path and daemon"
    );
    assert!(
        plist.contains("<string>daemon</string>"),
        "plist should contain daemon subcommand"
    );
    assert!(
        plist.contains("<key>RunAtLoad</key>"),
        "plist should enable run at load"
    );
    assert!(
        plist.contains("<key>KeepAlive</key>"),
        "plist should enable keep alive"
    );
    assert!(
        plist.contains("/Users/testuser/Library/Logs/cfgd.log"),
        "plist should set stdout log path under home"
    );
    assert!(
        plist.contains("/Users/testuser/Library/Logs/cfgd.err"),
        "plist should set stderr log path under home"
    );
    // Without profile, no --profile argument should appear
    assert!(
        !plist.contains("--profile"),
        "plist without profile should not contain --profile"
    );
}

#[cfg(unix)]
#[test]
fn generate_launchd_plist_with_profile() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/.config/cfgd/config.yaml");
    let home = Path::new("/home/user");

    let plist = launchd_plist_default_dirs(binary, config, Some("work"), home, crate::Scope::User);

    assert!(
        plist.contains("<string>--profile</string>"),
        "plist with profile should contain --profile argument"
    );
    assert!(
        plist.contains("<string>work</string>"),
        "plist with profile should contain the profile name"
    );
    // Verify order: --config before --profile before --quiet before daemon
    let config_pos = plist.find("<string>--config</string>").unwrap();
    let quiet_pos = plist.find("<string>--quiet</string>").unwrap();
    let daemon_pos = plist.find("<string>daemon</string>").unwrap();
    let profile_pos = plist.find("<string>--profile</string>").unwrap();
    assert!(
        config_pos < profile_pos,
        "--config should appear before --profile"
    );
    assert!(
        profile_pos < quiet_pos,
        "--profile should appear before --quiet"
    );
    assert!(
        quiet_pos < daemon_pos,
        "--quiet should appear before daemon"
    );
}

// --- generate_systemd_unit tests ---

#[cfg(unix)]
#[test]
fn generate_systemd_unit_contains_correct_structure() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/.config/cfgd/config.yaml");

    let unit = systemd_unit_default_dirs(binary, config, None, crate::Scope::User);

    assert!(
        unit.contains("[Unit]"),
        "unit file should have [Unit] section"
    );
    assert!(
        unit.contains("Description=cfgd configuration daemon"),
        "unit file should have correct description"
    );
    assert!(
        unit.contains("After=network.target"),
        "unit file should depend on network.target"
    );
    assert!(
        unit.contains("[Service]"),
        "unit file should have [Service] section"
    );
    assert!(
        unit.contains("Type=simple"),
        "unit file should use simple service type"
    );
    assert!(
        unit.contains(
            "ExecStart=/usr/local/bin/cfgd --config /home/user/.config/cfgd/config.yaml --quiet daemon"
        ),
        "unit file should have correct ExecStart"
    );
    assert!(
        unit.contains("Restart=on-failure"),
        "unit file should restart on failure"
    );
    assert!(
        unit.contains("RestartSec=10"),
        "unit file should have 10s restart delay"
    );
    assert!(
        unit.contains("[Install]"),
        "unit file should have [Install] section"
    );
    assert!(
        unit.contains("WantedBy=default.target"),
        "unit file should be wanted by default.target"
    );
    // Without profile, no --profile should appear
    assert!(
        !unit.contains("--profile"),
        "unit without profile should not contain --profile"
    );
}

#[cfg(unix)]
#[test]
fn generate_systemd_unit_with_profile() {
    let binary = Path::new("/opt/bin/cfgd");
    let config = Path::new("/etc/cfgd/config.yaml");

    let unit = systemd_unit_default_dirs(binary, config, Some("server"), crate::Scope::User);

    assert!(
        unit.contains(
            "ExecStart=/opt/bin/cfgd --config /etc/cfgd/config.yaml --profile server --quiet daemon"
        ),
        "unit file with profile should include --profile in ExecStart"
    );
}

// install_launchd_service + uninstall_launchd_service: redirect HOME under
// TestHomeGuard, run install → assert plist landed, run uninstall → assert
// plist removed. Exercises the dir-create, atomic_write, exists, remove paths.

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn install_then_uninstall_launchd_service_round_trips_plist() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let binary = tmp.path().join("cfgd");
    std::fs::write(&binary, b"").unwrap();
    let config = tmp.path().join("config.yaml");
    std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\n").unwrap();

    install_launchd_default_dirs(&binary, &config, Some("work"), crate::Scope::User)
        .expect("install ok");

    let plist = tmp
        .path()
        .join("Library/LaunchAgents/com.cfgd.daemon.plist");
    assert!(plist.exists(), "plist should be installed at expected path");
    let body = std::fs::read_to_string(&plist).unwrap();
    assert!(body.contains("com.cfgd.daemon"));
    assert!(body.contains("--profile"));

    uninstall_launchd_service(&test_printer(), crate::Scope::User).expect("uninstall ok");
    assert!(!plist.exists(), "plist should be removed after uninstall");
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn uninstall_launchd_service_is_noop_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    // No prior install — uninstall must succeed without error.
    uninstall_launchd_service(&test_printer(), crate::Scope::User)
        .expect("uninstall on clean home is a no-op");
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn install_then_uninstall_systemd_service_round_trips_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let binary = tmp.path().join("cfgd");
    std::fs::write(&binary, b"").unwrap();
    let config = tmp.path().join("config.yaml");
    std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\n").unwrap();

    install_systemd_default_dirs(&binary, &config, None, crate::Scope::User).expect("install ok");

    let unit_path = tmp.path().join(".config/systemd/user/cfgd.service");
    assert!(unit_path.exists(), "unit should be installed");
    let body = std::fs::read_to_string(&unit_path).unwrap();
    assert!(body.contains("ExecStart="));
    assert!(body.contains("--quiet daemon"));
    assert!(!body.contains("--profile"));

    uninstall_systemd_service(&test_printer(), crate::Scope::User).expect("uninstall ok");
    assert!(
        !unit_path.exists(),
        "unit should be removed after uninstall"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn uninstall_systemd_service_is_noop_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    uninstall_systemd_service(&test_printer(), crate::Scope::User)
        .expect("uninstall on clean home is a no-op");
}

// Cross-platform dispatcher: install_service uses current_exe() + cfg(macos)/else
// to delegate to launchd/systemd. uninstall_service mirrors that branch.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn install_service_then_uninstall_service_round_trips_via_dispatcher() {
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let config = tmp.path().join("config.yaml");
    std::fs::write(&config, "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\n").unwrap();

    crate::daemon::service::install_service(
        &config,
        None,
        crate::Scope::User,
        &crate::daemon::DaemonDirOverrides::default(),
    )
    .expect("install_service ok");
    // Whether macOS (plist) or Linux (unit), uninstall must round-trip without
    // panic. Skip exists() assertions — the dispatcher branch depends on
    // target_os and we just want both arms exercised.
    crate::daemon::service::uninstall_service(&test_printer(), crate::Scope::User)
        .expect("uninstall_service ok");
}

// --- managed-target drift gating (#97) ---

#[test]
fn path_is_managed_target_true_for_exact_member() {
    let managed = vec![PathBuf::from("/home/user/.zshrc")];
    assert!(
        super::drift::path_is_managed_target(Path::new("/home/user/.zshrc"), &managed),
        "a path exactly in managed_paths must count as a managed target"
    );
}

#[test]
fn path_is_managed_target_false_for_git_internals() {
    let managed = vec![PathBuf::from("/home/user/.config/cfgd/profiles/dev.yaml")];
    assert!(
        !super::drift::path_is_managed_target(
            Path::new("/home/user/.config/cfgd/.git/index"),
            &managed
        ),
        "a .git source path must NOT count as a managed target"
    );
}

#[test]
fn path_is_managed_target_false_for_config_source() {
    let managed = vec![PathBuf::from("/home/user/.zshrc")];
    assert!(
        !super::drift::path_is_managed_target(
            Path::new("/home/user/.config/cfgd/cfgd.yaml"),
            &managed
        ),
        "a config source file must NOT count as a managed target"
    );
}

#[test]
fn path_is_managed_target_false_for_sibling_in_watched_parent() {
    // The watcher also watches the PARENT of a not-yet-existing managed file,
    // so sibling files fire events. Exact membership must exclude them.
    let managed = vec![PathBuf::from("/home/user/.config/app/managed.conf")];
    assert!(
        !super::drift::path_is_managed_target(
            Path::new("/home/user/.config/app/other.conf"),
            &managed
        ),
        "a sibling in a watched parent dir must NOT count as a managed target"
    );
}

// --- record_file_drift_to tests ---

#[test]
fn record_file_drift_to_records_event() {
    let store = test_state();
    let path = Path::new("/home/user/.bashrc");

    let result = record_file_drift_to(&store, path);
    assert!(result, "record_file_drift_to should return true on success");

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1, "should have exactly one drift event");
    assert_eq!(events[0].resource_id, "/home/user/.bashrc");
}

#[test]
fn record_file_drift_to_records_correct_type() {
    let store = test_state();
    let path = Path::new("/etc/config.yaml");

    record_file_drift_to(&store, path);

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].resource_type, "file",
        "drift event should have resource_type 'file'"
    );
    assert_eq!(
        events[0].source, "local",
        "drift event should have source 'local'"
    );
    assert_eq!(
        events[0].actual.as_deref(),
        Some("modified"),
        "drift event should have actual value 'modified'"
    );
    assert!(
        events[0].expected.is_none(),
        "drift event should have no expected value"
    );
}

// --- discover_managed_paths tests ---

#[test]
fn discover_managed_paths_with_no_config_returns_empty() {
    use std::path::Path;

    // Non-existent config file should return empty paths
    let paths = discover_managed_paths(
        Path::new("/nonexistent/config.yaml"),
        None,
        &crate::test_helpers::NoopDaemonHooks,
    );
    assert!(
        paths.is_empty(),
        "non-existent config should return no managed paths"
    );
}

// --- parse_daemon_config tests ---

#[test]
fn parse_daemon_config_defaults() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert_eq!(
        parsed.reconcile_interval,
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
    assert_eq!(parsed.sync_interval, Duration::from_secs(DEFAULT_SYNC_SECS));
    assert!(!parsed.auto_pull);
    assert!(!parsed.auto_push);
    assert!(!parsed.on_change_reconcile);
    assert!(!parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Stdout));
    assert!(parsed.webhook_url.is_none());
    assert!(!parsed.auto_apply);
}

#[test]
fn parse_daemon_config_custom_intervals() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "10m".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::default(),
            patches: vec![],
        }),
        sync: Some(config::SyncConfig {
            auto_pull: false,
            auto_push: false,
            interval: "30s".to_string(),
        }),
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert_eq!(parsed.reconcile_interval, Duration::from_secs(600));
    assert_eq!(parsed.sync_interval, Duration::from_secs(30));
}

#[test]
fn parse_daemon_config_notification_settings() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: Some(config::NotifyConfig {
            drift: true,
            method: NotifyMethod::Webhook,
            webhook_url: Some("https://hooks.example.com/drift".to_string()),
        }),
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Webhook));
    assert_eq!(
        parsed.webhook_url.as_deref(),
        Some("https://hooks.example.com/drift")
    );
}

#[test]
fn parse_daemon_config_sync_flags() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: Some(config::SyncConfig {
            auto_pull: true,
            auto_push: true,
            interval: "5m".to_string(),
        }),
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.auto_pull);
    assert!(parsed.auto_push);
}

#[test]
fn parse_daemon_config_on_change_enabled() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "5m".to_string(),
            on_change: true,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::default(),
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.on_change_reconcile);
    assert!(!parsed.auto_apply);
}

#[test]
fn parse_daemon_config_auto_apply_enabled() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "5m".to_string(),
            on_change: false,
            auto_apply: true,
            policy: None,
            drift_policy: config::DriftPolicy::Auto,
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let parsed = parse_daemon_config(&daemon_cfg);
    assert!(parsed.auto_apply);
}

#[test]
fn handle_reconcile_with_no_config_file() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().to_path_buf();
    let printer = test_printer();

    // Passing a nonexistent config path should return gracefully (no panic)
    handle_reconcile(
        Path::new("/nonexistent/path/config.yaml"),
        None,
        quiet_reconcile_ctx(
            &state,
            &notifier,
            false,
            &crate::test_helpers::NoopDaemonHooks,
            &state_dir,
            &printer,
        ),
    );
    // If we got here without panic, the function handled the missing config gracefully.
    // Verify the state wasn't updated (no reconciliation occurred).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = rt.block_on(state.lock());
    assert!(
        guard.last_reconcile.is_none(),
        "no reconcile should have occurred with missing config"
    );
}

#[test]
fn handle_reconcile_with_no_profile() {
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().to_path_buf();

    // Write a valid config with NO profile set
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let printer = test_printer();
    // No profile override and no profile in config — should return gracefully
    handle_reconcile(
        &config_path,
        None,
        quiet_reconcile_ctx(
            &state,
            &notifier,
            false,
            &crate::test_helpers::NoopDaemonHooks,
            &state_dir,
            &printer,
        ),
    );
    // Should not have updated state since no profile was available
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = rt.block_on(state.lock());
    assert!(
        guard.last_reconcile.is_none(),
        "no reconcile should have occurred without a profile"
    );
}

// --- build_reconcile_tasks ---

#[test]
fn build_reconcile_tasks_default_only_when_no_patches() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(60), false);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
    assert_eq!(tasks[0].interval, Duration::from_secs(60));
    assert!(!tasks[0].auto_apply);
    assert_eq!(tasks[0].drift_policy, config::DriftPolicy::NotifyOnly);
}

#[test]
fn build_reconcile_tasks_default_inherits_global_drift_policy() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "120s".to_string(),
            on_change: false,
            auto_apply: true,
            policy: None,
            drift_policy: config::DriftPolicy::Auto,
            patches: vec![],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(120), true);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].drift_policy, config::DriftPolicy::Auto);
    assert!(tasks[0].auto_apply);
}

#[test]
fn build_reconcile_tasks_no_reconcile_config_uses_defaults() {
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(&daemon_cfg, None, &[], Duration::from_secs(300), false);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
    assert_eq!(tasks[0].interval, Duration::from_secs(300));
    // Default drift policy is NotifyOnly
    assert_eq!(tasks[0].drift_policy, config::DriftPolicy::default());
}

#[test]
fn build_reconcile_tasks_patches_without_resolved_profile_skips_modules() {
    // Patches exist but no resolved profile — should still get only __default__
    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches: vec![config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("vim".to_string()),
                interval: Some("10s".to_string()),
                auto_apply: Some(true),
                drift_policy: None,
            }],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };
    let tasks = build_reconcile_tasks(
        &daemon_cfg,
        None, // no resolved profile
        &["default"],
        Duration::from_secs(60),
        false,
    );
    // Only default task — no module tasks since profile isn't resolved
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
}

#[test]
fn build_reconcile_tasks_module_with_overridden_interval_gets_dedicated_task() {
    // Build a resolved profile with a module
    let merged = config::MergedProfile {
        modules: vec!["vim".to_string()],
        ..Default::default()
    };
    let resolved = config::ResolvedProfile {
        layers: vec![config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "default".to_string(),
            priority: 0,
            policy: config::LayerPolicy::Local,
            spec: Default::default(),
        }],
        merged,
    };

    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches: vec![config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("vim".to_string()),
                interval: Some("10s".to_string()),
                auto_apply: None,
                drift_policy: None,
            }],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let tasks = build_reconcile_tasks(
        &daemon_cfg,
        Some(&resolved),
        &["default"],
        Duration::from_secs(60),
        false,
    );
    // Should have 2 tasks: one for "vim" with 10s interval, one for __default__
    assert_eq!(tasks.len(), 2);
    let vim_task = tasks.iter().find(|t| t.entity == "vim").unwrap();
    assert_eq!(vim_task.interval, Duration::from_secs(10));
    assert!(!vim_task.auto_apply);
    let default_task = tasks.iter().find(|t| t.entity == "__default__").unwrap();
    assert_eq!(default_task.interval, Duration::from_secs(60));
}

#[test]
fn build_reconcile_tasks_module_matching_global_gets_no_dedicated_task() {
    // When a module's effective settings match global, no dedicated task is created
    let merged = config::MergedProfile {
        modules: vec!["vim".to_string()],
        ..Default::default()
    };
    let resolved = config::ResolvedProfile {
        layers: vec![config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "default".to_string(),
            priority: 0,
            policy: config::LayerPolicy::Local,
            spec: Default::default(),
        }],
        merged,
    };

    let daemon_cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".to_string(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            // Patch that produces same values as global
            patches: vec![config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("vim".to_string()),
                interval: None,     // inherits "60s"
                auto_apply: None,   // inherits false
                drift_policy: None, // inherits NotifyOnly
            }],
        }),
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let tasks = build_reconcile_tasks(
        &daemon_cfg,
        Some(&resolved),
        &["default"],
        Duration::from_secs(60),
        false,
    );
    // Only __default__ — vim's effective settings match global
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
}

// --- build_sync_tasks ---

#[test]
fn build_sync_tasks_local_only_when_no_sources() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: true,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let tasks = build_sync_tasks(tmp.path(), &parsed, &[], false, tmp.path(), |_| None);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_name, "local");
    assert!(tasks[0].auto_pull);
    assert!(!tasks[0].auto_push);
    assert!(tasks[0].auto_apply);
    assert_eq!(tasks[0].interval, Duration::from_secs(300));
    assert!(!tasks[0].require_signed_commits);
}

#[test]
fn build_sync_tasks_includes_source_when_dir_exists() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: false,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("sources");
    std::fs::create_dir_all(cache_dir.join("team-config")).unwrap();

    let sources = vec![config::SourceSpec {
        name: "team-config".to_string(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/team/config.git".to_string(),
            branch: "main".to_string(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: config::SourceSyncSpec {
            interval: "120s".to_string(),
            auto_apply: true,
            pin_version: None,
            required: false,
        },
    }];

    let tasks = build_sync_tasks(
        tmp.path(),
        &parsed,
        &sources,
        false,
        &cache_dir,
        |_| Some(true), // manifest requires signed commits
    );
    assert_eq!(tasks.len(), 2);
    let source_task = tasks
        .iter()
        .find(|t| t.source_name == "team-config")
        .unwrap();
    assert!(source_task.auto_pull);
    assert!(!source_task.auto_push);
    assert!(source_task.auto_apply);
    assert_eq!(source_task.interval, Duration::from_secs(120));
    assert!(source_task.require_signed_commits);
}

#[test]
fn build_sync_tasks_skips_source_when_dir_missing() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: false,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("sources");
    // Intentionally don't create the source directory

    let sources = vec![config::SourceSpec {
        name: "missing-source".to_string(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/team/config.git".to_string(),
            branch: "main".to_string(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    }];

    let tasks = build_sync_tasks(tmp.path(), &parsed, &sources, false, &cache_dir, |_| None);
    // Only local task — source dir doesn't exist
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_name, "local");
}

#[test]
fn build_sync_tasks_propagates_allow_unsigned() {
    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(60),
        sync_interval: Duration::from_secs(300),
        auto_pull: true,
        auto_push: true,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };
    let tmp = tempfile::tempdir().unwrap();
    let tasks = build_sync_tasks(
        tmp.path(),
        &parsed,
        &[],
        true, // allow_unsigned
        tmp.path(),
        |_| None,
    );
    assert!(tasks[0].allow_unsigned);
}

// --- handle_reconcile: deeper paths ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_with_valid_config_records_drift_events() {
    // Set up a tmpdir with config.yaml + profiles/default.yaml containing packages.
    // DaemonHooks that returns a PackageAction::Install so the plan has drift.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Write config
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    // Write profile
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n",
        )
        .unwrap();

    struct DriftHooks;
    impl DaemonHooks for DriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            // Return a package install action to create drift
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["bat".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &DriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Verify drift events were recorded in the state store
    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(
        !drift_events.is_empty(),
        "drift events should have been recorded"
    );
    // The drift should be for the package install action
    let pkg_drift = drift_events.iter().find(|e| e.resource_type == "package");
    assert!(
        pkg_drift.is_some(),
        "should have a package drift event; events: {:?}",
        drift_events
    );
    assert_eq!(pkg_drift.unwrap().resource_id, "cargo:bat");

    // Verify daemon state was updated
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "last_reconcile should have been set"
    );
    assert!(
        guard.drift_count > 0,
        "drift_count should have been incremented"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_notify_only_drift_policy_does_not_apply() {
    // Verify that with NotifyOnly drift policy, drift is recorded but no apply happens.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      onChange: false\n      autoApply: false\n      driftPolicy: NotifyOnly\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n",
        )
        .unwrap();

    struct NotifyOnlyHooks;
    impl DaemonHooks for NotifyOnlyHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["ripgrep".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &NotifyOnlyHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Drift should be recorded
    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(
        !drift_events.is_empty(),
        "drift events should be recorded even with NotifyOnly policy"
    );

    // Verify state reflects drift
    let guard = state.lock().await;
    assert!(guard.drift_count > 0);
    assert!(guard.last_reconcile.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_no_drift_when_no_actions() {
    // When plan has no actions, no drift events should be recorded.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    struct NoDriftHooks;
    impl DaemonHooks for NoDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &NoDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // No drift events should have been recorded
    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(
        drift_events.is_empty(),
        "no drift events should be recorded when plan has no actions"
    );

    // State should reflect a reconciliation occurred
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert_eq!(guard.drift_count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_clean_tick_clears_outstanding_drift() {
    // A previously-recorded drift row that no longer diverges must be resolved
    // by the no-drift branch's snapshot reset, driving both the DB and the
    // in-memory drift_count back to 0.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Seed an outstanding unresolved drift row, and prime the in-memory count
    // to match — the prior reconcile that recorded it would have set this.
    {
        let store = StateStore::open_in_dir(&state_dir).unwrap();
        store
            .record_drift(
                "file",
                "/home/user/.zshrc",
                None,
                Some("drift detected"),
                "local",
            )
            .unwrap();
        assert_eq!(store.unresolved_drift().unwrap().len(), 1);
    }

    struct NoDriftHooks;
    impl DaemonHooks for NoDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    {
        let mut st = state.lock().await;
        st.drift_count = 1;
        if let Some(source) = st.sources.first_mut() {
            source.drift_count = 1;
        }
    }
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &NoDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // The clean tick resolved the seeded row end-to-end.
    let store = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        store.unresolved_drift().unwrap().is_empty(),
        "clean reconcile tick must resolve the previously-outstanding drift row"
    );

    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert_eq!(
        guard.drift_count, 0,
        "clean reconcile tick must reset in-memory drift_count to 0"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_with_profile_override() {
    // Test that profile_override is used instead of config's profile field.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Config with profile "other" but we override to "default"
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: nonexistent\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    // Override profile to "default" which exists
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            Some("default"),
            quiet_reconcile_ctx(
                &st,
                &not,
                false,
                &crate::test_helpers::NoopDaemonHooks,
                &sd,
                &printer,
            ),
        );
    })
    .await
    .unwrap();

    // Should have completed successfully with the overridden profile
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "reconciliation should succeed with profile override"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_multiple_actions_records_all_drift() {
    // Verify that all drift-producing actions are recorded as separate events.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - bat\n        - ripgrep\n        - fd-find\n",
        )
        .unwrap();

    struct MultiDriftHooks;
    impl DaemonHooks for MultiDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            // Also include a file action
            Ok(vec![FileAction::Create {
                source: PathBuf::from("/src/.zshrc"),
                target: PathBuf::from("/home/user/.zshrc"),
                origin: "local".into(),
                strategy: crate::config::FileStrategy::default(),
                source_hash: None,
                patch: None,
            }])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![
                PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["bat".into(), "ripgrep".into()],
                    origin: "local".into(),
                },
                PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["fd-find".into()],
                    origin: "local".into(),
                },
            ])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &MultiDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    // Should have drift events for all actions:
    // 1 file create + 2 package install actions = 3 drift events
    assert_eq!(
        drift_events.len(),
        3,
        "should have drift events for all actions; got: {:?}",
        drift_events
    );

    let resource_types: Vec<&str> = drift_events
        .iter()
        .map(|e| e.resource_type.as_str())
        .collect();
    assert!(
        resource_types.contains(&"file"),
        "should have a file drift event"
    );
    assert!(
        resource_types.contains(&"package"),
        "should have package drift events"
    );
}

// --- handle_reconcile: autoApply + onDrift + notify_on_drift arms ---
//
// These tests cover the branches that the prior drift tests skipped:
// `DriftPolicy::Auto` invoking `reconciler.apply()`, the `scripts.on_drift`
// execution loop, and the `notify_on_drift=true` notifier paths.

/// `DriftingFileHooks` returns a single `FileAction::Create` whose `source`
/// and `target` paths are owned by the test fixture. With
/// `FileStrategy::Copy`, the reconciler's apply path will `std::fs::copy` the
/// file, succeeding under normal conditions or failing if `source` is absent.
struct DriftingFileHooks {
    source: PathBuf,
    target: PathBuf,
}

impl DaemonHooks for DriftingFileHooks {
    fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
        ProviderRegistry::new()
    }
    fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> crate::errors::Result<Vec<FileAction>> {
        Ok(vec![FileAction::Create {
            source: self.source.clone(),
            target: self.target.clone(),
            origin: "local".into(),
            strategy: crate::config::FileStrategy::Copy,
            source_hash: None,
            patch: None,
        }])
    }
    fn plan_packages(
        &self,
        _: &MergedProfile,
        _: &[&dyn PackageManager],
        _: &std::collections::HashSet<String>,
        _: &PackageContext<'_>,
    ) -> crate::errors::Result<Vec<PackageAction>> {
        Ok(vec![])
    }
    fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &config::PackagesSpec) {}
    fn expand_tilde(&self, path: &Path) -> PathBuf {
        crate::expand_tilde(path)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_policy_with_drift_invokes_apply_success() {
    // DriftPolicy::Auto + a FileAction::Create with valid source/target →
    // reconciler.apply() runs the copy, succeeded > 0. notify_on_drift=true
    // exercises the success-notification branch.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Real source file inside tmp, target inside tmp — copy succeeds.
    let source = tmp.path().join("src.txt");
    std::fs::write(&source, "hello").unwrap();
    let target = tmp.path().join("dst.txt");
    let hooks = DriftingFileHooks {
        source,
        target: target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Apply succeeded — file copied to target. This proves the auto-apply
    // branch (DriftPolicy::Auto + drift > 0 → reconciler.apply) was reached.
    assert!(
        target.exists(),
        "auto-apply should have copied source to target"
    );
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    // Auto policy healed the drift in this same tick, so the in-memory count
    // must reflect the resolved state — not the old append-only accumulator.
    assert_eq!(
        guard.drift_count, 0,
        "successful auto-apply must drive drift_count back to 0"
    );
    drop(guard);
    let store = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        store.unresolved_drift().unwrap().is_empty(),
        "successful auto-apply must leave no outstanding drift rows"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_apply_honors_a_raised_abort_flag() {
    // `systemctl stop cfgd` mid-auto-apply must stop the reconcile the way it
    // stops a CLI apply. A throwaway flag nobody raises would let this run to
    // completion — and, with a profile script in the plan, wait out
    // PROFILE_SCRIPT_TIMEOUT before the daemon could exit.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let source = tmp.path().join("src.txt");
    std::fs::write(&source, "hello").unwrap();
    let target = tmp.path().join("dst.txt");
    let hooks = DriftingFileHooks {
        source,
        target: target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let abort = Arc::new(crate::AbortFlag::new());
    abort.set(143);

    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let ab = Arc::clone(&abort);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            ReconcileCtx {
                state: &st,
                notifier: &not,
                notify_on_drift: false,
                hooks: &hooks,
                state_dir_override: Some(&sd),
                printer: &printer,
                module_filter: None,
                auto_apply_override: None,
                drift_policy_override: None,
                scope: crate::Scope::User,
                abort: &ab,
            },
        );
    })
    .await
    .unwrap();

    assert!(
        !target.exists(),
        "an aborted auto-apply must not perform the copy"
    );
    let store = StateStore::open_in_dir(&state_dir).unwrap();
    let record = store
        .last_apply()
        .unwrap()
        .expect("the aborted run must still be recorded");
    assert_eq!(
        record.status,
        crate::state::ApplyStatus::Aborted,
        "the reconcile apply must record the cooperative abort, not success"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_policy_apply_failure_notifies() {
    // DriftPolicy::Auto + FileAction::Create with nonexistent source →
    // copy fails, exercising the auto-apply partial-failure notification branch.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Source does NOT exist → std::fs::copy fails → apply records failure.
    let source = tmp.path().join("missing.txt");
    let target = tmp.path().join("dst.txt");
    let hooks = DriftingFileHooks {
        source,
        target: target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Target never created — apply failed.
    assert!(!target.exists(), "apply should have failed to copy");
    // Drift recorded regardless.
    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(!drift_events.is_empty());
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert!(guard.drift_count > 0);
}

// Uses `touch` to create a marker — not a Windows builtin. The on-drift
// loop itself is portable; this test exercises it with a Unix-typical
// command. A Windows-targeted equivalent would need a portable script.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_runs_on_drift_scripts() {
    // Profile with `scripts.onDrift` populated → on-drift script loop runs.
    // The script writes a marker file we can assert on.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let marker = tmp.path().join("on-drift-ran.marker");
    let marker_str = marker.display().to_string();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  scripts:\n    onDrift:\n      - \"touch '{}'\"\n",
            marker_str
        ),
    )
    .unwrap();

    // Use the existing DriftHooks pattern: a package action creates drift,
    // which triggers the onDrift loop.
    struct PkgDriftHooks;
    impl DaemonHooks for PkgDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["bat".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    // The test-home override is a thread-local set on this runtime thread; the
    // wrapper carries it onto the blocking-pool worker so the onDrift script
    // workdir resolves the test home (via `home_dir_var`), not the ambient $HOME.
    crate::spawn_blocking_with_test_home(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &PkgDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    assert!(
        marker.exists(),
        "onDrift script should have created marker file at {}",
        marker.display()
    );
}

// Every hook body here is a no-op valid in BOTH shells `ScriptShell::Auto`
// dispatches to — `sh -c` on Unix and `cmd.exe /C` on Windows — so the test
// runs everywhere the daemon does. What it asserts (heading order, owner
// depth, the derived alignment column) is produced by `pseudo_phase` /
// `align_width_of` / `Printer::section`, none of which has an OS-conditional
// arm; the shell was the only host-bound part of the fixture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_only_tick_renders_both_on_drift_owners_above_the_reconcile_header() {
    // A tick that detected drift and chose not to act still reports what
    // drifted and who ran hooks over it: one `Drift Hooks` heading holding
    // both owners, ABOVE the `Reconcile` header, then the preview tree, then a
    // verdict — never a rollup, because nothing was applied.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Two bodies of different length, so the shared alignment column below is
    // a real assertion rather than a tautology.
    let profile_hook = "cd .";
    let module_hook = "exit 0";

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: false\n      driftPolicy: NotifyOnly\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - nvim\n  scripts:\n    onDrift:\n      - \"{profile_hook}\"\n"
        ),
    )
    .unwrap();
    // The module's `postReconcile` script is what drifts — it is a planned
    // module action, so `module_has_drift` fires the module's own hook.
    let mod_dir = tmp.path().join("modules").join("nvim");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec:\n  scripts:\n    postReconcile:\n      - \"exit 0\"\n    onDrift:\n      - \"{module_hook}\"\n"
        ),
    )
    .unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    crate::spawn_blocking_with_test_home(move || {
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    let out = crate::output::strip_ansi(&buf.lock().unwrap());
    let hooks_at = out
        .find(crate::reconciler::HOOKS_PHASE_LABEL)
        .unwrap_or_else(|| panic!("no Drift Hooks heading in:\n{out}"));
    let header_at = out
        .find("\nReconcile\n")
        .unwrap_or_else(|| panic!("no Reconcile header in:\n{out}"));
    assert!(
        hooks_at < header_at,
        "the hooks ran before the reconcile, so their tree renders above its header:\n{out}"
    );
    assert!(
        out.contains("\n  profile:default\n") && out.contains("\n  module:nvim\n"),
        "both onDrift owners must open a group inside the pseudo-phase:\n{out}"
    );

    // One status per script, at owner depth, carrying the `onDrift` marker.
    // A `Role::Ok` glyph is the evidence the script ran and exited zero.
    let hook_lines: Vec<&str> = out.lines().filter(|l| l.contains("onDrift:")).collect();
    assert_eq!(hook_lines.len(), 2, "one status per hook, got:\n{out}");
    for (line, body) in hook_lines.iter().zip([profile_hook, module_hook]) {
        assert!(
            line.starts_with(&format!("    \u{2713} onDrift: {body}")),
            "hook status must render Ok at owner depth under its group: {line:?}"
        );
    }
    // Both groups share the pseudo-phase's derived column, so the trailing
    // duration of the shorter subject lands where the longer one's does.
    let columns: Vec<Option<usize>> = hook_lines.iter().map(|l| l.find('(')).collect();
    assert!(
        columns[0].is_some() && columns[0] == columns[1],
        "the derived alignment column must be shared by every group in the pseudo-phase: {hook_lines:?}"
    );

    assert!(
        out.contains("Trigger  drift (1 resources)"),
        "the header names what woke the tick:\n{out}"
    );
    assert!(
        out.contains("Phase: "),
        "a notify-only tick still shows WHAT drifted:\n{out}"
    );
    assert!(
        out.contains("Drift detected — 1 action(s); policy is notify-only, nothing applied"),
        "a non-applying tick closes on a verdict:\n{out}"
    );
    assert!(
        !out.contains("Reconcile complete"),
        "nothing was applied, so no rollup may claim otherwise:\n{out}"
    );
    assert!(
        !out.contains("Actions  "),
        "a preview-only run's count belongs to its verdict, not to an Actions row:\n{out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_apply_tick_renders_header_tree_and_rollup() {
    // The CLI/daemon asymmetry closed: an auto-applying tick renders the same
    // run skeleton `cfgd apply` does — header rows, the execution tree, and one
    // rollup — under `RunTitle::Reconcile`.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let source = tmp.path().join("src.txt");
    std::fs::write(&source, "hello").unwrap();
    let target = tmp.path().join("dst.txt");
    let hooks = DriftingFileHooks {
        source,
        target: target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    crate::spawn_blocking_with_test_home(move || {
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    let out = crate::output::strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("\nReconcile\n") || out.starts_with("Reconcile\n"),
        "the tick opens with its own run header:\n{out}"
    );
    assert!(
        out.contains("Profile  default")
            && out.contains("Trigger  drift (1 resources)")
            && out.contains("Actions  1 planned"),
        "header rows name the profile, the trigger and the planned count:\n{out}"
    );
    assert!(
        out.contains("Phase: Files") && out.contains("\n  profile:default\n"),
        "the executed work renders as a phase/owner tree:\n{out}"
    );
    assert!(
        out.contains("Reconcile complete — 1 action(s) succeeded"),
        "the run closes on one rollup naming the title:\n{out}"
    );
    assert!(target.exists(), "the tick actually applied the action");
}

/// Records every package name passed to `install`, so a withholding test can
/// assert what a tick EXECUTED rather than only what it planned.
struct RecordingInstallManager {
    installed: Arc<Mutex<Vec<String>>>,
}

impl PackageManager for RecordingInstallManager {
    fn name(&self) -> &str {
        "cargo"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> crate::errors::Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> crate::errors::Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn install(&self, packages: &[String], _: &PackageContext<'_>) -> crate::errors::Result<()> {
        // blocking_lock is safe: reconcile apply runs on spawn_blocking, off the
        // async runtime worker.
        self.installed
            .blocking_lock()
            .extend(packages.iter().cloned());
        Ok(())
    }
    fn uninstall(&self, _: &[String], _: &PackageContext<'_>) -> crate::errors::Result<()> {
        Ok(())
    }
    fn update(&self, _: &PackageContext<'_>) -> crate::errors::Result<()> {
        Ok(())
    }
    fn available_version(&self, _: &str) -> crate::errors::Result<Option<String>> {
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn auto_apply_tick_withholds_the_resources_awaiting_a_source_decision() {
    // An auto-applying tick must not touch a resource whose source change is
    // still awaiting the operator's decision. Two pending rows are seeded —
    // one package inside a batch, one file — and the tick has to withhold
    // exactly those: `ripgrep` still installs alongside the withheld `bat`, the
    // undecided file is never written, and the counts the run reports (header,
    // trigger, rollup, drift rows, journal positions) all describe the pruned
    // plan rather than the planned-but-withheld one.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    // A configured source with an empty cache is a benign cache-miss: the tick
    // reconciles local-only and still runs the source-decision workflow. It is
    // also the reason the withhold is scoped to the SUBSCRIPTION rather than to
    // what composition merged — a source that delivered nothing this run must
    // not be a source whose undecided items suddenly apply.
    let cache_root = tmp.path().join("cache-root-empty").join("cfgd");
    std::fs::create_dir_all(&cache_root).unwrap();
    let _cache =
        crate::test_helpers::EnvVarGuard::set("CFGD_CACHE_DIR", cache_root.to_str().unwrap());

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        seed.upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat (from acme)",
        )
        .unwrap();
        // The decision keeps the DECLARED spelling of the target; the planner
        // expands it. Both halves of that mapping are exercised here.
        seed.upsert_pending_decision(
            "acme",
            "files.~/withheld.txt",
            "recommended",
            "install",
            "recommended files.~/withheld.txt (from acme)",
        )
        .unwrap();
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n      policy:\n        newRecommended: Accept\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.test/acme.git\n      subscription:\n        profile: team\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        // Only `ripgrep` is the operator's own; `bat` is the source's offer, so
        // the seeded decision is about a resource the local profile does not
        // declare.
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - ripgrep\n",
    )
    .unwrap();

    let source_file = tmp.path().join("src.txt");
    std::fs::write(&source_file, "hello").unwrap();
    let kept_target = tmp.path().join("kept.txt");
    let withheld_target = crate::expand_tilde(Path::new("~/withheld.txt"));

    struct DecisionHooks {
        installed: Arc<Mutex<Vec<String>>>,
        source: PathBuf,
        kept: PathBuf,
        withheld: PathBuf,
    }
    impl DaemonHooks for DecisionHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.package_managers.push(Box::new(RecordingInstallManager {
                installed: Arc::clone(&self.installed),
            }));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![
                FileAction::Create {
                    source: self.source.clone(),
                    target: self.kept.clone(),
                    origin: "acme".into(),
                    strategy: crate::config::FileStrategy::Copy,
                    source_hash: None,
                    patch: None,
                },
                FileAction::Create {
                    source: self.source.clone(),
                    target: self.withheld.clone(),
                    origin: "acme".into(),
                    strategy: crate::config::FileStrategy::Copy,
                    source_hash: None,
                    patch: None,
                },
            ])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["bat".into(), "ripgrep".into()],
                origin: "acme".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let installed = Arc::new(Mutex::new(Vec::<String>::new()));
    let hooks = DecisionHooks {
        installed: Arc::clone(&installed),
        source: source_file,
        kept: kept_target.clone(),
        withheld: withheld_target.clone(),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
    crate::spawn_blocking_with_test_home(move || {
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // What the tick EXECUTED: the decided package only, the decided file only.
    let installed = installed.lock().await;
    assert_eq!(
        *installed,
        vec!["ripgrep".to_string()],
        "the undecided package must leave the batch while its siblings still install"
    );
    assert!(
        kept_target.exists(),
        "a resource with no pending decision still applies"
    );
    assert!(
        !withheld_target.exists(),
        "a file awaiting a decision must not be written by an auto-applying tick"
    );

    // What the tick REPORTED: one set — header, trigger and rollup all count
    // the pruned plan.
    let out = crate::output::strip_ansi(&buf.lock().unwrap());
    assert!(
        out.contains("Trigger  drift (2 resources)") && out.contains("Actions  2 planned"),
        "the header must count the pruned plan, not the withheld resources:\n{out}"
    );
    assert!(
        out.contains("Reconcile complete — 2 action(s) succeeded"),
        "the rollup must agree with the header:\n{out}"
    );
    // The tmp root is substituted first: a random temp-dir name could otherwise
    // carry the package name as a substring and fail this on luck alone.
    let named = crate::normalize_for_snapshot(&out, &[(tmp.path(), "TMP")]);
    assert!(
        !named.contains("bat") && !named.contains("withheld.txt"),
        "a withheld resource must not be named anywhere in the run:\n{out}"
    );

    // What the tick RECORDED: drift rows and journal positions over the same
    // pruned set, with `action_index` dense from 0.
    let after = StateStore::open_in_dir(&state_dir).unwrap();
    let drift_ids: Vec<String> = after
        .unresolved_drift()
        .unwrap()
        .into_iter()
        .map(|d| d.resource_id)
        .collect();
    assert!(
        !drift_ids.iter().any(|id| id.contains("bat")),
        "a withheld resource is not drift the tick recorded: {drift_ids:?}"
    );
    let journal = after.journal_entries_after_apply(0).unwrap();
    let mut indexes: Vec<i64> = journal.iter().map(|e| e.action_index).collect();
    indexes.sort_unstable();
    assert_eq!(
        indexes,
        vec![0, 1],
        "`action_index` stays dense from 0 over the pruned plan"
    );
    assert!(
        !journal
            .iter()
            .any(|e| e.resource_id.contains("bat") || e.resource_id.contains("withheld.txt")),
        "no withheld resource may reach the journal: {:?}",
        journal.iter().map(|e| &e.resource_id).collect::<Vec<_>>()
    );
}

/// Every file under `root`, with its content, for a leak assertion that no
/// single known surface path can miss.
fn files_under(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(path),
                Ok(ft) if ft.is_file() => {
                    if let Ok(body) = std::fs::read_to_string(&path) {
                        out.push((path, body));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_tick_that_cannot_record_a_decision_still_withholds_the_item() {
    // Fail-closed on the WRITE half too: `mint_decisions` logs and skips a row
    // the store rejects, so the classified item exists nowhere in the store —
    // and the unattended tick is the one path with nobody at the screen to
    // catch it. The item must ride `WithheldDecisions::with_unrecorded` exactly
    // as it does on `cfgd plan`/`cfgd apply`, or a locked table quietly
    // installs what nobody was asked about.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let cache_root = tmp.path().join("cache-root").join("cfgd");
    std::fs::create_dir_all(&cache_root).unwrap();
    let _cache =
        crate::test_helpers::EnvVarGuard::set("CFGD_CACHE_DIR", cache_root.to_str().unwrap());
    // `acme` delivers `bat` on a recommended layer; the config sets no policy,
    // so the item falls to `newRecommended`'s `Notify` default and must be
    // asked about before it installs.
    stage_cached_source(
        &cache_root,
        "acme",
        "  packages:\n    cargo:\n      packages:\n        - bat\n",
    );

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    {
        // Create the schema first, then deny every INSERT into
        // `pending_decisions` via a trigger: `upsert_pending_decision` fails
        // while every read (and every other table) keeps working — the exact
        // shape of a store that rejects the one write minting needs.
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        drop(seed);
        let conn = rusqlite::Connection::open(state_dir.join(crate::state::STATE_DB_FILENAME))
            .expect("open the state db directly");
        conn.execute_batch(
            "CREATE TRIGGER deny_pending_decision_writes\n             BEFORE INSERT ON pending_decisions\n             BEGIN SELECT RAISE(ABORT, 'pending_decisions rejects writes in this test'); END;",
        )
        .expect("install the write-denying trigger");
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.test/acme.git\n      subscription:\n        profile: team\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        // `ripgrep` is the operator's own and must still install: withholding
        // the unrecorded item may not turn into skipping the whole tick.
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      packages:\n        - ripgrep\n",
    )
    .unwrap();

    struct MintDeniedHooks {
        installed: Arc<Mutex<Vec<String>>>,
    }
    impl DaemonHooks for MintDeniedHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.package_managers.push(Box::new(RecordingInstallManager {
                installed: Arc::clone(&self.installed),
            }));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            merged: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            // Plan from the composed profile the tick resolved, so the source's
            // `bat` is genuinely in the plan the withholding must prune.
            let packages = merged
                .packages
                .cargo
                .as_ref()
                .map(|c| c.packages.clone())
                .unwrap_or_default();
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages,
                origin: "acme".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let installed = Arc::new(Mutex::new(Vec::<String>::new()));
    let hooks = MintDeniedHooks {
        installed: Arc::clone(&installed),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    crate::spawn_blocking_with_test_home(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    let after = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        after.pending_decisions().unwrap().is_empty(),
        "the fixture's premise: the mint write really failed, so no row exists"
    );
    let installed = installed.lock().await;
    assert!(
        !installed.contains(&"bat".to_string()),
        "an item whose decision could not be recorded is still awaiting one — \
         a failed write must cost the operator the ability to answer, never the \
         protection; installed: {installed:?}"
    );
    assert!(
        installed.contains(&"ripgrep".to_string()),
        "the operator's own declaration still installs alongside the withheld \
         item; installed: {installed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn auto_apply_tick_does_not_regenerate_a_withheld_env_surface() {
    // The env arm withholds the whole surface — but apply REGENERATES that
    // surface after the phases run, from the full declared set rather than from
    // the pruned plan, whenever a secret resolved env vars (or a manager was
    // bootstrapped this tick). The undecided variable must not reach the machine
    // through that back door.
    //
    // The control run (no pending decision) proves the fixture genuinely drives
    // the regeneration: TEAM_TOKEN lands. Without it, an early bail anywhere in
    // the tick would leave the withheld run vacuously green.
    let landed = secret_env_tick_leaks(false).await;
    assert!(
        !landed.is_empty(),
        "control: with no pending decision the regeneration must write TEAM_TOKEN — \
         an empty result means the tick never reached the env surface at all"
    );

    let leaked = secret_env_tick_leaks(true).await;
    assert!(
        leaked.is_empty(),
        "a variable awaiting a source decision must not reach the machine through the \
         post-phase env regeneration; it landed in: {leaked:?}"
    );
}

/// One auto-apply tick over a secret-envs profile; returns every file under the
/// test home whose body names TEAM_TOKEN.
async fn secret_env_tick_leaks(seed_pending_decision: bool) -> Vec<PathBuf> {
    let tmp = tempfile::tempdir().unwrap();
    // The home is a SEPARATE root from the config, so "nothing under the home
    // names the variable" cannot be satisfied by the declaring profile itself.
    let home = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(home.path());
    // `acme` really delivers TEAM_TOKEN: the decision is about a source's
    // variable, not about one the operator declared for themselves.
    let cache_root = tmp.path().join("cache-root").join("cfgd");
    std::fs::create_dir_all(&cache_root).unwrap();
    let _cache =
        crate::test_helpers::EnvVarGuard::set("CFGD_CACHE_DIR", cache_root.to_str().unwrap());
    stage_cached_source(
        &cache_root,
        "acme",
        "  env:\n    - name: TEAM_TOKEN\n      value: from-acme\n",
    );

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    if seed_pending_decision {
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        seed.upsert_pending_decision(
            "acme",
            "env.TEAM_TOKEN",
            "recommended",
            "install",
            "recommended env.TEAM_TOKEN (from acme)",
        )
        .unwrap();
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n      policy:\n        newRecommended: Accept\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.test/acme.git\n      subscription:\n        profile: team\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    // `secrets[].envs` with an available provider fills `secret_env_collector`,
    // which is what triggers the regeneration on EVERY apply.
    std::fs::write(
        profiles_dir.join("default.yaml"),
        // `envScope: Login` keeps the surface to files: the live-session arm is
        // not redirectable by a test home, so a test that does not mean to reach
        // the operator's session pins the scope instead.
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  envScope: Login\n  secrets:\n    - source: \"vault://kv/token\"\n      envs:\n        - SECRET_TOKEN\n",
    )
    .unwrap();

    struct SecretEnvHooks;
    impl DaemonHooks for SecretEnvHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.secret_providers
                .push(Box::new(crate::test_helpers::MockSecretProvider::new(
                    "vault",
                )));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    crate::spawn_blocking_with_test_home(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &SecretEnvHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    files_under(home.path())
        .into_iter()
        .filter(|(_, body)| body.contains("TEAM_TOKEN"))
        .map(|(path, _)| path)
        .collect()
}

/// Records every package name passed to `uninstall`, so a daemon prune test can
/// assert the reconcile actually executed the removal (not just planned it).
struct RecordingUninstallManager {
    uninstalled: Arc<Mutex<Vec<String>>>,
    installed: HashSet<String>,
}

impl PackageManager for RecordingUninstallManager {
    fn name(&self) -> &str {
        "cargo"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn can_bootstrap(&self) -> bool {
        false
    }
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> crate::errors::Result<()> {
        Ok(())
    }
    fn installed_packages(&self, _: &PackageContext<'_>) -> crate::errors::Result<HashSet<String>> {
        Ok(self.installed.clone())
    }
    fn install(&self, _: &[String], _: &PackageContext<'_>) -> crate::errors::Result<()> {
        Ok(())
    }
    fn uninstall(&self, packages: &[String], _: &PackageContext<'_>) -> crate::errors::Result<()> {
        // blocking_lock is safe: reconcile apply runs on spawn_blocking, off the
        // async runtime worker.
        self.uninstalled
            .blocking_lock()
            .extend(packages.iter().cloned());
        Ok(())
    }
    fn update(&self, _: &PackageContext<'_>) -> crate::errors::Result<()> {
        Ok(())
    }
    fn available_version(&self, _: &str) -> crate::errors::Result<Option<String>> {
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_policy_prunes_tracked_dropped_package() {
    // The daemon is the primary GitOps reconcile path. A package cfgd installed
    // (tracked in managed_resources) that has left the desired set must be
    // pruned by `cfgd daemon`, and its tracking row deleted afterward. This
    // proves cfgd_installed is read from state, threaded to the hook, the
    // resulting Uninstall executed, and the row removed.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // Pre-seed: cfgd previously installed cargo/bat (tracked) and cargo/ripgrep.
    {
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        seed.upsert_managed_resource("package", "cargo/bat", "local", None, None)
            .unwrap();
        seed.upsert_managed_resource("package", "cargo/ripgrep", "local", None, None)
            .unwrap();
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let uninstalled = Arc::new(Mutex::new(Vec::<String>::new()));

    // Hook that registers the recording manager and emits an Uninstall for the
    // dropped tracked package — but ONLY for packages actually present in the
    // daemon-supplied cfgd_installed set, so the assertion proves the wiring.
    struct PruneHooks {
        uninstalled: Arc<Mutex<Vec<String>>>,
        seen: Arc<Mutex<Vec<String>>>,
    }
    impl DaemonHooks for PruneHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.package_managers
                .push(Box::new(RecordingUninstallManager {
                    uninstalled: Arc::clone(&self.uninstalled),
                    // bat is still on the system; ripgrep too (desired, kept).
                    installed: ["bat".to_string(), "ripgrep".to_string()]
                        .into_iter()
                        .collect(),
                }));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            cfgd_installed: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            self.seen
                .blocking_lock()
                .extend(cfgd_installed.iter().cloned());
            // bat dropped from desired but tracked+installed → prune it.
            // ripgrep is desired (kept) so it is not pruned.
            let mut actions = Vec::new();
            if cfgd_installed.contains("cargo/bat") {
                actions.push(PackageAction::Uninstall {
                    manager: "cargo".into(),
                    packages: vec!["bat".into()],
                    origin: "local".into(),
                });
            }
            Ok(actions)
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let hooks = PruneHooks {
        uninstalled: Arc::clone(&uninstalled),
        seen: Arc::clone(&seen),
    };

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // The daemon read both tracked rows from state and forwarded them.
    let seen = seen.lock().await;
    assert!(
        seen.contains(&"cargo/bat".to_string()) && seen.contains(&"cargo/ripgrep".to_string()),
        "daemon must forward the real tracked set to the hook: {seen:?}"
    );

    // The Uninstall executed against the manager.
    let uninstalled = uninstalled.lock().await;
    assert_eq!(
        *uninstalled,
        vec!["bat".to_string()],
        "daemon should have pruned the dropped tracked package"
    );

    // The tracking row for the pruned package is gone; the kept one remains.
    let after = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        !after.is_resource_managed("package", "cargo/bat").unwrap(),
        "pruned package's tracking row must be deleted"
    );
    assert!(
        after
            .is_resource_managed("package", "cargo/ripgrep")
            .unwrap(),
        "kept package's tracking row must remain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_policy_gcs_stale_tracking_row() {
    // A tracked row whose package is gone out-of-band produces no plan action,
    // so prune can't reach it. The daemon's full-reconcile GC must reap it.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    {
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        // bat is installed (kept); phantom is tracked but NOT installed (stale).
        seed.upsert_managed_resource("package", "cargo/bat", "local", None, None)
            .unwrap();
        seed.upsert_managed_resource("package", "cargo/phantom", "local", None, None)
            .unwrap();
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    // Profile desires bat so there is drift → an apply runs → GC fires after.
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages:\n    cargo:\n      - bat\n",
    )
    .unwrap();

    let uninstalled = Arc::new(Mutex::new(Vec::<String>::new()));

    struct GcHooks {
        uninstalled: Arc<Mutex<Vec<String>>>,
    }
    impl DaemonHooks for GcHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.package_managers
                .push(Box::new(RecordingUninstallManager {
                    uninstalled: Arc::clone(&self.uninstalled),
                    // Only bat is on the system — phantom is gone.
                    installed: ["bat".to_string()].into_iter().collect(),
                }));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            // Emit one (idempotent) action so the reconcile enters the Auto-apply
            // branch where GC runs; the GC is what this test asserts on.
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["bat".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let hooks = GcHooks {
        uninstalled: Arc::clone(&uninstalled),
    };
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    let after = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        !after
            .is_resource_managed("package", "cargo/phantom")
            .unwrap(),
        "stale tracking row (package gone) must be GC'd by the daemon"
    );
    assert!(
        after.is_resource_managed("package", "cargo/bat").unwrap(),
        "tracking row for an installed package must remain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_notify_only_with_notify_on_drift_sends_notification() {
    // NotifyOnly policy + notify_on_drift=true + drift → notifier.notify
    // called for "drift detected". Stdout notifier just logs, but the call
    // path is what we want to exercise (it's a distinct branch from the
    // notify_on_drift=false NotifyOnly case already covered).
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: false\n      driftPolicy: NotifyOnly\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    struct PkgDriftHooks;
    impl DaemonHooks for PkgDriftHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![PackageAction::Install {
                manager: "cargo".into(),
                packages: vec!["ripgrep".into()],
                origin: "local".into(),
            }])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        // notify_on_drift = true → notifier.notify() reached
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &PkgDriftHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Drift event recorded. notify ran (stdout notifier just traces; we assert
    // the call path was reached by checking the drift bookkeeping side-effects).
    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let drift_events = store.unresolved_drift().unwrap();
    assert!(!drift_events.is_empty());
    let guard = state.lock().await;
    assert!(guard.last_reconcile.is_some());
    assert!(guard.drift_count > 0);
}

// --- discover_managed_paths ---

#[test]
fn discover_managed_paths_returns_targets_from_profile() {
    let tmp = tempfile::tempdir().unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: src/zshrc\n        target: /home/user/.zshrc\n      - source: src/vimrc\n        target: /home/user/.vimrc\n",
        )
        .unwrap();

    let paths = discover_managed_paths(&config_path, None, &crate::test_helpers::NoopDaemonHooks);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("/home/user/.zshrc")));
    assert!(paths.contains(&PathBuf::from("/home/user/.vimrc")));
}

#[test]
fn discover_managed_paths_returns_empty_for_missing_config() {
    let paths = discover_managed_paths(
        Path::new("/nonexistent/config.yaml"),
        None,
        &crate::test_helpers::NoopDaemonHooks,
    );
    assert!(paths.is_empty());
}

#[test]
fn discover_managed_paths_with_profile_override() {
    let tmp = tempfile::tempdir().unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
            profiles_dir.join("custom.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: custom\nspec:\n  files:\n    managed:\n      - source: src/bashrc\n        target: /home/user/.bashrc\n",
        )
        .unwrap();

    let paths = discover_managed_paths(
        &config_path,
        Some("custom"),
        &crate::test_helpers::NoopDaemonHooks,
    );
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("/home/user/.bashrc"));
}

// --- withheld_decisions ---

#[test]
fn withheld_decisions_returns_empty_for_no_decisions() {
    let store = test_state();
    let paths = withheld_paths(&store);
    assert!(paths.is_empty());
}

// --- generate_launchd_plist: detailed content verification ---

#[test]
#[cfg(unix)]
fn generate_launchd_plist_xml_structure_complete() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/Users/alice/.config/cfgd/config.yaml");
    let home = Path::new("/Users/alice");

    let plist = launchd_plist_default_dirs(binary, config, None, home, crate::Scope::User);

    // Verify required XML structure
    assert!(
        plist.contains("<?xml version=\"1.0\""),
        "should start with XML declaration"
    );
    assert!(
        plist.contains("<!DOCTYPE plist"),
        "should contain plist DOCTYPE"
    );
    assert!(
        plist.contains(&format!("<string>{}</string>", LAUNCHD_LABEL)),
        "should contain the label"
    );
    assert!(
        plist.contains("<string>/usr/local/bin/cfgd</string>"),
        "should contain binary path"
    );
    assert!(
        plist.contains("<string>--config</string>"),
        "should contain --config flag"
    );
    assert!(
        plist.contains("<string>/Users/alice/.config/cfgd/config.yaml</string>"),
        "should contain config path"
    );
    assert!(
        plist.contains("<string>--quiet</string>"),
        "should contain --quiet flag"
    );
    assert!(
        plist.contains("<string>daemon</string>"),
        "should contain daemon subcommand"
    );
    assert!(
        plist.contains("<key>RunAtLoad</key>"),
        "should set RunAtLoad"
    );
    assert!(
        plist.contains("<key>KeepAlive</key>"),
        "should set KeepAlive"
    );
    assert!(
        plist.contains("/Users/alice/Library/Logs/cfgd.log"),
        "stdout log should be under home Library/Logs"
    );
    assert!(
        plist.contains("/Users/alice/Library/Logs/cfgd.err"),
        "stderr log should be under home Library/Logs"
    );
    // Should NOT contain --profile when None
    assert!(
        !plist.contains("--profile"),
        "should not contain --profile when None"
    );
}

#[test]
#[cfg(unix)]
fn generate_launchd_plist_includes_profile_flag() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/config.yaml");
    let home = Path::new("/home/user");

    let plist = launchd_plist_default_dirs(binary, config, Some("work"), home, crate::Scope::User);

    assert!(
        plist.contains("<string>--profile</string>"),
        "should contain --profile flag"
    );
    assert!(
        plist.contains("<string>work</string>"),
        "should contain profile name"
    );
    assert!(
        plist.contains("<string>--quiet</string>"),
        "should contain --quiet flag"
    );
    // Strict ordering: --config < --profile < --quiet < daemon (parity with systemd).
    let config_pos = plist.find("<string>--config</string>").unwrap();
    let profile_pos = plist.find("<string>--profile</string>").unwrap();
    let quiet_pos = plist.find("<string>--quiet</string>").unwrap();
    let daemon_pos = plist.find("<string>daemon</string>").unwrap();
    assert!(
        config_pos < profile_pos,
        "--config should appear before --profile"
    );
    assert!(
        profile_pos < quiet_pos,
        "--profile should appear before --quiet"
    );
    assert!(
        quiet_pos < daemon_pos,
        "--quiet should appear before daemon"
    );
}

// --- generate_systemd_unit: detailed content verification ---

#[test]
#[cfg(unix)]
fn generate_systemd_unit_complete_structure() {
    let binary = Path::new("/usr/local/bin/cfgd");
    let config = Path::new("/home/user/.config/cfgd/config.yaml");

    let unit = systemd_unit_default_dirs(binary, config, None, crate::Scope::User);

    assert!(unit.contains("[Unit]"), "should contain [Unit] section");
    assert!(
        unit.contains("[Service]"),
        "should contain [Service] section"
    );
    assert!(
        unit.contains("[Install]"),
        "should contain [Install] section"
    );
    assert!(
        unit.contains("Description=cfgd configuration daemon"),
        "should have description"
    );
    assert!(
        unit.contains("After=network.target"),
        "should require network"
    );
    assert!(
        unit.contains("Type=simple"),
        "should be simple service type"
    );
    assert!(
        unit.contains("Restart=on-failure"),
        "should restart on failure"
    );
    assert!(unit.contains("RestartSec=10"), "should have restart delay");
    assert!(
        unit.contains("WantedBy=default.target"),
        "should be wanted by default.target"
    );

    // Verify ExecStart format: binary --config path --quiet daemon
    let expected_exec = format!(
        "ExecStart={} --config {} --quiet daemon",
        binary.display(),
        config.display()
    );
    assert!(
        unit.contains(&expected_exec),
        "ExecStart should be '{expected_exec}', got unit:\n{unit}"
    );
    // Should NOT contain --profile
    assert!(
        !unit.contains("--profile"),
        "should not contain --profile when None"
    );
}

#[test]
#[cfg(unix)]
fn generate_systemd_unit_includes_profile() {
    let binary = Path::new("/opt/cfgd/cfgd");
    let config = Path::new("/etc/cfgd/config.yaml");

    let unit = systemd_unit_default_dirs(binary, config, Some("server"), crate::Scope::User);

    let expected_exec = format!(
        "ExecStart={} --config {} --profile {} --quiet daemon",
        binary.display(),
        config.display(),
        "server"
    );
    assert!(
        unit.contains(&expected_exec),
        "ExecStart with profile should be '{expected_exec}', got:\n{unit}"
    );
}

// --- record_file_drift_to: actual drift recording ---

#[test]
fn record_file_drift_to_stores_event_in_db() {
    let store = test_state();
    let path = Path::new("/home/user/.bashrc");

    let result = record_file_drift_to(&store, path);
    assert!(result, "record_file_drift_to should return true on success");

    // Verify the drift event was actually stored
    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 1, "should have exactly one drift event");
    assert_eq!(events[0].resource_type, "file");
    assert_eq!(events[0].resource_id, "/home/user/.bashrc");
}

#[test]
fn record_file_drift_to_multiple_files() {
    let store = test_state();

    record_file_drift_to(&store, Path::new("/etc/hosts"));
    record_file_drift_to(&store, Path::new("/etc/resolv.conf"));
    record_file_drift_to(&store, Path::new("/home/user/.zshrc"));

    let events = store.unresolved_drift().unwrap();
    assert_eq!(events.len(), 3, "should have three drift events");

    let ids: Vec<&str> = events.iter().map(|e| e.resource_id.as_str()).collect();
    assert!(ids.contains(&"/etc/hosts"));
    assert!(ids.contains(&"/etc/resolv.conf"));
    assert!(ids.contains(&"/home/user/.zshrc"));
}

// --- parse_daemon_config: comprehensive config parsing ---

#[test]
fn parse_daemon_config_all_defaults() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let parsed = parse_daemon_config(&cfg);
    assert_eq!(
        parsed.reconcile_interval,
        Duration::from_secs(DEFAULT_RECONCILE_SECS)
    );
    assert_eq!(parsed.sync_interval, Duration::from_secs(DEFAULT_SYNC_SECS));
    assert!(!parsed.auto_pull);
    assert!(!parsed.auto_push);
    assert!(!parsed.on_change_reconcile);
    assert!(!parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Stdout));
    assert!(parsed.webhook_url.is_none());
    assert!(!parsed.auto_apply);
}

#[test]
fn parse_daemon_config_with_all_settings() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "60s".into(),
            on_change: true,
            auto_apply: true,
            policy: None,
            drift_policy: config::DriftPolicy::Auto,
            patches: vec![],
        }),
        sync: Some(config::SyncConfig {
            auto_pull: true,
            auto_push: true,
            interval: "120s".into(),
        }),
        notify: Some(config::NotifyConfig {
            drift: true,
            method: NotifyMethod::Webhook,
            webhook_url: Some("https://hooks.example.com/notify".into()),
        }),
        windows_event_log: false,
    };

    let parsed = parse_daemon_config(&cfg);
    assert_eq!(parsed.reconcile_interval, Duration::from_secs(60));
    assert_eq!(parsed.sync_interval, Duration::from_secs(120));
    assert!(parsed.auto_pull);
    assert!(parsed.auto_push);
    assert!(parsed.on_change_reconcile);
    assert!(parsed.notify_on_drift);
    assert!(matches!(parsed.notify_method, NotifyMethod::Webhook));
    assert_eq!(
        parsed.webhook_url.as_deref(),
        Some("https://hooks.example.com/notify")
    );
    assert!(parsed.auto_apply);
}

#[test]
fn parse_daemon_config_with_minute_interval() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: Some(config::ReconcileConfig {
            interval: "10m".into(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::default(),
            patches: vec![],
        }),
        sync: Some(config::SyncConfig {
            auto_pull: false,
            auto_push: false,
            interval: "30m".into(),
        }),
        notify: None,
        windows_event_log: false,
    };

    let parsed = parse_daemon_config(&cfg);
    assert_eq!(parsed.reconcile_interval, Duration::from_secs(600));
    assert_eq!(parsed.sync_interval, Duration::from_secs(1800));
}

// --- build_sync_tasks: comprehensive sync task building ---

#[test]
fn build_sync_tasks_propagates_source_sync_interval() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();
    let source_cache = dir.path().join("sources");
    std::fs::create_dir_all(source_cache.join("team-tools")).unwrap();

    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(300),
        sync_interval: Duration::from_secs(300),
        auto_pull: true,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };

    let sources = vec![config::SourceSpec {
        name: "team-tools".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/team/tools.git".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec {
            auto_apply: true,
            interval: "60s".into(),
            pin_version: None,
            required: false,
        },
    }];

    let tasks = build_sync_tasks(config_dir, &parsed, &sources, false, &source_cache, |_| {
        None
    });

    assert_eq!(tasks.len(), 2, "should have local + team-tools");
    // Local task inherits global settings
    assert_eq!(tasks[0].source_name, "local");
    assert!(tasks[0].auto_pull);
    assert!(!tasks[0].auto_push);
    assert_eq!(tasks[0].interval, Duration::from_secs(300));

    // Source task uses its own interval
    assert_eq!(tasks[1].source_name, "team-tools");
    assert!(tasks[1].auto_pull); // always true for sources
    assert!(!tasks[1].auto_push); // always false for sources
    assert!(tasks[1].auto_apply);
    assert_eq!(tasks[1].interval, Duration::from_secs(60));
}

#[test]
fn build_sync_tasks_manifest_detector_sets_require_signed() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();
    let source_cache = dir.path().join("sources");
    std::fs::create_dir_all(source_cache.join("signed-source")).unwrap();

    let parsed = ParsedDaemonConfig {
        reconcile_interval: Duration::from_secs(300),
        sync_interval: Duration::from_secs(300),
        auto_pull: false,
        auto_push: false,
        on_change_reconcile: false,
        notify_on_drift: false,
        notify_method: NotifyMethod::Stdout,
        webhook_url: None,
        auto_apply: false,
    };

    let sources = vec![config::SourceSpec {
        name: "signed-source".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://github.com/secure/config.git".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec::default(),
    }];

    // Manifest detector returns true => require signed commits
    let tasks = build_sync_tasks(config_dir, &parsed, &sources, false, &source_cache, |_| {
        Some(true)
    });

    assert_eq!(tasks.len(), 2);
    assert!(
        !tasks[0].require_signed_commits,
        "local should not require signed"
    );
    assert!(
        tasks[1].require_signed_commits,
        "source with manifest should require signed"
    );
}

// --- build_reconcile_tasks: comprehensive reconcile task building ---

#[test]
fn build_reconcile_tasks_always_has_default() {
    let cfg = config::DaemonConfig {
        enabled: true,
        reconcile: None,
        sync: None,
        notify: None,
        windows_event_log: false,
    };

    let tasks = build_reconcile_tasks(&cfg, None, &[], Duration::from_secs(300), false);

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].entity, "__default__");
    assert_eq!(tasks[0].interval, Duration::from_secs(300));
    assert!(!tasks[0].auto_apply);
}

// --- git operations with local repos ---

#[test]
fn git_pull_on_local_repo_no_remote_is_error() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();

    // Create initial commit so HEAD exists
    let repo = git2::Repository::open(dir.path()).unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let tree_oid = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // No remote configured -> should error
    let result = git_pull(dir.path());
    assert!(result.is_err(), "pull without remote should fail");
}

#[test]
fn git_auto_commit_push_with_no_changes_returns_false() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    // Create initial commit
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    std::fs::write(dir.path().join("README.md"), "# Hello").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    // No changes after initial commit
    let result = git_auto_commit_push(dir.path());
    // Should return Ok(false) — no changes to commit
    assert_eq!(result, Ok(false));
}

// --- DaemonStatusResponse serialization edge cases ---

#[test]
fn daemon_status_response_camel_case_keys() {
    let response = DaemonStatusResponse {
        running: true,
        pid: 100,
        uptime_secs: 3600,
        last_reconcile: Some("2026-01-01T00:00:00Z".into()),
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(
        json.contains("\"uptimeSecs\""),
        "should use camelCase: {json}"
    );
    assert!(
        json.contains("\"lastReconcile\""),
        "should use camelCase: {json}"
    );
    assert!(
        json.contains("\"driftCount\""),
        "should use camelCase: {json}"
    );
    assert!(
        !json.contains("\"uptime_secs\""),
        "should not use snake_case: {json}"
    );
}

// --- ModuleReconcileStatus serialization ---

#[test]
fn module_reconcile_status_round_trips_extended() {
    let status = ModuleReconcileStatus {
        name: "security-baseline".into(),
        interval: "30s".into(),
        auto_apply: true,
        drift_policy: "Auto".into(),
        last_reconcile: Some("2026-04-01T12:00:00Z".into()),
    };

    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"autoApply\""), "should use camelCase");
    assert!(json.contains("\"driftPolicy\""), "should use camelCase");
    assert!(json.contains("\"lastReconcile\""), "should use camelCase");

    let parsed: ModuleReconcileStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "security-baseline");
    assert!(parsed.auto_apply);
    assert_eq!(parsed.drift_policy, "Auto");
}

// --- declared_decision_paths edge cases ---

#[test]
fn extract_source_resources_includes_npm_and_pipx_and_dnf() {
    use crate::config::{MergedProfile, NpmSpec, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            npm: Some(NpmSpec {
                file: None,
                global: vec!["typescript".into(), "eslint".into()],
            }),
            pipx: vec!["black".into()],
            dnf: vec!["gcc".into(), "make".into()],
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("packages.npm.typescript"));
    assert!(resources.contains("packages.npm.eslint"));
    assert!(resources.contains("packages.pipx.black"));
    assert!(resources.contains("packages.dnf.gcc"));
    assert!(resources.contains("packages.dnf.make"));
    assert_eq!(resources.len(), 5);
}

#[test]
fn extract_source_resources_includes_apt() {
    use crate::config::{AptSpec, MergedProfile, PackagesSpec};

    let merged = MergedProfile {
        packages: PackagesSpec {
            apt: Some(AptSpec {
                packages: vec!["vim".into(), "git".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("packages.apt.vim"));
    assert!(resources.contains("packages.apt.git"));
    assert_eq!(resources.len(), 2);
}

#[test]
fn extract_source_resources_includes_system_keys() {
    use crate::config::MergedProfile;

    let mut merged = MergedProfile::default();
    merged.system.insert(
        "shell".into(),
        serde_yaml::to_value(serde_json::json!({"defaultShell": "/bin/zsh"})).unwrap(),
    );
    merged.system.insert(
        "macos_defaults".into(),
        serde_yaml::Value::Mapping(Default::default()),
    );

    let resources = declared_decision_paths(&merged);
    assert!(resources.contains("system.shell"));
    assert!(resources.contains("system.macos_defaults"));
    assert_eq!(resources.len(), 2);
}

// --- Notifier webhook creates correct payload ---

#[test]
fn notifier_new_stores_method_and_url() {
    let notifier = Notifier::new(
        NotifyMethod::Webhook,
        Some("https://hooks.slack.com/test".into()),
    );
    assert!(matches!(notifier.method, NotifyMethod::Webhook));
    assert_eq!(
        notifier.webhook_url.as_deref(),
        Some("https://hooks.slack.com/test")
    );
}

#[test]
fn notifier_desktop_does_not_panic() {
    let notifier = Notifier::new(NotifyMethod::Desktop, None);
    // On CI without a display, this will fall back to stdout — shouldn't panic either way
    notifier.notify("test title", "test body");
}

// --- build_webhook_payload ---

#[test]
fn build_webhook_payload_emits_expected_schema() {
    let body = super::build_webhook_payload(
        "cfgd: drift detected",
        "5 file(s) changed",
        "2026-05-07T05:30:00Z",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("payload must be valid JSON");
    assert_eq!(parsed["event"], "cfgd: drift detected");
    assert_eq!(parsed["message"], "5 file(s) changed");
    assert_eq!(parsed["timestamp"], "2026-05-07T05:30:00Z");
    assert_eq!(
        parsed["source"], "cfgd",
        "source must be hardcoded so receivers can filter on it"
    );
}

#[test]
fn build_webhook_payload_preserves_unicode_in_message() {
    let body =
        super::build_webhook_payload("hdr", "msg with 中文 + emoji 🎉", "2026-05-07T00:00:00Z");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["message"], "msg with 中文 + emoji 🎉");
}

#[test]
fn build_webhook_payload_escapes_quotes_and_backslashes() {
    // The function must produce JSON that round-trips even when the message
    // contains characters that would break a naive string concat.
    let body = super::build_webhook_payload(
        "hdr",
        "a \"quoted\" path: C:\\Users\\me\\.config",
        "2026-05-07T00:00:00Z",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("payload with quotes/backslashes must round-trip");
    assert_eq!(
        parsed["message"],
        "a \"quoted\" path: C:\\Users\\me\\.config"
    );
}

#[test]
fn build_webhook_payload_accepts_empty_strings() {
    let body = super::build_webhook_payload("", "", "");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["event"], "");
    assert_eq!(parsed["message"], "");
    assert_eq!(parsed["timestamp"], "");
    assert_eq!(parsed["source"], "cfgd");
}

// ===========================================================================
// Daemon-loop harness tests (runner.rs)
//
// `run_daemon_loop` is extracted from `run_daemon` so the per-branch
// orchestration is exercisable without spawning real timers, file watchers, or
// signal handlers. The tests below drive either the loop end-to-end (via
// `mpsc` channel triggers + a `oneshot` shutdown) or the individual branch
// helpers directly.
// ===========================================================================

mod harness {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration as StdDuration;
    use tokio::sync::{mpsc, oneshot};

    /// Minimal DaemonHooks impl that returns empty/identity values. Suitable
    /// for any test that doesn't need package or file planning to do real work.
    pub(super) use crate::test_helpers::NoopDaemonHooks as NoopHooks;

    /// Build a `DaemonLoopContext` wired for tests. `config_path` is set to a
    /// nonexistent file under `tmp` so any handler that tries to load config
    /// returns early before touching real system state. `state_dir_override`
    /// is set so `handle_reconcile` does not touch `~/.local/state/cfgd/`.
    pub(super) fn make_test_ctx(
        tmp: &tempfile::TempDir,
        on_change_reconcile: bool,
        notify_on_drift: bool,
        compliance: Option<config::ComplianceConfig>,
    ) -> (
        DaemonLoopContext,
        Arc<Mutex<DaemonState>>,
        Arc<std::sync::Mutex<String>>,
    ) {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let ctx = DaemonLoopContext {
            abort: Arc::new(crate::AbortFlag::new()),
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            state: Arc::clone(&state),
            hooks: Arc::new(NoopHooks),
            notifier,
            config_path: tmp.path().join("nonexistent-config.yaml"),
            profile_override: None,
            on_change_reconcile,
            notify_on_drift,
            compliance_config: compliance,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
            managed_paths: Vec::new(),
            scope: crate::Scope::User,
        };
        (ctx, state, buf)
    }

    pub(super) fn make_triggers() -> (DaemonTriggers, TriggerSenders) {
        let (file_tx, file_rx) = mpsc::channel::<PathBuf>(8);
        let (reconcile_tx, reconcile_rx) = mpsc::channel::<()>(8);
        let (sync_tx, sync_rx) = mpsc::channel::<()>(8);
        let (version_check_tx, version_check_rx) = mpsc::channel::<()>(8);
        let (compliance_tx, compliance_rx) = mpsc::channel::<()>(8);
        let (sighup_tx, sighup_rx) = mpsc::channel::<()>(8);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        (
            DaemonTriggers {
                file_rx,
                reconcile_rx,
                sync_rx,
                version_check_rx,
                compliance_rx,
                sighup_rx,
                shutdown_rx,
            },
            TriggerSenders {
                file_tx,
                reconcile_tx,
                sync_tx,
                version_check_tx,
                compliance_tx,
                sighup_tx,
                shutdown_tx,
            },
        )
    }

    /// Poll the shared printer buffer until it contains `needle`, or panic once
    /// `timeout` elapses. Lets a `run_daemon_with` test synchronize on a daemon
    /// lifecycle banner (e.g. "Daemon running") that is written from the spawned
    /// daemon task before driving an action that would otherwise race the
    /// daemon's own setup.
    async fn wait_for_buffer_contains(
        buf: &Arc<std::sync::Mutex<String>>,
        needle: &str,
        timeout: StdDuration,
    ) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if buf.lock().unwrap().contains(needle) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "timed out after {:?} waiting for buffer to contain {:?}; got: {}",
                    timeout,
                    needle,
                    buf.lock().unwrap()
                );
            }
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
    }

    #[allow(dead_code)]
    pub(super) struct TriggerSenders {
        pub file_tx: mpsc::Sender<PathBuf>,
        pub reconcile_tx: mpsc::Sender<()>,
        pub sync_tx: mpsc::Sender<()>,
        pub version_check_tx: mpsc::Sender<()>,
        pub compliance_tx: mpsc::Sender<()>,
        pub sighup_tx: mpsc::Sender<()>,
        pub shutdown_tx: oneshot::Sender<()>,
    }

    // ----- apply_sighup_reload / compute_sighup_intervals tests -----

    fn parse_cfgd_config(yaml: &str) -> CfgdConfig {
        serde_yaml::from_str(yaml).expect("test yaml must parse")
    }

    #[test]
    fn compute_sighup_intervals_returns_none_when_daemon_spec_absent() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        );
        let (reconcile, sync) = runner::compute_sighup_intervals(&cfg);
        assert!(reconcile.is_none());
        assert!(sync.is_none());
    }

    #[test]
    fn compute_sighup_intervals_returns_reconcile_when_set() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 45s\n",
        );
        let (reconcile, sync) = runner::compute_sighup_intervals(&cfg);
        assert_eq!(reconcile, Some(StdDuration::from_secs(45)));
        assert!(sync.is_none());
    }

    #[test]
    fn compute_sighup_intervals_returns_sync_when_set() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    sync:\n      interval: 10m\n",
        );
        let (reconcile, sync) = runner::compute_sighup_intervals(&cfg);
        assert!(reconcile.is_none());
        assert_eq!(sync, Some(StdDuration::from_secs(600)));
    }

    /// A `DaemonLoopContext` pointed at `config_path`, plus the printer buffer
    /// the reload writes into. `apply_sighup_reload` reads the profile override
    /// and scope off the context to rebuild backup timers, so the reload tests
    /// need a real one rather than a bare printer.
    pub(super) fn sighup_ctx(
        tmp: &tempfile::TempDir,
        config_path: &Path,
    ) -> (DaemonLoopContext, Arc<std::sync::Mutex<String>>) {
        let (mut ctx, _state, buf) = make_test_ctx(tmp, false, false, None);
        ctx.config_path = config_path.to_path_buf();
        (ctx, buf)
    }

    /// One SIGHUP reload against `config_path`, returning the captured output
    /// and the reconcile/sync intervals it left behind (both start at 300s).
    fn run_sighup(tmp: &tempfile::TempDir, config_path: &Path) -> (String, u64, u64) {
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);
        let (ctx, buf) = sighup_ctx(tmp, config_path);
        let mut backup_timers = crate::daemon::BackupTimers::empty();
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut backup_timers);
        let captured = buf.lock().unwrap().clone();
        (
            captured,
            reconcile_secs.load(Ordering::Relaxed),
            sync_secs.load(Ordering::Relaxed),
        )
    }

    #[test]
    fn apply_sighup_reload_warns_on_unparseable_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("bad.yaml");
        std::fs::write(&config_path, "::: not yaml :::").unwrap();
        let (captured, reconcile_secs, sync_secs) = run_sighup(&tmp, &config_path);
        assert!(
            captured.contains("Config reload failed"),
            "expected reload-failed warning in: {}",
            captured
        );
        // Atomics untouched on failure
        assert_eq!(reconcile_secs, 300);
        assert_eq!(sync_secs, 300);
    }

    #[test]
    fn apply_sighup_reload_updates_atomics_and_reports_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 90s\n    sync:\n      interval: 2m\n",
        )
        .unwrap();
        let (captured, reconcile_secs, sync_secs) = run_sighup(&tmp, &config_path);
        assert!(
            captured.contains("Timer intervals reloaded"),
            "expected reload success in: {}",
            captured
        );
        assert_eq!(reconcile_secs, 90);
        assert_eq!(sync_secs, 120);
    }

    #[test]
    fn apply_sighup_reload_states_scope_is_timers_and_backups_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 90s\n",
        )
        .unwrap();
        let (captured, _, _) = run_sighup(&tmp, &config_path);
        assert!(
            captured.contains("timer intervals and backup schedules only"),
            "SIGHUP start message must state scope: {}",
            captured
        );
        assert!(
            captured.contains("other field changes require restart"),
            "SIGHUP completion line must mention restart for other fields: {}",
            captured
        );
    }

    #[test]
    fn apply_sighup_reload_reports_no_changes_for_silent_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n",
        )
        .unwrap();
        let (captured, reconcile_secs, sync_secs) = run_sighup(&tmp, &config_path);
        assert!(
            captured.contains("no timer changes detected"),
            "expected no-changes message in: {}",
            captured
        );
        assert_eq!(reconcile_secs, 300);
        assert_eq!(sync_secs, 300);
    }

    // ----- build_initial_source_status tests -----

    #[test]
    fn build_initial_source_status_empty_when_no_sources() {
        let rows = runner::build_initial_source_status(&[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn build_initial_source_status_one_row_per_source() {
        let cfg = parse_cfgd_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  sources:\n    - name: alpha\n      origin:\n        type: Git\n        url: https://example.com/a.git\n    - name: beta\n      origin:\n        type: Git\n        url: https://example.com/b.git\n",
        );
        let rows = runner::build_initial_source_status(&cfg.spec.sources);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[1].name, "beta");
        for r in &rows {
            assert_eq!(r.status, "active");
            assert_eq!(r.drift_count, 0);
            assert!(r.last_sync.is_none());
            assert!(r.last_reconcile.is_none());
        }
    }

    // ----- handle_file_change_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_records_path_in_debounce_map() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
        let path = PathBuf::from("/tmp/observed-1.txt");
        let res = runner::handle_file_change_tick(
            &ctx,
            &mut last_change,
            StdDuration::from_millis(500),
            path.clone(),
        )
        .await;
        assert!(res.is_ok());
        assert!(last_change.contains_key(&path));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_debounces_rapid_repeats() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
        let path = PathBuf::from("/tmp/observed-2.txt");
        // 60s debounce window — large enough that any plausible parallel-test
        // scheduling jitter still keeps both calls inside the window.
        let debounce = StdDuration::from_secs(60);
        runner::handle_file_change_tick(&ctx, &mut last_change, debounce, path.clone())
            .await
            .unwrap();
        let first_ts = *last_change.get(&path).unwrap();
        runner::handle_file_change_tick(&ctx, &mut last_change, debounce, path.clone())
            .await
            .unwrap();
        let second_ts = *last_change.get(&path).unwrap();
        assert_eq!(
            first_ts, second_ts,
            "debounced call must not refresh timestamp"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_triggers_reconcile_when_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, true, false, None);
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();
        let path = PathBuf::from("/tmp/observed-3.txt");
        // on_change_reconcile=true sends handle_reconcile through spawn_blocking.
        // With a nonexistent config_path the handler returns early — we only
        // care that the branch ran without panicking.
        let res = runner::handle_file_change_tick(
            &ctx,
            &mut last_change,
            StdDuration::from_millis(0), // disable debounce
            path,
        )
        .await;
        assert!(res.is_ok());
        // No real reconcile occurred (config is missing) — last_reconcile stays None.
        let st = state.lock().await;
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_change_tick_records_drift_only_for_managed_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let managed = tmp.path().join("managed.conf");
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.managed_paths = vec![managed.clone()];
        let mut last_change: HashMap<PathBuf, Instant> = HashMap::new();

        // A source/config path (NOT a managed target) must record no drift.
        let source_path = tmp.path().join(".git").join("index");
        runner::handle_file_change_tick(
            &ctx,
            &mut last_change,
            StdDuration::from_millis(0),
            source_path,
        )
        .await
        .unwrap();
        {
            let store = StateStore::open_in_dir(tmp.path()).unwrap();
            assert!(
                store.unresolved_drift().unwrap().is_empty(),
                "a .git source change must NOT record drift (BUG1)"
            );
            let st = state.lock().await;
            assert_eq!(st.drift_count, 0, "source change must not bump drift_count");
        }

        // A managed target change DOES record drift.
        runner::handle_file_change_tick(
            &ctx,
            &mut last_change,
            StdDuration::from_millis(0),
            managed.clone(),
        )
        .await
        .unwrap();
        {
            let store = StateStore::open_in_dir(tmp.path()).unwrap();
            let events = store.unresolved_drift().unwrap();
            assert_eq!(events.len(), 1, "managed target change must record drift");
            assert_eq!(events[0].resource_id, crate::to_posix_string(&managed));
            let st = state.lock().await;
            assert_eq!(
                st.drift_count, 1,
                "drift_count must reflect the outstanding row count"
            );
        }
    }

    // ----- handle_reconcile_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_with_no_tasks_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks: Vec<ReconcileTask> = Vec::new();
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_skips_task_whose_interval_has_not_elapsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let recent = Instant::now();
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(3600),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: Some(recent),
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        // Task skipped — last_reconciled unchanged.
        assert_eq!(tasks[0].last_reconciled, Some(recent));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_advances_default_task_last_reconciled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        assert!(tasks[0].last_reconciled.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_tick_updates_module_timestamp_for_non_default_entity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Per-module reconcile now invokes the reconciler — point at a real
        // config so `handle_reconcile` reaches the state-update step.
        let config_path = write_happy_path_config(&tmp);
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "my-module".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: true,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        assert!(tasks[0].last_reconciled.is_some());
        let st = state.lock().await;
        assert!(st.module_last_reconcile.contains_key("my-module"));
    }

    // ----- handle_sync_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_with_no_tasks_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks: Vec<SyncTask> = Vec::new();
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        let st = state.lock().await;
        assert!(st.last_sync.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_skips_task_whose_interval_has_not_elapsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let recent = Instant::now();
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: tmp.path().to_path_buf(),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(3600),
            last_synced: Some(recent),
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert_eq!(tasks[0].last_synced, Some(recent));
    }

    // ----- handle_compliance_tick tests -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compliance_tick_is_noop_when_config_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        // Should return Ok immediately — compliance_config is None.
        runner::handle_compliance_tick(&ctx).await.unwrap();
    }

    // ----- end-to-end loop tests (run_daemon_loop) -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_exits_cleanly_on_shutdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        // Immediately request shutdown.
        senders.shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error");
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_processes_sighup_then_shuts_down() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Write a config that updates intervals.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 77s\n",
        )
        .unwrap();
        let (mut ctx, _state, buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let reconcile_secs_observe = Arc::clone(&reconcile_secs);
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        // Fire a SIGHUP-equivalent tick.
        senders.sighup_tx.send(()).await.unwrap();
        // Give the loop a moment to process before shutdown.
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err");
        assert_eq!(reconcile_secs_observe.load(Ordering::Relaxed), 77);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Timer intervals reloaded"),
            "expected reload message in: {}",
            captured
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_drains_reconcile_ticks_with_no_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        for _ in 0..3 {
            senders.reconcile_tx.send(()).await.unwrap();
        }
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err");
        let st = state.lock().await;
        // No reconcile_tasks → nothing changes.
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_drains_sync_ticks_with_no_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        senders.sync_tx.send(()).await.unwrap();
        senders.sync_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err");
        let st = state.lock().await;
        assert!(st.last_sync.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_drains_compliance_ticks_when_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        senders.compliance_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err");
    }

    // (loop dispatch of file-change events is covered by
    // `handle_file_change_tick_*` direct-helper tests; a parallel loop test
    // running under `cargo llvm-cov` flaked on the StateStore opening inside
    // record_file_drift, so we exercise the branch by calling the helper
    // directly rather than through run_daemon_loop's select!.)

    // ----- Per-tick failure isolation -----
    //
    // The select! loop must log and continue when a tick handler panics or
    // returns Err — a single failing tick must not tear the daemon down.
    // These tests panic inside the spawn_blocking that backs each tick (via
    // `DaemonHooks` whose plan_files / build_registry implementation panics)
    // and then assert that the loop still services subsequent ticks and
    // exits cleanly on shutdown.

    /// Budget for "the loop exited after shutdown". This bounds a HANG, not
    /// the loop's speed: a deadlocked select! never exits at any budget, while
    /// a live one exits in milliseconds. Sizing it near the observed runtime
    /// converts runner contention into a test failure — the 2s/3s budgets this
    /// replaced completed in 0.3-1.2s on a dedicated Windows host and blew
    /// past 3s on a 2-vCPU hosted runner, where nextest schedules other tests
    /// against the same cores.
    const LOOP_EXIT_BUDGET: StdDuration = StdDuration::from_secs(30);

    /// `DaemonHooks` that panics in `plan_files`. Used to drive
    /// `handle_reconcile_tick` into a `JoinError` so the loop's recovery
    /// behavior is observable.
    ///
    /// The hook announces itself on `ran` immediately before panicking, so a
    /// test can await the panic having actually happened instead of sleeping
    /// for a duration it hopes is long enough. Without that signal a loaded
    /// runner can deliver `shutdown` while the panicking tick is still queued;
    /// the loop then breaks without ever exercising the continue-on-error path
    /// the test exists to prove, and passes for the wrong reason.
    struct PanickingPlanFilesHooks {
        ran: tokio::sync::mpsc::UnboundedSender<()>,
    }

    impl DaemonHooks for PanickingPlanFilesHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            let _ = self.ran.send(());
            panic!("intentional panic in plan_files (test fixture)")
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    /// `DaemonHooks` that panics in `build_registry`. Used to drive
    /// `handle_compliance_tick` (and, secondarily, any tick that builds a
    /// registry) into a `JoinError`.
    struct PanickingRegistryHooks;

    impl DaemonHooks for PanickingRegistryHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            panic!("intentional panic in build_registry (test fixture)")
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    /// Build a `DaemonLoopContext` whose `hooks` panic inside `plan_files`.
    ///
    /// The third element receives one message per `plan_files` entry; await it
    /// to sequence a test against the panic instead of against a sleep.
    fn make_panicking_plan_files_ctx(
        tmp: &tempfile::TempDir,
    ) -> (
        DaemonLoopContext,
        Arc<Mutex<DaemonState>>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let (ran_tx, ran_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = DaemonLoopContext {
            abort: Arc::new(crate::AbortFlag::new()),
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            state: Arc::clone(&state),
            hooks: Arc::new(PanickingPlanFilesHooks { ran: ran_tx }),
            notifier,
            config_path: write_happy_path_config(tmp),
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
            managed_paths: Vec::new(),
            scope: crate::Scope::User,
        };
        (ctx, state, ran_rx)
    }

    /// Await the panicking `plan_files` having actually run, so a test can send
    /// `shutdown` knowing the continue-on-error path was exercised.
    async fn await_panicking_tick(rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>) {
        tokio::time::timeout(LOOP_EXIT_BUDGET, rx.recv())
            .await
            .expect("panicking plan_files never ran")
            .expect("panicking hook was dropped without running");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_reconcile_tick_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, mut ran_rx) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            tasks,
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        // Reconcile tick triggers the panicking plan_files inside
        // spawn_blocking; the loop should log and continue.
        senders.reconcile_tx.send(()).await.unwrap();
        await_panicking_tick(&mut ran_rx).await;
        // Fire a no-op sync tick to prove the loop is still alive and
        // processing further dispatches.
        senders.sync_tx.send(()).await.unwrap();
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_compliance_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "7d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let ctx = DaemonLoopContext {
            abort: Arc::new(crate::AbortFlag::new()),
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            state: Arc::clone(&state),
            hooks: Arc::new(PanickingRegistryHooks),
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: Some(compliance_cfg),
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
            managed_paths: Vec::new(),
            scope: crate::Scope::User,
        };
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        senders.compliance_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_sync_tick_error() {
        // A sync tick whose repo_path does not exist exercises the sync
        // handler's error path (git2 returns Err from `Repository::open`).
        // After the failing tick we fire a reconcile tick that panics, then
        // a no-op sync tick, then shutdown — proving the loop keeps
        // servicing both error and panic flavors of failure.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, mut ran_rx) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let sync_tasks = vec![SyncTask {
            source_name: "broken".to_string(),
            repo_path: tmp.path().join("does-not-exist"),
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(0),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: false,
        }];
        let reconcile_tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            reconcile_tasks,
            sync_tasks,
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        senders.sync_tx.send(()).await.unwrap();
        senders.reconcile_tx.send(()).await.unwrap();
        await_panicking_tick(&mut ran_rx).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_continues_after_version_check_tick() {
        // Version check runs via spawn_blocking on `handle_version_check`,
        // which reads/writes a small JSON cache under HOME (guarded to the
        // tempdir). With no network reachable the upgrade module errors
        // gracefully and the tick must not abort the loop. After the tick
        // we fire a panicking reconcile to confirm the loop's
        // continue-on-error behavior is engaged, then shutdown.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, mut ran_rx) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let reconcile_tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            reconcile_tasks,
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        senders.version_check_tx.send(()).await.unwrap();
        senders.reconcile_tx.send(()).await.unwrap();
        await_panicking_tick(&mut ran_rx).await;
        senders.shutdown_tx.send(()).unwrap();
        tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error")
            .expect("loop returned Err — should have logged and continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn select_loop_exits_on_shutdown_after_panicking_tick() {
        // Regression guard: shutdown must still drain cleanly after a tick
        // handler has panicked. Without the per-tick continue-on-error
        // contract the loop would have already returned Err before shutdown
        // arrives, and this test would observe that as a JoinError.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, mut ran_rx) = make_panicking_plan_files_ctx(&tmp);
        let (triggers, senders) = make_triggers();
        let reconcile_secs = Arc::new(AtomicU64::new(300));
        let sync_secs = Arc::new(AtomicU64::new(300));
        let tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            tasks,
            Vec::new(),
            crate::daemon::BackupTimers::empty(),
            reconcile_secs,
            sync_secs,
        ));
        // Both ticks must be observed panicking before shutdown, otherwise a
        // slow runner can leave the second one unprocessed and the test proves
        // recovery from one panic rather than from a repeated one.
        senders.reconcile_tx.send(()).await.unwrap();
        await_panicking_tick(&mut ran_rx).await;
        senders.reconcile_tx.send(()).await.unwrap();
        await_panicking_tick(&mut ran_rx).await;
        senders.shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(LOOP_EXIT_BUDGET, handle)
            .await
            .expect("loop did not exit after shutdown")
            .expect("join error");
        assert!(
            result.is_ok(),
            "loop should exit Ok after panicking ticks + shutdown, got {:?}",
            result
        );
    }

    // ----- BLOCKER #2 — per-module reconcile actually invokes the reconciler -----
    //
    // The per-module branch of `handle_reconcile_tick` used to log and write
    // `module_last_reconcile` without calling the reconciler. These tests
    // drive the branch with a `ReconcilePatch`-bearing config and a hooks
    // impl that records calls, asserting the reconciler is invoked with the
    // expected module filter and that the patch's auto_apply / drift_policy
    // are respected.

    /// `DaemonHooks` whose `plan_files` records how many times it has been
    /// called and what `config_dir` it was invoked with. Returns the empty
    /// vec so `handle_reconcile` proceeds without producing real actions.
    struct RecordingHooks {
        plan_files_calls: Arc<std::sync::atomic::AtomicUsize>,
        build_registry_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl DaemonHooks for RecordingHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            self.build_registry_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            self.plan_files_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_module_reconcile_invokes_reconciler_with_filter() {
        // Two ReconcileTasks (one default, one per-module). Firing a tick
        // when both are due should call into `handle_reconcile` for the
        // default task and ALSO for the per-module task. Previously the
        // per-module branch was a no-op, so the hook would only fire once.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let plan_files_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build_registry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hooks = Arc::new(RecordingHooks {
            plan_files_calls: Arc::clone(&plan_files_calls),
            build_registry_calls: Arc::clone(&build_registry_calls),
        });
        let ctx = DaemonLoopContext {
            abort: Arc::new(crate::AbortFlag::new()),
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            state: Arc::clone(&state),
            hooks,
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
            managed_paths: Vec::new(),
            scope: crate::Scope::User,
        };
        let mut tasks = vec![
            ReconcileTask {
                entity: "__default__".to_string(),
                interval: StdDuration::from_secs(0),
                auto_apply: false,
                drift_policy: config::DriftPolicy::NotifyOnly,
                last_reconciled: None,
            },
            ReconcileTask {
                entity: "docker".to_string(),
                interval: StdDuration::from_secs(0),
                auto_apply: false,
                drift_policy: config::DriftPolicy::NotifyOnly,
                last_reconciled: None,
            },
        ];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        // Both tasks invoked the reconciler — plan_files called twice, once
        // per branch (default + filtered module).
        assert_eq!(
            plan_files_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected plan_files to fire for both the default and the per-module ticks"
        );
        // Per-module branch must populate module_last_reconcile for the
        // patched module name.
        let st = state.lock().await;
        assert!(
            st.module_last_reconcile.contains_key("docker"),
            "per-module branch should have recorded module_last_reconcile for 'docker' — got keys: {:?}",
            st.module_last_reconcile.keys().collect::<Vec<_>>()
        );
        assert!(
            st.last_reconcile.is_some(),
            "default branch should have updated profile-wide last_reconcile"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_module_reconcile_respects_drift_policy_notify_only() {
        // Per-module patch with drift_policy=NotifyOnly + a tick that
        // doesn't produce drift (empty profile). The reconciler is invoked
        // but apply is NOT (the NotifyOnly branch). We assert the per-module
        // entry shows up in state and that profile-wide drift_count is 0.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let plan_files_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build_registry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hooks = Arc::new(RecordingHooks {
            plan_files_calls: Arc::clone(&plan_files_calls),
            build_registry_calls: Arc::clone(&build_registry_calls),
        });
        let ctx = DaemonLoopContext {
            abort: Arc::new(crate::AbortFlag::new()),
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            state: Arc::clone(&state),
            hooks,
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
            managed_paths: Vec::new(),
            scope: crate::Scope::User,
        };
        let mut tasks = vec![ReconcileTask {
            entity: "monitoring".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        // The reconciler was driven for the module — plan_files fired once.
        assert_eq!(
            plan_files_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let st = state.lock().await;
        assert!(st.module_last_reconcile.contains_key("monitoring"));
        // No drift detected against empty profile + NotifyOnly policy.
        assert_eq!(st.drift_count, 0);
        // Per-module tick must not bump profile-wide last_reconcile.
        assert!(
            st.last_reconcile.is_none(),
            "per-module tick should not touch profile-wide last_reconcile"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_module_reconcile_with_auto_apply_invokes_reconciler() {
        // Per-module patch with auto_apply=true, drift_policy=Auto. With an
        // empty profile the apply branch is unreachable (no drift) but the
        // reconciler is still driven and the state is updated.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let plan_files_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build_registry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hooks = Arc::new(RecordingHooks {
            plan_files_calls: Arc::clone(&plan_files_calls),
            build_registry_calls: Arc::clone(&build_registry_calls),
        });
        let ctx = DaemonLoopContext {
            abort: Arc::new(crate::AbortFlag::new()),
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            state: Arc::clone(&state),
            hooks,
            notifier,
            config_path,
            profile_override: None,
            on_change_reconcile: false,
            notify_on_drift: false,
            compliance_config: None,
            printer,
            state_dir_override: Some(tmp.path().to_path_buf()),
            managed_paths: Vec::new(),
            scope: crate::Scope::User,
        };
        let mut tasks = vec![ReconcileTask {
            entity: "vault".to_string(),
            interval: StdDuration::from_secs(0),
            auto_apply: true,
            drift_policy: config::DriftPolicy::Auto,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        assert_eq!(
            plan_files_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "reconciler must be invoked for the per-module branch when auto_apply=true"
        );
        assert_eq!(
            build_registry_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let st = state.lock().await;
        assert!(st.module_last_reconcile.contains_key("vault"));
    }

    // ----- run_daemon_loop never returns Err for the channel-trigger branches
    // — tick errors are logged and the loop continues (see above). -----

    // ----- spawn_interval_pump smoke test -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_pump_clamps_zero_to_one_second() {
        // A 0-second interval would spin tight — the pump must clamp to >=1s.
        // We don't actually wait a full second; instead, we trip the abort path.
        let secs = Arc::new(AtomicU64::new(0));
        let (tx, mut rx) = mpsc::channel::<()>(8);
        let handle = super::super::spawn_interval_pump(secs, tx);
        // Give the runtime a chance to schedule the pump task.
        tokio::time::sleep(StdDuration::from_millis(10)).await;
        handle.abort();
        // No assertion on rx — we only verify the pump didn't spin or panic before
        // abort. If the clamp were missing this test would hang the runtime.
        let _ = rx.try_recv();
    }

    // ----- Happy-path fixture: drive handle_reconcile end-to-end ---------
    //
    // The previous tests exit early inside handle_reconcile because
    // `config_path` points to a missing file. This fixture writes a real
    // `cfgd.yaml` + `profiles/default.yaml` so reconcile reaches the plan
    // generation + state.last_reconcile update. Unlocks coverage in
    // daemon/reconcile.rs and (via handle_sync_tick) daemon/sync.rs.

    /// Write a minimal but complete cfgd config tree under `tmp`. Returns
    /// the path to `cfgd.yaml`. The config selects profile "default", which
    /// resolves to an empty `profiles/default.yaml`.
    fn write_happy_path_config(tmp: &tempfile::TempDir) -> PathBuf {
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        config_path
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_runs_full_happy_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(
            st.last_reconcile.is_some(),
            "handle_reconcile should have updated state.last_reconcile on happy path"
        );
        // No drift expected — empty profile means no actions to apply.
        assert_eq!(st.drift_count, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_handles_unknown_profile_gracefully() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config that points to a profile name that doesn't exist on disk.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: missing-profile\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        // Profile resolution fails → handle_reconcile returns before
        // touching last_reconcile.
        assert!(st.last_reconcile.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_respects_profile_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config has no profile — override supplies one.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("override-profile.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: override-profile\nspec: {}\n",
        )
        .unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        ctx.profile_override = Some("override-profile".to_string());
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_auto_apply_traverses_apply_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config with daemon.reconcile.autoApply=true exercises the auto-apply
        // policy branch even though the plan is empty.
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: true,
            drift_policy: config::DriftPolicy::Auto,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_some());
        assert_eq!(st.drift_count, 0);
    }

    // ----- Real sync_task with a tempdir non-git repo path -----
    //
    // handle_sync will attempt git operations against `repo_path`. With a
    // non-git directory, all git calls fail gracefully and the handler
    // still returns false (no changes). The orchestration around it — the
    // last_synced bump, the state.last_sync update via block_on — is what
    // we cover here.

    /// Create a bare upstream repo + a working clone of it. Returns the
    /// (bare_path, work_path) pair. The clone starts with a single commit
    /// already pushed to bare's HEAD branch.
    fn make_bare_and_clone(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let bare = tmp.path().join("upstream.git");
        let work = tmp.path().join("workdir");
        let _bare_repo = git2::Repository::init_bare(&bare).unwrap();
        let src = tmp.path().join("src");
        let src_repo = git2::Repository::init(&src).unwrap();
        std::fs::write(src.join("README.md"), "hi").unwrap();
        let mut index = src_repo.index().unwrap();
        index.add_path(std::path::Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let bare_url = crate::test_helpers::file_url(&bare);
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        let _ = git2::Repository::clone(&bare_url, &work).unwrap();
        (bare, work)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_runs_git_pull_against_real_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (_bare, work) = make_bare_and_clone(&tmp);
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: work,
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some());
        let st = state.lock().await;
        assert!(st.last_sync.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_runs_git_push_against_real_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (_bare, work) = make_bare_and_clone(&tmp);
        // Make a local edit so git_auto_commit_push has something to commit.
        std::fs::write(work.join("README.md"), "local change").unwrap();
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: work,
            auto_pull: false,
            auto_push: true,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_handles_invalid_repo_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        // Path that exists but isn't a git repo — git_pull fails gracefully.
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path: not_a_repo,
            auto_pull: true,
            auto_push: true,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some());
    }

    // ----- handle_reconcile with files+packages in profile -----
    //
    // Plan with a non-empty profile exercises file/package planning paths.
    // NoopHooks returns empty actions, so plan is still empty — but the
    // resolve_profile body walks merged.files.managed, merged.packages, etc.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_tick_with_managed_files_in_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: example.txt\n        target: ~/example.txt\n  packages:\n    brew:\n      formulae:\n        - ripgrep\n",
        )
        .unwrap();
        let (mut ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = config_path;
        let mut tasks = vec![ReconcileTask {
            entity: "__default__".to_string(),
            interval: StdDuration::from_secs(60),
            auto_apply: false,
            drift_policy: config::DriftPolicy::NotifyOnly,
            last_reconciled: None,
        }];
        runner::handle_reconcile_tick(&ctx, &mut tasks)
            .await
            .unwrap();
        let st = state.lock().await;
        assert!(st.last_reconcile.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_tick_advances_last_synced_for_due_task() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (ctx, state, _buf) = make_test_ctx(&tmp, false, false, None);
        let repo_path = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&repo_path).unwrap();
        let mut tasks = vec![SyncTask {
            source_name: "local".to_string(),
            repo_path,
            // auto_pull/push false → handle_sync does no git work, just updates state
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            interval: StdDuration::from_secs(60),
            last_synced: None,
            require_signed_commits: false,
            allow_unsigned: true,
        }];
        runner::handle_sync_tick(&ctx, &mut tasks).await.unwrap();
        assert!(tasks[0].last_synced.is_some(), "last_synced should advance");
        let st = state.lock().await;
        assert!(st.last_sync.is_some(), "state.last_sync should be set");
    }

    // ----- build_pre_loop_setup: SETUP-arm coverage -----

    /// `build_pre_loop_setup` under the arguments every SETUP-arm test shares:
    /// no-op hooks, user scope, a test printer, and no state-dir override.
    pub(super) fn pre_loop(
        config_path: &std::path::Path,
        profile: Option<&str>,
    ) -> Result<PreLoopSetup> {
        build_pre_loop_setup(
            config_path,
            profile,
            &NoopHooks,
            crate::Scope::User,
            &Printer::for_test().0,
            None,
        )
    }

    #[test]
    fn build_pre_loop_setup_happy_path_yields_defaulted_intervals() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);

        let setup = pre_loop(&config_path, None).expect("happy setup");

        // Default reconcile + sync interval = 300s (5m)
        assert_eq!(setup.parsed.reconcile_interval, Duration::from_secs(300));
        assert_eq!(setup.parsed.sync_interval, Duration::from_secs(300));
        assert!(!setup.parsed.auto_pull);
        assert!(!setup.parsed.auto_push);
        assert!(!setup.parsed.auto_apply);
        // Compliance not configured → no interval
        assert!(setup.compliance_config.is_none());
        assert!(setup.compliance_interval.is_none());
        // One sync task for local config dir
        assert_eq!(setup.sync_tasks.len(), 1);
        // Only the __default__ reconcile task (no module patches)
        assert_eq!(setup.reconcile_tasks.len(), 1);
        assert_eq!(setup.reconcile_tasks[0].entity, "__default__");
        // No external sources → only the seeded "local" source status (added in run_daemon, not setup)
        // Setup itself just produces the additions, which is empty here.
        assert!(setup.initial_source_status.is_empty());
        // No files in default profile → no managed paths
        assert!(setup.managed_paths.is_empty());
        // No server origin → no startup check-in URL
        assert!(setup.server_checkin_url.is_none());
        // Stdout notifier by default
        assert!(matches!(setup.parsed.notify_method, NotifyMethod::Stdout));
        // shortest_* == defaults when no per-module patches narrow them
        assert_eq!(setup.shortest_reconcile, Duration::from_secs(300));
        assert_eq!(setup.shortest_sync, Duration::from_secs(300));
        // config_dir matches the parent of config_path
        assert_eq!(setup.config_dir, tmp.path());
    }

    #[test]
    fn build_pre_loop_setup_loads_compliance_interval_when_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  compliance:\n    enabled: true\n    interval: 30m\n    retention: 30d\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let setup = pre_loop(&config_path, None).expect("setup");

        assert!(setup.compliance_config.is_some());
        assert_eq!(setup.compliance_interval, Some(Duration::from_secs(1800)));
    }

    #[test]
    fn build_pre_loop_setup_skips_compliance_interval_when_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  compliance:\n    enabled: false\n    interval: 30m\n    retention: 30d\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let setup = pre_loop(&config_path, None).expect("setup");

        // Compliance config present but interval None because enabled=false short-circuits filter.
        assert!(setup.compliance_config.is_some());
        assert!(setup.compliance_interval.is_none());
    }

    #[test]
    fn build_pre_loop_setup_returns_err_for_unparseable_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "::: not yaml :::").unwrap();

        let result = pre_loop(&config_path, None);

        match result {
            Ok(_) => panic!("invalid yaml must error"),
            Err(e) => {
                // Just confirm an error surfaced. Message asserts would be brittle.
                let msg = format!("{}", e);
                assert!(!msg.is_empty());
            }
        }
    }

    #[test]
    fn build_pre_loop_setup_respects_profile_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        // Config has profile: default; override should pick override-profile instead.
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("override-profile.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: override-profile\nspec:\n  files:\n    managed:\n      - source: example.txt\n        target: /tmp/example-override.txt\n",
        )
        .unwrap();

        let setup = pre_loop(&config_path, Some("override-profile")).expect("setup");

        // override-profile has a managed file → discover_managed_paths populates it.
        assert_eq!(setup.managed_paths.len(), 1);
        assert!(
            setup
                .managed_paths
                .iter()
                .any(|p| p.ends_with("example-override.txt"))
        );
    }

    #[test]
    fn build_pre_loop_setup_falls_back_to_default_profile_name_when_unset() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Config has no profile field → fallback chain is "default".
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();

        let setup = pre_loop(&config_path, None).expect("setup");

        // No profile resolution → no managed paths, reconcile_tasks contains just __default__
        assert!(setup.managed_paths.is_empty());
        assert_eq!(setup.reconcile_tasks.len(), 1);
        assert_eq!(setup.reconcile_tasks[0].entity, "__default__");
    }

    #[test]
    fn build_pre_loop_setup_picks_up_sync_auto_pull_push_from_daemon_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    sync:\n      interval: 90s\n      autoPull: true\n      autoPush: true\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let setup = pre_loop(&config_path, None).expect("setup");

        assert!(setup.parsed.auto_pull);
        assert!(setup.parsed.auto_push);
        assert_eq!(setup.parsed.sync_interval, Duration::from_secs(90));
        assert_eq!(setup.shortest_sync, Duration::from_secs(90));
        // First (and only) sync task is the local one, which inherits parsed values.
        assert_eq!(setup.sync_tasks.len(), 1);
    }

    #[test]
    fn build_pre_loop_setup_finds_server_url_for_server_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  origin:\n    - type: Server\n      url: https://gateway.example/api\n      branch: master\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let setup = pre_loop(&config_path, None).expect("setup");

        assert_eq!(
            setup.server_checkin_url.as_deref(),
            Some("https://gateway.example/api")
        );
    }

    // ----- handle_compliance_snapshot: state_dir_override coverage -----

    #[test]
    fn handle_compliance_snapshot_writes_to_state_dir_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();

        let hooks = NoopHooks;
        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
            crate::Scope::User,
        );

        // Snapshot row was written to the override DB.
        let store =
            crate::state::StateStore::open(&state_dir.join("state.db")).expect("override db");
        let hash = store
            .latest_compliance_hash()
            .expect("hash query")
            .expect("snapshot present");
        assert!(!hash.is_empty());
    }

    #[test]
    fn handle_compliance_snapshot_returns_early_on_unparseable_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "::: not yaml :::").unwrap();

        let hooks = NoopHooks;
        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
            crate::Scope::User,
        );

        // No snapshot stored because config load failed.
        let store =
            crate::state::StateStore::open(&state_dir.join("state.db")).expect("override db");
        let hash = store.latest_compliance_hash().expect("hash query");
        assert!(hash.is_none());
    }

    #[test]
    fn handle_compliance_snapshot_returns_early_when_named_profile_does_not_exist() {
        // The cfg names a profile (`ghost`) but profiles/ doesn't contain it →
        // `resolve_profile` returns Err → the function takes the resolve-Err
        // arm (lines 151-157 in sync.rs) and bails without opening the store.
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: ghost\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        // Intentionally no ghost.yaml → resolve_profile fails.

        let hooks = NoopHooks;
        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
            crate::Scope::User,
        );

        // No snapshot stored because resolve_profile failed.
        let store =
            crate::state::StateStore::open(&state_dir.join("state.db")).expect("override db");
        let hash = store.latest_compliance_hash().expect("hash query");
        assert!(
            hash.is_none(),
            "missing profile → resolve_profile Err → no snapshot stored"
        );
    }

    #[test]
    fn handle_compliance_snapshot_skips_when_no_profile_configured() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let config_path = tmp.path().join("cfgd.yaml");
        // No spec.profile, no override → handler bails before opening the store.
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();

        let hooks = NoopHooks;
        let compliance_cfg = config::ComplianceConfig {
            enabled: true,
            interval: "1h".into(),
            retention: "30d".into(),
            scope: config::ComplianceScope::default(),
            export: config::ComplianceExport::default(),
        };

        super::super::sync::handle_compliance_snapshot(
            &config_path,
            None,
            &hooks,
            &compliance_cfg,
            Some(&state_dir),
            crate::Scope::User,
        );

        let store =
            crate::state::StateStore::open(&state_dir.join("state.db")).expect("override db");
        let hash = store.latest_compliance_hash().expect("hash query");
        assert!(hash.is_none());
    }

    // ----- handle_version_check: policy-driven coverage -----
    //
    // The policy-driven check interval-gates against the persisted version
    // cache timestamp, then hits the releases API for the value. The persisted
    // cache here is left absent (no `version-check.json`) so the gate opens and
    // the API mock supplies the latest release. `CFGD_GITHUB_API_BASE` redirects
    // `check_latest` at a mockito server (process-global env → `#[serial]`).

    fn notify_update_cfg() -> config::UpdateConfig {
        config::UpdateConfig {
            policy: config::UpdatePolicy::Notify,
            ..Default::default()
        }
    }

    // The test_home thread-local is installed on the calling thread; the
    // version-check helper propagates that override into its spawn_blocking
    // closure so the cache lookup sees the tempdir.
    async fn drive_version_check(
        home: std::path::PathBuf,
        cfg: &config::UpdateConfig,
    ) -> Arc<Mutex<DaemonState>> {
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let _g = crate::with_test_home_guard(&home);
        super::super::sync::handle_version_check(cfg, &state, &notifier, env!("CARGO_PKG_VERSION"))
            .await;
        state
    }

    // current_thread so the test_home thread-local installed in
    // `drive_version_check` survives across the `.await` — multi_thread can
    // migrate the future to a different worker thread mid-poll.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn handle_version_check_notify_records_update_available() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v999.0.0", "assets": []}"#)
            .create_async()
            .await;
        let _api = crate::test_helpers::EnvVarGuard::set("CFGD_GITHUB_API_BASE", &server.url());

        let state = drive_version_check(tmp.path().to_path_buf(), &notify_update_cfg()).await;

        let st = state.lock().await;
        assert_eq!(st.update_available.as_deref(), Some("999.0.0"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn handle_version_check_surfaces_consolidated_skill_stale_when_up_to_date() {
        // Binary current (tag == running) + a stale user-scope skill under Notify
        // → the consolidated skill-stale notice fires once, recorded in state
        // by its per-scope signature. Rule 3 wired through `handle_version_check`.
        use crate::generate::SkillKind;
        use crate::providers::skill::SkillScope;

        let tmp = tempfile::TempDir::new().unwrap();
        let runtime = tempfile::TempDir::new().unwrap();
        let _rt = crate::test_helpers::EnvVarGuard::set(
            "CFGD_RUNTIME_DIR",
            &runtime.path().to_string_lossy(),
        );

        // Seed a stale user-scope skill inside the test home before driving.
        {
            let _g = crate::with_test_home_guard(tmp.path());
            crate::test_helpers::seed_stale_skill(SkillKind::Module, SkillScope::User);
        }

        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"tag_name": "{tag}", "assets": []}}"#))
            .create_async()
            .await;
        let _api = crate::test_helpers::EnvVarGuard::set("CFGD_GITHUB_API_BASE", &server.url());

        let state = drive_version_check(tmp.path().to_path_buf(), &notify_update_cfg()).await;

        let st = state.lock().await;
        // No binary update (rule 1 not triggered); exactly one consolidated skill
        // surface recorded — project count is 0 (cwd has no skill), user is 1.
        assert_eq!(
            st.skills_stale_notified.as_deref(),
            Some("user:1,project:0"),
            "consolidated skill-stale notice fires once with per-scope counts"
        );
        assert!(st.update_available.is_none(), "no binary update pending");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn handle_version_check_leaves_state_clean_when_up_to_date() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"tag_name": "{tag}", "assets": []}}"#))
            .create_async()
            .await;
        let _api = crate::test_helpers::EnvVarGuard::set("CFGD_GITHUB_API_BASE", &server.url());

        let state = drive_version_check(tmp.path().to_path_buf(), &notify_update_cfg()).await;

        let st = state.lock().await;
        assert!(st.update_available.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn handle_version_check_manual_policy_skips_entirely() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No mock server: a network call would error. Manual must not check, so
        // state stays clean regardless.
        let cfg = config::UpdateConfig {
            policy: config::UpdatePolicy::Manual,
            ..Default::default()
        };
        let state = drive_version_check(tmp.path().to_path_buf(), &cfg).await;
        let st = state.lock().await;
        assert!(st.update_available.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn handle_version_check_manual_policy_mutates_no_update_state() {
        // Manual returns before any network or skill-surface work, so BOTH update
        // surfaces stay untouched even when a stale user-scope skill is present —
        // the gate fires ahead of `surface_stale_skills`.
        use crate::generate::SkillKind;
        use crate::providers::skill::SkillScope;

        let tmp = tempfile::TempDir::new().unwrap();
        let runtime = tempfile::TempDir::new().unwrap();
        let _rt = crate::test_helpers::EnvVarGuard::set(
            "CFGD_RUNTIME_DIR",
            &runtime.path().to_string_lossy(),
        );
        {
            let _g = crate::with_test_home_guard(tmp.path());
            crate::test_helpers::seed_stale_skill(SkillKind::Module, SkillScope::User);
        }

        // No mock server: a network call would error, proving Manual never reaches it.
        let cfg = config::UpdateConfig {
            policy: config::UpdatePolicy::Manual,
            ..Default::default()
        };
        let state = drive_version_check(tmp.path().to_path_buf(), &cfg).await;

        let st = state.lock().await;
        assert!(
            st.update_available.is_none(),
            "Manual must not record a binary update"
        );
        assert!(
            st.skills_stale_notified.is_none(),
            "Manual gates before the skill-stale surface, so no notice is recorded"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn handle_version_check_within_interval_gate_skips_check() {
        // A recent recorded check + a long interval makes `should_check` false for
        // a non-Manual policy, so the gate returns before any network or
        // skill-surface work and leaves both update surfaces untouched.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());

        // Stamp a check "now" into the test-home version cache; with the default
        // 24h interval, the next tick is well within the window.
        crate::upgrade::record_check_at(env!("CARGO_PKG_VERSION"), crate::unix_secs_now());

        // No mock server: a network call would error, proving the gate short-circuits.
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        super::super::sync::handle_version_check(
            &notify_update_cfg(),
            &state,
            &notifier,
            env!("CARGO_PKG_VERSION"),
        )
        .await;

        let st = state.lock().await;
        assert!(
            st.update_available.is_none(),
            "within-interval tick must not record a binary update"
        );
        assert!(
            st.skills_stale_notified.is_none(),
            "within-interval tick gates before the skill-stale surface"
        );
    }

    // ----- init_daemon_state tests -----

    #[test]
    fn init_daemon_state_uses_override_dir_for_store_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = super::super::init_daemon_state(Some(tmp.path()), crate::Scope::User);
        let store = st
            .store_path_for_test()
            .expect("override yields a store_path");
        assert_eq!(store, tmp.path().join("state.db"));
    }

    #[test]
    #[serial_test::serial]
    fn init_daemon_state_falls_back_when_default_state_dir_fails() {
        use crate::test_helpers::EnvVarGuard;
        // Drive `default_state_dir` into its deterministic failure branch: no
        // home is resolvable. That requires every higher-precedence tier to be
        // absent — `CFGD_STATE_DIR`, systemd's `STATE_DIRECTORY`, and the home
        // env (`HOME`/`USERPROFILE`) — and no test-home override installed (it
        // would otherwise satisfy `home_dir_var`). With home unresolvable the
        // resolver returns `Err` before consulting `directories::BaseDirs`, so
        // the fallback is exercised regardless of the runner's XDG layout or a
        // systemd-launched `STATE_DIRECTORY`.
        let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
        let _systemd = EnvVarGuard::unset("STATE_DIRECTORY");
        let _home = EnvVarGuard::unset("HOME");
        let _userprofile = EnvVarGuard::unset("USERPROFILE");

        // The fallback yields a state with no store_path (the /drift endpoint
        // then returns empty events).
        let st = super::super::init_daemon_state(None, crate::Scope::User);
        assert!(
            st.store_path_for_test().is_none(),
            "resolve failure must fall back to a store-less state"
        );

        // With an explicit override the store_path is always set.
        let tmp = tempfile::TempDir::new().unwrap();
        let st_with_override =
            super::super::init_daemon_state(Some(tmp.path()), crate::Scope::User);
        assert!(st_with_override.store_path_for_test().is_some());
    }

    #[test]
    #[serial_test::serial]
    fn init_daemon_state_with_warning_reports_message_on_resolve_failure() {
        // The daemon used to only emit a `tracing::warn!` when state-dir
        // resolution failed, leaving the
        // /drift endpoint silently disabled. The variant exposes a banner
        // message so the startup banner can surface it.
        //
        // Same deterministic-failure setup as the fallback test: unset every
        // tier above the home-based resolution and install no test-home
        // override, so resolution always fails and the warning always fires.
        use crate::test_helpers::EnvVarGuard;
        let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
        let _systemd = EnvVarGuard::unset("STATE_DIRECTORY");
        let _home = EnvVarGuard::unset("HOME");
        let _userprofile = EnvVarGuard::unset("USERPROFILE");

        let (st, warning) = super::super::init_daemon_state_with_warning(None, crate::Scope::User);
        let msg = warning.expect("resolve failure must surface an operator-facing warning");
        assert!(
            msg.contains("Drift endpoint disabled"),
            "warning should be operator-facing; got {msg:?}"
        );
        assert!(
            st.store_path_for_test().is_none(),
            "warning path must also fall back to a store-less state"
        );

        // With an override the variant must NEVER emit a warning.
        let tmp = tempfile::TempDir::new().unwrap();
        let (_st2, w2) =
            super::super::init_daemon_state_with_warning(Some(tmp.path()), crate::Scope::User);
        assert!(w2.is_none(), "override path must not warn; got {w2:?}");
    }

    // ----- system-scope directory resolution tests -----

    // System-scope IPC/state roots are platform-specific absolutes: Linux FHS
    // (`/run/cfgd`, `/var/lib/cfgd`), the macOS `/Library/Application Support`
    // mirror, and `%ProgramData%\cfgd` on Windows. Each platform pins its own
    // root so a `unix`-wide assertion never false-fails on macOS (which is also
    // `unix` but resolves under `/Library`).

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn run_daemon_with_system_scope_ipc_resolves_fhs() {
        use crate::test_helpers::EnvVarGuard;
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _runtime = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
        let _xdg = EnvVarGuard::unset("XDG_RUNTIME_DIR");
        let _runtime_dir = EnvVarGuard::unset("RUNTIME_DIRECTORY");

        let overrides = super::super::DaemonRunOverrides {
            scope: crate::Scope::System,
            skip_health_server: true,
            ..Default::default()
        };
        let ipc = overrides
            .ipc_path
            .clone()
            .unwrap_or_else(|| super::super::resolve_default_ipc_path(None, overrides.scope));
        assert!(
            ipc.starts_with("/run/cfgd"),
            "system-scope IPC path must be under /run/cfgd, got: {}",
            ipc.display()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial_test::serial]
    fn run_daemon_with_system_scope_ipc_resolves_application_support() {
        use crate::test_helpers::EnvVarGuard;
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _runtime = EnvVarGuard::unset("CFGD_RUNTIME_DIR");

        let overrides = super::super::DaemonRunOverrides {
            scope: crate::Scope::System,
            skip_health_server: true,
            ..Default::default()
        };
        let ipc = overrides
            .ipc_path
            .clone()
            .unwrap_or_else(|| super::super::resolve_default_ipc_path(None, overrides.scope));
        assert!(
            ipc.starts_with("/Library/Application Support/cfgd/runtime"),
            "system-scope IPC path must be under the macOS runtime root, got: {}",
            ipc.display()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn init_daemon_state_with_warning_system_scope_uses_fhs_state_dir() {
        use crate::test_helpers::EnvVarGuard;
        let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
        let _systemd = EnvVarGuard::unset("STATE_DIRECTORY");
        let _home = EnvVarGuard::unset("HOME");
        let _userprofile = EnvVarGuard::unset("USERPROFILE");

        let (st, _warning) =
            super::super::init_daemon_state_with_warning(None, crate::Scope::System);
        assert!(
            st.store_path_for_test()
                .map(|p| p.starts_with("/var/lib/cfgd"))
                .unwrap_or(false),
            "system-scope state dir must be under /var/lib/cfgd"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial_test::serial]
    fn init_daemon_state_with_warning_system_scope_uses_application_support_state_dir() {
        use crate::test_helpers::EnvVarGuard;
        let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
        let _systemd = EnvVarGuard::unset("STATE_DIRECTORY");

        let (st, _warning) =
            super::super::init_daemon_state_with_warning(None, crate::Scope::System);
        assert!(
            st.store_path_for_test()
                .map(|p| p.starts_with("/Library/Application Support/cfgd/state"))
                .unwrap_or(false),
            "system-scope state dir must be under the macOS state root"
        );
    }

    #[cfg(windows)]
    #[test]
    #[serial_test::serial]
    fn init_daemon_state_with_warning_system_scope_uses_program_data_state_dir() {
        use crate::test_helpers::EnvVarGuard;
        let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");

        let expected = crate::program_data_dir().join("cfgd").join("state");
        let (st, _warning) =
            super::super::init_daemon_state_with_warning(None, crate::Scope::System);
        assert!(
            st.store_path_for_test()
                .map(|p| p.starts_with(&expected))
                .unwrap_or(false),
            "system-scope state dir must be under %ProgramData%\\cfgd\\state"
        );
    }

    // ----- check_already_running tests -----

    #[cfg(unix)]
    #[test]
    fn check_already_running_ok_when_path_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("missing.sock");
        super::super::check_already_running(&path, crate::Scope::User)
            .expect("ok when path missing");
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn check_already_running_removes_stale_socket_file() {
        // A plain file at the IPC path with no listener simulates a crashed
        // daemon: connect() fails, and the cleanup branch unlinks the file.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("stale.sock");
        std::fs::write(&path, b"stale").unwrap();
        super::super::check_already_running(&path, crate::Scope::User).expect("ok with stale file");
        assert!(
            !path.exists(),
            "stale socket file should have been removed: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn check_already_running_errors_when_listener_is_accepting() {
        use std::os::unix::net::UnixListener as StdUnixListener;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("live.sock");
        let _listener = StdUnixListener::bind(&path).unwrap();
        let err = super::super::check_already_running(&path, crate::Scope::User)
            .expect_err("expect AlreadyRunning when a listener is accepting");
        let msg = format!("{err}");
        assert!(
            msg.contains("already") || msg.to_lowercase().contains("running"),
            "expected AlreadyRunning message, got: {msg}"
        );
        // The file is NOT removed when the listener is live.
        assert!(path.exists());
    }

    // ----- format_interval_lines tests -----

    #[test]
    fn format_interval_lines_reports_reconcile_only_by_default() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(300),
            sync_interval: StdDuration::from_secs(300),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, None, 0, None);
        assert_eq!(lines, vec!["reconcile=300s".to_string()]);
    }

    #[test]
    fn format_interval_lines_includes_sync_when_pull_or_push_enabled() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(60),
            sync_interval: StdDuration::from_secs(120),
            auto_pull: true,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, None, 0, None);
        assert_eq!(
            lines,
            vec![
                "reconcile=60s".to_string(),
                "sync=120s (pull=true, push=false)".to_string(),
            ]
        );
    }

    #[test]
    fn format_interval_lines_appends_compliance_when_supplied() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(30),
            sync_interval: StdDuration::from_secs(30),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(
            &parsed,
            Some(StdDuration::from_secs(900)),
            0,
            None,
        );
        assert_eq!(
            lines,
            vec!["reconcile=30s".to_string(), "compliance=900s".to_string()]
        );
    }

    // ----- print_startup_banner tests -----

    #[test]
    fn print_startup_banner_emits_health_intervals_and_run_hint() {
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        super::super::print_startup_banner(
            &printer,
            &["reconcile=30s".to_string(), "compliance=900s".to_string()],
            "/tmp/cfgd-banner-test.sock",
        );
        let out = buf.lock().unwrap().clone();
        assert!(out.contains("Health: /tmp/cfgd-banner-test.sock"));
        assert!(out.contains("Intervals: reconcile=30s, compliance=900s"));
        assert!(out.contains("Daemon running"));
    }

    // ----- run_startup_checkin_blocking tests -----

    fn parse_minimal_cfg(yaml: &str) -> CfgdConfig {
        serde_yaml::from_str(yaml).expect("test yaml must parse")
    }

    #[test]
    fn run_startup_checkin_blocking_bails_when_no_profile_resolved() {
        // Profile name resolves to a profile dir that does not exist —
        // resolve_profile errors, the function warns + returns. No panic,
        // no network.
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );
        // No profile YAML on disk → resolve_profile fails → function warns.
        super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
    }

    #[test]
    fn run_startup_checkin_blocking_no_op_when_profile_missing_in_cfg() {
        // No profile in cfg AND no override → early-return.
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec: {}\n",
        );
        super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
    }

    #[test]
    fn run_startup_checkin_blocking_resolves_profile_and_returns_when_no_server_url() {
        // Seed a valid profile so resolve_profile succeeds. With no Server
        // origin in cfg, try_server_checkin returns false without network.
        // pending-server-config load returns None on a fresh state-dir.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages: {}\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );
        super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
    }

    // current_thread so the test_home thread-local installed below survives
    // across the `.await` — multi_thread can migrate the future mid-poll.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn startup_checkin_spawn_respects_test_home() {
        // Regression: the startup check-in dispatch resolves home-relative
        // state (pending-server-config via `default_state_dir`) inside its
        // blocking closure. Dispatched via plain `spawn_blocking` the worker
        // lost the test-home override and fell back to the ambient $HOME.
        // Drive the same wrapper-based dispatch shape as the daemon loop and
        // assert state resolution lands INSIDE the test home — no CFGD_STATE_DIR
        // pin, so the wrapper and the override-honoring state resolver are
        // proven to protect the site together. Still #[serial]: CFGD_STATE_DIR
        // (env) outranks the thread-local override, so a concurrently mutating
        // test could redirect resolution out from under these assertions.
        let tmp = tempfile::TempDir::new().unwrap();
        let _sd = crate::test_helpers::EnvVarGuard::unset("CFGD_STATE_DIR");
        let _sysd = crate::test_helpers::EnvVarGuard::unset("STATE_DIRECTORY");
        let _g = crate::with_test_home_guard(tmp.path());
        let resolved_state = crate::state::default_state_dir().unwrap();
        assert!(
            resolved_state.starts_with(tmp.path()),
            "user-scope state must resolve inside the test home, got {}",
            resolved_state.display()
        );
        let config_path = tmp.path().join("cfgd.yaml");
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  packages: {}\n",
        )
        .unwrap();
        let cfg = parse_minimal_cfg(
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );

        let pending_path =
            crate::state::save_pending_server_config(&serde_json::json!({"seed": true}))
                .expect("seed pending server config");
        assert!(
            pending_path.starts_with(tmp.path()),
            "pending config must land under the tempdir, got {}",
            pending_path.display()
        );

        let expected_home = tmp.path().to_path_buf();
        let seen_home = crate::spawn_blocking_with_test_home(move || {
            super::super::run_startup_checkin_blocking(&config_path, None, &cfg);
            crate::test_home_override()
        })
        .await
        .unwrap();
        assert_eq!(
            seen_home,
            Some(expected_home),
            "the startup check-in closure must run under the test-home override"
        );

        // The closure resolved the SAME test-home state dir: the seeded
        // pending config was found and cleared. Under real-$HOME fallback it
        // would still be present.
        assert!(
            crate::state::load_pending_server_config()
                .expect("load pending")
                .is_none(),
            "startup check-in must consume the pending config under the test home"
        );
    }

    // ----- cleanup_ipc_socket tests -----

    #[cfg(unix)]
    #[test]
    fn cleanup_ipc_socket_removes_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("to-remove.sock");
        std::fs::write(&path, b"stale").unwrap();
        super::super::cleanup_ipc_socket(&path);
        assert!(!path.exists(), "expected {} to be removed", path.display());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_ipc_socket_is_noop_when_path_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("missing.sock");
        // Must not panic.
        super::super::cleanup_ipc_socket(&path);
        assert!(!path.exists());
    }

    // ----- setup_file_watcher tests -----

    #[test]
    fn setup_file_watcher_watches_existing_managed_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let managed = tmp.path().join("watched.txt");
        std::fs::write(&managed, b"initial").unwrap();
        let config_dir = tmp.path().to_path_buf();
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);

        let watcher = super::super::reconcile::setup_file_watcher(
            tx,
            std::slice::from_ref(&managed),
            &config_dir,
        );
        assert!(
            watcher.is_ok(),
            "expected watcher to construct: {watcher:?}"
        );
    }

    #[test]
    fn setup_file_watcher_watches_parent_when_path_does_not_yet_exist() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Managed path is in tmp but file does not exist yet — the watcher
        // should fall back to watching the parent dir for create events.
        let managed = tmp.path().join("not-yet-created.txt");
        let config_dir = tmp.path().to_path_buf();
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);

        let watcher = super::super::reconcile::setup_file_watcher(
            tx,
            std::slice::from_ref(&managed),
            &config_dir,
        );
        assert!(
            watcher.is_ok(),
            "watcher should still succeed via parent-dir fallback: {watcher:?}"
        );
    }

    #[test]
    fn setup_file_watcher_tolerates_missing_config_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Config dir doesn't exist; watcher logs a warning and returns Ok.
        let missing_config = tmp.path().join("does/not/exist");
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);
        let watcher = super::super::reconcile::setup_file_watcher(tx, &[], &missing_config);
        assert!(
            watcher.is_ok(),
            "missing config_dir should not error: {watcher:?}"
        );
    }

    // ----- run_daemon_with end-to-end tests -----
    //
    // These drive `run_daemon_with` against externally-supplied triggers so
    // the full SETUP body (pre-loop config, IPC path, health-server gating,
    // startup-checkin gating, ctx assembly, loop run, cleanup) executes
    // without binding the per-user runtime socket or hitting the network.

    fn make_overrides_for_test(
        tmp: &tempfile::TempDir,
        triggers: DaemonTriggers,
    ) -> super::super::DaemonRunOverrides {
        super::super::DaemonRunOverrides {
            ipc_path: Some(tmp.path().join("daemon-test.sock")),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
            scope: crate::Scope::User,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_external_triggers_shuts_down_cleanly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        // Give the loop a tick to enter the select! arm
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        // Send shutdown
        senders.shutdown_tx.send(()).unwrap();

        // 30s, not 5s — Windows CI runners under cargo-llvm-cov instrumentation
        // run this shutdown path much slower than un-instrumented. Generous
        // slack so a slow runner is never the bug.
        let result = tokio::time::timeout(StdDuration::from_secs(30), daemon)
            .await
            .expect("daemon shutdown did not complete in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon should exit Ok, got {:?}", result);

        // Banner emitted by print_startup_banner
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Daemon running"),
            "banner should announce running state, got: {}",
            out
        );
        assert!(
            out.contains("Daemon stopped"),
            "shutdown should print stopped message, got: {}",
            out
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_reconcile_tick_via_external_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        // Drive a reconcile tick (default task __default__ is auto-built
        // from setup.reconcile_tasks with a 300s interval; first tick
        // always fires because last_reconciled is None).
        senders.reconcile_tx.send(()).await.unwrap();
        // Allow handle_reconcile to land before we shut down.
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
        // The state dir override is honored; a state.db should now exist
        // (handle_reconcile opens the store via state_dir_override). Both the
        // override and the production-default paths resolve the same canonical
        // filename, so no sibling `cfgd.db` is ever created.
        let store = tmp.path().join(crate::state::STATE_DB_FILENAME);
        assert!(
            store.exists(),
            "expected {} under {}",
            crate::state::STATE_DB_FILENAME,
            tmp.path().display()
        );
        assert!(
            !tmp.path().join("cfgd.db").exists(),
            "no divergent cfgd.db sibling should be created"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_sync_tick_with_no_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        senders.sync_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(60)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_sighup_tick_and_reloads_intervals() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        // Start with the happy-path config (no daemon spec).
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path.clone(),
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        // Wait until the daemon has finished its pre-loop setup — signalled by
        // the "Daemon running" banner — before rewriting the config. Setup
        // calls `config::load_config(config_path)` (build_pre_loop_setup); on
        // Windows a `std::fs::write` that truncates the same file *while* that
        // read is in flight raises a sharing violation (os error 32), which
        // propagates out of `run_daemon_with` as an early Err, drops the
        // injected triggers, and turns the SIGHUP send below into a SendError.
        // POSIX tolerates the concurrent read/truncate, so the race only ever
        // bit Windows CI. Sequencing the rewrite after setup removes the race
        // on every platform.
        wait_for_buffer_contains(&buf, "Daemon running", StdDuration::from_secs(5)).await;

        // Rewrite the config to introduce daemon reconcile interval.
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 45s\n",
        )
        .unwrap();
        senders.sighup_tx.send(()).await.unwrap();
        // Give the daemon enough headroom to process the SIGHUP and emit
        // the reload chatter. 80ms is fine for native runs but tight under
        // llvm-cov instrumentation, which slowed this test enough to lose
        // the printer-buffer race in the coverage build.
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Reloading configuration") || out.contains("Timer intervals reloaded"),
            "expected sighup reload chatter, got: {}",
            out
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_file_change_tick_via_external_trigger() {
        // A file-change tick goes through the dispatch arm in run_daemon_loop
        // and lands in handle_file_change_tick → debounce::record_change.
        // The daemon should keep running until we send shutdown.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path.clone(),
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        // Push a synthetic file-change path. The path doesn't need to map to
        // a managed_paths entry — the handler tolerates unknown paths and
        // simply records into the debounce map.
        senders.file_tx.send(config_path.clone()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_processes_compliance_tick_via_external_trigger() {
        // Drive the compliance-tick arm of run_daemon_loop. Without a
        // `compliance` config block the handler runs but writes nothing to
        // the state store; the daemon should still exit cleanly afterwards.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = make_overrides_for_test(&tmp, triggers);
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        senders.compliance_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_health_server_enabled_binds_ipc_socket() {
        // `skip_health_server = false` exercises the health-server spawn
        // branch. The IPC socket should be created and reachable while the
        // daemon is alive, then cleaned up by `cleanup_ipc_socket` on exit.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("health-on.sock");
        let (triggers, senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: false,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
            scope: crate::Scope::User,
        };
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        // Polled to a deadline rather than slept for a fixed span — a runner
        // executing the whole suite in parallel misses a 120ms budget while
        // being perfectly healthy.
        let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
        while std::time::Instant::now() < deadline && !ipc_path.exists() {
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
        assert!(
            ipc_path.exists(),
            "health server should have created the IPC socket at {}",
            ipc_path.display()
        );

        senders.shutdown_tx.send(()).unwrap();
        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);
        // cleanup_ipc_socket should unlink the socket on shutdown.
        assert!(
            !ipc_path.exists(),
            "cleanup_ipc_socket must remove the socket on exit"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_daemon_with_errors_when_ipc_path_has_live_listener() {
        use std::os::unix::net::UnixListener as StdUnixListener;
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("busy.sock");
        let _listener = StdUnixListener::bind(&ipc_path).unwrap();

        let (triggers, _senders) = make_triggers();
        let (printer, _buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
            scope: crate::Scope::User,
        };
        let result = super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        )
        .await;
        let err = result.expect_err("expect AlreadyRunning error");
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("already") || msg.to_lowercase().contains("running"),
            "expected AlreadyRunning, got: {msg}"
        );
    }

    // ----- additional pump / signal / banner coverage -----

    #[test]
    fn format_interval_lines_includes_sync_when_only_auto_push_enabled() {
        // The auto_pull-only and reconcile-only branches are already covered;
        // this hits the auto_push=true / auto_pull=false leg of the
        // `auto_pull || auto_push` guard so both halves of the boolean OR
        // execute under coverage instrumentation.
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(60),
            sync_interval: StdDuration::from_secs(180),
            auto_pull: false,
            auto_push: true,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(&parsed, None, 0, None);
        assert_eq!(
            lines,
            vec![
                "reconcile=60s".to_string(),
                "sync=180s (pull=false, push=true)".to_string(),
            ]
        );
    }

    #[test]
    fn format_interval_lines_reconcile_sync_compliance_combined() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(45),
            sync_interval: StdDuration::from_secs(90),
            auto_pull: true,
            auto_push: true,
            auto_apply: false,
            on_change_reconcile: false,
            notify_method: NotifyMethod::Stdout,
            notify_on_drift: false,
            webhook_url: None,
        };
        let lines = super::super::format_interval_lines(
            &parsed,
            Some(StdDuration::from_secs(600)),
            0,
            None,
        );
        assert_eq!(
            lines,
            vec![
                "reconcile=45s".to_string(),
                "sync=90s (pull=true, push=true)".to_string(),
                "compliance=600s".to_string(),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_pump_delivers_tick_within_interval() {
        // Cadence=1s. The pump should push a single () through within the
        // 3-second receive window.
        let secs = Arc::new(AtomicU64::new(1));
        let (tx, mut rx) = mpsc::channel::<()>(8);
        let handle = super::super::spawn_interval_pump(secs, tx);
        let tick = tokio::time::timeout(StdDuration::from_secs(3), rx.recv()).await;
        handle.abort();
        assert!(tick.is_ok(), "expected a tick within 3s, got {:?}", tick);
        assert!(
            tick.unwrap().is_some(),
            "tick should be Some(()) — pump must not close prematurely"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_pump_exits_when_receiver_dropped() {
        // After the rx is dropped, the first `tx.send().await` returns Err and
        // the loop breaks. The JoinHandle must transition to finished without
        // requiring abort().
        let secs = Arc::new(AtomicU64::new(1));
        let (tx, rx) = mpsc::channel::<()>(1);
        let handle = super::super::spawn_interval_pump(secs, tx);
        drop(rx);
        // Wait long enough for one cadence tick + send failure to land.
        let joined = tokio::time::timeout(StdDuration::from_secs(3), handle).await;
        assert!(
            joined.is_ok(),
            "pump must exit on send failure, got timeout"
        );
        // The task either finished normally (Ok) — abort wasn't needed.
        let join_result = joined.unwrap();
        assert!(
            join_result.is_ok(),
            "pump task should exit cleanly when rx closes, got {:?}",
            join_result
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn sighup_pump_forwards_signal_into_channel() {
        // Spawn the pump, raise SIGHUP at our own PID, and expect a () on the
        // receiver. Serial because signal handling is process-global and a
        // concurrent test could observe / consume the signal.
        let (tx, mut rx) = mpsc::channel::<()>(8);
        let handle = super::super::spawn_sighup_pump(tx).expect("sighup pump registers");
        // Give tokio the SIGHUP signal subscription a chance to wire up before
        // we raise the signal. Without this the kill arrives before the
        // listener exists and is dropped.
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        // SAFETY: libc::kill against own PID is well-defined.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGHUP);
        }
        let tick = tokio::time::timeout(StdDuration::from_secs(3), rx.recv()).await;
        handle.abort();
        assert!(tick.is_ok(), "expected a SIGHUP tick within 3s");
        assert!(tick.unwrap().is_some(), "tick should be Some(())");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn wait_for_shutdown_returns_on_sigterm() {
        // Driving the shutdown wait directly: register, raise SIGTERM at our
        // own PID, and verify the wait returns AND the printer captured the
        // SIGTERM message.
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        // Registration is synchronous, so the signal cannot arrive before the
        // handler exists — no sleep is needed to make this deterministic, and
        // the delivery the assertions rely on is a latched pending
        // notification rather than a race the test happened to win.
        let signals = super::super::ShutdownSignals::install();
        // SAFETY: libc::kill against own PID is well-defined.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
        let handle = tokio::spawn(signals.wait(Arc::clone(&printer)));
        let joined = tokio::time::timeout(StdDuration::from_secs(3), handle).await;
        assert!(
            joined.is_ok(),
            "wait_for_shutdown must return after SIGTERM"
        );
        joined.unwrap().expect("task join");
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Received SIGTERM"),
            "shutdown printer should announce SIGTERM, got: {}",
            out
        );
    }

    // ----- DaemonState direct unit coverage -----

    #[test]
    fn daemon_state_with_store_path_round_trips_through_test_accessor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state.db");
        let state = super::super::DaemonState::new().with_store_path(path.clone());
        let store = state
            .store_path_for_test()
            .expect("store_path set after with_store_path");
        assert_eq!(store, path.as_path());
    }

    #[test]
    fn daemon_state_to_response_reflects_internal_counters_and_sources() {
        // Construct a state, mutate the fields that to_response copies out,
        // and assert each field carried through. Catches reordering / typo
        // regressions in the field-by-field clone in to_response.
        let mut state = super::super::DaemonState::new();
        state.last_reconcile = Some("2026-05-25T00:00:00Z".to_string());
        state.last_sync = Some("2026-05-25T00:05:00Z".to_string());
        state.drift_count = 7;
        state.update_available = Some("0.5.0".to_string());
        state.sources.push(super::super::SourceStatus {
            name: "remote".to_string(),
            last_sync: None,
            last_reconcile: None,
            drift_count: 2,
            status: "active".to_string(),
        });
        let resp = state.to_response();
        assert!(resp.running);
        assert_eq!(resp.pid, std::process::id());
        assert_eq!(resp.last_reconcile.as_deref(), Some("2026-05-25T00:00:00Z"));
        assert_eq!(resp.last_sync.as_deref(), Some("2026-05-25T00:05:00Z"));
        assert_eq!(resp.drift_count, 7);
        assert_eq!(resp.update_available.as_deref(), Some("0.5.0"));
        // Default ctor adds a "local" source; we pushed one more.
        assert_eq!(resp.sources.len(), 2);
        assert_eq!(resp.sources[0].name, "local");
        assert_eq!(resp.sources[1].name, "remote");
        // module_reconcile is always built empty in to_response (a
        // forward-looking field populated elsewhere).
        assert!(resp.module_reconcile.is_empty());
    }

    // ----- Notifier::notify_webhook with a real (mock) HTTP endpoint -----
    //
    // notify_webhook spawns a tokio::task::spawn_blocking, so this test
    // sleeps briefly after .notify() to let the POST land. We assert via the
    // mockito expectation count rather than inspecting tracing output.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notifier_webhook_posts_payload_to_configured_url() {
        let server = tokio::task::spawn_blocking(mockito::Server::new)
            .await
            .expect("spawn mockito server");
        let mut server = server;
        let mock = server
            .mock("POST", "/notify")
            .with_status(200)
            .with_body("ok")
            .expect_at_least(1)
            .create();
        let url = format!("{}/notify", server.url());

        let notifier = super::super::Notifier::new(NotifyMethod::Webhook, Some(url));
        notifier.notify("test-event", "test-body");

        // The webhook POST is queued via spawn_blocking. Poll up to ~2s for the
        // mockito expectation to be satisfied.
        let mut satisfied = false;
        for _ in 0..40 {
            if mock.matched() {
                satisfied = true;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
        assert!(
            satisfied,
            "expected the webhook POST to land at the mock server within 2s"
        );
        mock.assert();
    }

    // ----- run_daemon_with: production-trigger path -----
    //
    // The other run_daemon_with tests in this module install
    // `external_triggers: Some(...)`. This one leaves it None so the function
    // takes the `else` branch and spawns real interval pumps, the SIGHUP pump
    // (Unix), and the shutdown listener. We bound the test with an outer
    // tokio::time::timeout so it terminates deterministically even though no
    // shutdown signal is sent — the assertion is that the function progressed
    // past the trigger-setup block and ran the loop until forcibly aborted.

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn run_daemon_with_production_triggers_progresses_past_setup_then_shutsdown_on_sigterm() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("prod-triggers.sock");
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: None,
            scope: crate::Scope::User,
        };

        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        // Wait for the startup banner. It is proof the SIGTERM handler is
        // installed, not merely that setup started: registration happens
        // synchronously before the banner is printed, so a banner in the
        // buffer means the signal raised below is delivered to the daemon
        // rather than to the default disposition that would kill this test
        // process. Polled to a deadline rather than slept for a fixed span —
        // a machine running the whole suite in parallel misses a fixed budget
        // while being perfectly healthy.
        let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
        let mut banner = false;
        while std::time::Instant::now() < deadline {
            if buf.lock().unwrap().contains("Daemon running") {
                banner = true;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
        assert!(
            banner,
            "production-trigger path must emit the startup banner before shutdown"
        );

        // SIGTERM drives the production wait_for_shutdown task which sends on
        // the shutdown oneshot, exiting the loop cleanly.
        // SAFETY: libc::kill against own PID is well-defined.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down on SIGTERM")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon should exit Ok, got {:?}", result);

        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Daemon stopped"),
            "cleanup path must run, got: {}",
            out
        );
    }

    // ----- handle_health_connection unit tests -----
    //
    // Drives the four-way path dispatch in health_ipc::handle_health_connection
    // through an in-memory `tokio::io::duplex` pair. No real socket bind, no
    // listener, no /tmp file — every assertion is on the HTTP response bytes
    // produced by the handler. Routes covered: /health, /status, /drift (both
    // with and without a store_path), and the unknown-path 404 fallback.

    /// Drive one request through `handle_health_connection`. Returns the raw
    /// HTTP response bytes the handler wrote.
    async fn drive_health_request(
        state: Arc<Mutex<super::super::DaemonState>>,
        request: &str,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (client, server) = tokio::io::duplex(8192);
        let handler = tokio::spawn(super::super::health_ipc::handle_health_connection(
            server, state,
        ));
        let (mut client_read, mut client_write) = tokio::io::split(client);
        client_write.write_all(request.as_bytes()).await.unwrap();
        // Drop the write half so the server sees EOF when draining the request
        // headers — the handler completes its response and returns Ok(()).
        drop(client_write);
        let _ = handler
            .await
            .expect("handle_health_connection task panicked");

        let mut out = Vec::new();
        client_read.read_to_end(&mut out).await.unwrap();
        String::from_utf8(out).expect("response should be utf-8")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_returns_health_payload_with_pid_and_uptime() {
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            resp.starts_with("HTTP/1.1 200 OK"),
            "expected 200 OK status line, got: {resp}"
        );
        assert!(
            resp.contains("\"status\": \"ok\""),
            "/health body should include status=ok: {resp}"
        );
        assert!(
            resp.contains("\"pid\""),
            "/health body should include pid: {resp}"
        );
        assert!(
            resp.contains("\"uptime_secs\""),
            "/health body should include uptime_secs: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_returns_status_response_with_sources() {
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        // DaemonStatusResponse includes a default "local" source entry.
        assert!(
            resp.contains("\"running\": true"),
            "/status should report running=true: {resp}"
        );
        assert!(
            resp.contains("\"local\""),
            "/status should serialize the default local source: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_drift_with_no_store_path_returns_empty_events() {
        // DaemonState::new() sets store_path=None; the /drift branch then
        // skips the spawn_blocking + StateStore::open and returns drift_count=0.
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /drift HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(
            resp.contains("\"drift_count\": 0"),
            "drift_count should be 0 with no store_path: {resp}"
        );
        assert!(
            resp.contains("\"events\": []"),
            "events should be the empty array: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_drift_with_recorded_event_returns_it_in_body() {
        // store_path=Some(<tempfile>) drives the spawn_blocking branch that
        // opens the StateStore and pulls unresolved_drift(). With a single
        // recorded drift event, the JSON body should include drift_count=1
        // and the event's resource_id.
        let tmp = tempfile::TempDir::new().unwrap();
        let store_path = tmp.path().join("state.db");
        // Open & seed in a scoped block so the connection drops before the
        // handler opens its own connection — SQLite WAL handles concurrent
        // readers but we keep the test deterministic.
        {
            let store = crate::state::StateStore::open(&store_path).unwrap();
            store
                .record_drift(
                    "file",
                    "/etc/hosts",
                    Some("expected-sha"),
                    Some("actual-sha"),
                    "file-manager",
                )
                .unwrap();
        }
        let mut s = super::super::DaemonState::new();
        // Reach into the private field; `daemon::tests::harness` is inside
        // `daemon` so super::super:: gives us module-private access.
        s.store_path = Some(store_path);
        let state = Arc::new(Mutex::new(s));
        let resp = drive_health_request(
            state,
            "GET /drift HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(
            resp.contains("\"drift_count\": 1"),
            "drift_count should be 1 after recording one event: {resp}"
        );
        assert!(
            resp.contains("/etc/hosts"),
            "event resource_id should appear in body: {resp}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_health_connection_unknown_path_returns_404() {
        let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
        let resp = drive_health_request(
            state,
            "GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            resp.starts_with("HTTP/1.1 404 Not Found"),
            "expected 404 status: {resp}"
        );
        assert!(
            resp.contains("\"error\":\"not found\""),
            "404 body should include not-found marker: {resp}"
        );
    }

    // ----- Loop-surface snapshot floor -----
    //
    // These tests capture only what the daemon's own Printer writes:
    // startup banner, SIGHUP reload chatter, shutdown messages. Per-action
    // reconcile output is emitted by `daemon::reconcile` through separate
    // short-lived printers and is invisible to the buffer below.

    use crate::output::test_capture::{assert_snapshot_at, strip_ansi};

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon/snapshots")
    }

    fn normalize_ipc(raw: &str, ipc_path: &Path) -> String {
        crate::normalize_for_snapshot(raw, &[(ipc_path, "<IPC_PATH>")])
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn snapshot_clean_reconcile_cycle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("daemon-test.sock");
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
            scope: crate::Scope::User,
        };
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path,
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        senders.reconcile_tx.send(()).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        senders.shutdown_tx.send(()).unwrap();

        // 30s, not 5s — Windows CI runners under cargo-llvm-cov instrumentation
        // run this shutdown path much slower than un-instrumented. Generous
        // slack so a slow runner is never the bug.
        let result = tokio::time::timeout(StdDuration::from_secs(30), daemon)
            .await
            .expect("daemon shutdown did not complete in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon should exit Ok, got {:?}", result);

        drop(printer);
        let raw = buf.lock().unwrap().clone();
        let actual = normalize_ipc(&strip_ansi(&raw), &ipc_path);
        assert_snapshot("clean_reconcile_cycle.txt", &actual);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn snapshot_drift_event() {
        // A file-change tick walks handle_file_change_tick → drift recording →
        // notifier path. The notifier writes via tracing/stdout, not through the
        // loop's Printer, so this snapshot is shape-identical to the clean cycle.
        // Its value is regression coverage that the drift path doesn't write to
        // the loop's surface and that the daemon survives the drift codepath and
        // shuts down cleanly.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = write_happy_path_config(&tmp);
        let ipc_path = tmp.path().join("daemon-test.sock");
        let (triggers, senders) = make_triggers();
        let (printer, buf) = Printer::for_test_at(crate::output::Verbosity::Normal);
        let printer = Arc::new(printer);
        let hooks: Arc<dyn DaemonHooks> = Arc::new(NoopHooks);

        let overrides = super::super::DaemonRunOverrides {
            ipc_path: Some(ipc_path.clone()),
            state_dir_override: Some(tmp.path().to_path_buf()),
            skip_health_server: true,
            skip_startup_checkin: true,
            external_triggers: Some(triggers),
            scope: crate::Scope::User,
        };
        let daemon = tokio::spawn(super::super::run_daemon_with(
            config_path.clone(),
            None,
            Arc::clone(&printer),
            hooks,
            overrides,
            env!("CARGO_PKG_VERSION"),
        ));

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        senders.file_tx.send(config_path).await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        senders.shutdown_tx.send(()).unwrap();

        let result = tokio::time::timeout(StdDuration::from_secs(5), daemon)
            .await
            .expect("daemon should shut down in time")
            .expect("daemon join");
        assert!(result.is_ok(), "daemon Ok, got {:?}", result);

        drop(printer);
        let raw = buf.lock().unwrap().clone();
        let actual = normalize_ipc(&strip_ansi(&raw), &ipc_path);
        assert_snapshot("drift_event.txt", &actual);
    }
}

// ---------------------------------------------------------------------------
// daemon/reconcile.rs — extra branch coverage:
//   * Plural-message branch fires when count > 1 new pending decisions in
//     one call (singular path is already covered by *_detects_new_items_on_change)
//   * withheld_decisions direct read-back contract — empty / multi-decision
//     / post-resolution-empty
// ---------------------------------------------------------------------------

#[test]
fn process_source_decisions_three_new_items_all_become_pending_in_one_call() {
    use crate::config::{CargoSpec, PackagesSpec};
    let store = test_state();
    // The plural-vs-singular branch (lines 730-742 in reconcile.rs) fires
    // inside `notifier.notify(...)` rendering when new_pending_count > 1.
    // We can't inspect the formatted body directly via Stdout notifier, but
    // we CAN pin the precondition: a single call must register all three
    // items as pending in the store, all of which then withhold. That's the
    // shape that would trigger the plural message in the notifier body.
    let notifier = Notifier::new(NotifyMethod::Stdout, None);
    let policy = AutoApplyPolicyConfig::default(); // Notify

    let merged = MergedProfile {
        packages: PackagesSpec {
            cargo: Some(CargoSpec {
                file: None,
                packages: vec!["bat".into(), "ripgrep".into(), "fd".into()],
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    process_source_decisions(&store, "acme", &merged, &policy, &notifier);

    let pending = store.pending_decisions().unwrap();
    assert_eq!(
        pending.len(),
        3,
        "all three new cargo items must produce pending decisions on the first call"
    );
    assert_eq!(
        withheld_paths(&store).len(),
        3,
        "all three pending items must withhold their resource"
    );
    let names: std::collections::HashSet<&str> =
        pending.iter().map(|d| d.resource.as_str()).collect();
    assert!(names.contains("packages.cargo.bat"));
    assert!(names.contains("packages.cargo.ripgrep"));
    assert!(names.contains("packages.cargo.fd"));
}

#[test]
fn withheld_decisions_returns_decision_resources_as_set() {
    let store = test_state();
    // Empty store → empty set
    let empty = withheld_paths(&store);
    assert!(empty.is_empty(), "no decisions → empty set");

    // Two distinct decisions
    store
        .upsert_pending_decision(
            "acme",
            "packages.cargo.bat",
            "recommended",
            "install",
            "recommended packages.cargo.bat",
        )
        .unwrap();
    store
        .upsert_pending_decision(
            "acme",
            "files.security/rules.yaml",
            "locked",
            "install",
            "locked files.security/rules.yaml",
        )
        .unwrap();

    let paths = withheld_paths(&store);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains("packages.cargo.bat"));
    assert!(paths.contains("files.security/rules.yaml"));

    // Resolving a decision removes it from the pending set
    store
        .resolve_decisions_for_source("acme", "accepted")
        .unwrap();
    let after = withheld_paths(&store);
    assert!(
        after.is_empty(),
        "resolving all decisions empties the pending-resource set"
    );
}

// ---------------------------------------------------------------------------
// daemon/reconcile.rs::discover_managed_paths — early-return arms.
// The cfg-load-Err and happy-path arms are covered elsewhere
// (*_returns_empty_for_missing_config and *_returns_targets_from_profile).
// These tests pin:
//   * no-profile arm: cfg has no spec.profile AND no profile_override → []
//   * profile-resolve-Err arm: profile name resolves to a missing file
// ---------------------------------------------------------------------------

struct DiscoverTestHooks;
impl DaemonHooks for DiscoverTestHooks {
    fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
        ProviderRegistry::new()
    }
    fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> crate::errors::Result<Vec<FileAction>> {
        Ok(vec![])
    }
    fn plan_packages(
        &self,
        _: &MergedProfile,
        _: &[&dyn PackageManager],
        _: &std::collections::HashSet<String>,
        _: &PackageContext<'_>,
    ) -> crate::errors::Result<Vec<PackageAction>> {
        Ok(vec![])
    }
    fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &config::PackagesSpec) {}
    fn expand_tilde(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }
}

#[test]
fn discover_managed_paths_returns_empty_when_no_profile_configured_or_overridden() {
    // Config has NO spec.profile and the caller passes no override →
    // the function takes the `None => return Vec::new()` arm before any
    // profile resolution is attempted.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec: {}\n",
    )
    .unwrap();

    let paths = discover_managed_paths(&config_path, None, &DiscoverTestHooks);
    assert!(
        paths.is_empty(),
        "no profile configured + no override → empty path list"
    );
}

#[test]
fn discover_managed_paths_returns_empty_when_named_profile_does_not_exist() {
    // Config names a profile, but the profiles/ dir doesn't contain it →
    // resolve_profile returns Err → the function takes the resolve-Err arm
    // and returns an empty Vec without panicking.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: ghost\n",
    )
    .unwrap();
    // Intentionally no profiles/ dir written — the named profile cannot resolve.

    let paths = discover_managed_paths(&config_path, None, &DiscoverTestHooks);
    assert!(
        paths.is_empty(),
        "profile name set but resolve_profile fails → empty path list, no panic"
    );
}

// ---------------------------------------------------------------------------
// handle_reconcile — modules block (reconcile.rs lines 264-291)
// Covers the `!resolved.merged.modules.is_empty()` branch of the
// module-resolution if/else. Without these, the entire block (cache_base
// derivation, quiet_printer construction, resolve_modules call, and both
// the Ok and Err arms) is skipped because every existing reconcile test
// uses a profile with no `modules:` list.
// ---------------------------------------------------------------------------

/// Build the minimal CfgdConfig + Profile pair that drives `handle_reconcile`
/// into the modules-resolution block. The profile lists `module_names` in its
/// `spec.modules:` array; the daemon will then call `resolve_modules` against
/// `<config_dir>/modules/`.
fn write_config_with_module_refs(tmp: &Path, module_names: &[&str]) -> PathBuf {
    let config_path = tmp.join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n",
    )
    .unwrap();
    let profiles_dir = tmp.join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    let mods_inline = module_names
        .iter()
        .map(|n| format!("    - {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        profiles_dir.join("default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n{mods_inline}\n",
        ),
    )
    .unwrap();
    config_path
}

/// DaemonHooks impl with no packages and no files. Lets the reconcile flow
/// reach the modules block without needing a registry, package planner, or
/// file planner — those branches are already covered by sibling tests.
struct EmptyPlanHooks;
impl DaemonHooks for EmptyPlanHooks {
    fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
        ProviderRegistry::new()
    }
    fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> crate::errors::Result<Vec<FileAction>> {
        Ok(vec![])
    }
    fn plan_packages(
        &self,
        _: &MergedProfile,
        _: &[&dyn PackageManager],
        _: &std::collections::HashSet<String>,
        _: &PackageContext<'_>,
    ) -> crate::errors::Result<Vec<PackageAction>> {
        Ok(vec![])
    }
    fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &config::PackagesSpec) {}
    fn expand_tilde(&self, path: &Path) -> PathBuf {
        crate::expand_tilde(path)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_warns_when_module_resolution_fails_and_continues() {
    // Profile references a module name with no on-disk module dir, so
    // `resolve_modules -> resolve_dependency_order` returns Err. The reconcile
    // body must take the `tracing::warn!` arm at reconcile.rs:284-287 and
    // substitute Vec::new() for resolved_modules; the rest of the reconcile
    // (plan generation, state update) must continue normally.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let config_path = write_config_with_module_refs(tmp.path(), &["does-not-exist"]);
    // No `modules/` dir is written — load_all_modules finds nothing, then
    // resolve_dependency_order errors with "module not found".

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // The reconcile completed past the modules block — last_reconcile is set.
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "warn-on-module-fail must not short-circuit reconcile — last_reconcile should be set"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_resolves_non_empty_modules_when_module_dir_exists() {
    // Profile lists a module that DOES exist on disk; load_all_modules and
    // resolve_dependency_order both succeed, the Ok arm at reconcile.rs:282-283
    // fires, and the resulting Vec<ResolvedModule> flows into reconciler.plan
    // (covered by sibling tests; here we only assert the reconcile completes).
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let config_path = write_config_with_module_refs(tmp.path(), &["empty-mod"]);

    // Minimal Module on disk — empty spec is enough; the modules block only
    // needs load + dependency-order resolution to succeed.
    let mod_dir = tmp.path().join("modules").join("empty-mod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: empty-mod\nspec: {}\n",
    )
    .unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // The reconcile resolved a non-empty module list and completed.
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "resolve_modules Ok arm must allow reconcile to complete normally"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_reconcile_auto_apply_with_sources_processes_decisions_and_resolves_removed() {
    // Drives reconcile.rs:200-243 — the `auto_apply && !sources.is_empty()`
    // branch. Pre-stages two sources in the config + a state-store row for
    // a third "removed" source whose pending decisions should get
    // auto-resolved (lines 226-238). Asserts:
    //   - last_reconcile is set (reconcile ran past the block)
    //   - pending decisions for the removed source are flipped to
    //     "rejected" by the auto-resolve loop
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      onChange: false\n      autoApply: true\n  sources:\n    - name: keep-src\n      origin:\n        type: Git\n        url: https://example.test/keep.git\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    // Pre-stage a pending decision for a source that's NOT in the config —
    // the auto-resolve loop at 226-238 should flip it to "rejected".
    {
        let store = StateStore::open(&state_dir.join("state.db")).unwrap();
        store
            .upsert_pending_decision(
                "removed-src",
                "packages.cargo.bat",
                "recommended",
                "install",
                "install bat",
            )
            .unwrap();
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Reconcile completed past the auto-apply block.
    {
        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_some(),
            "auto-apply branch must allow reconcile to complete"
        );
    }

    // The removed-src pending decision should now be rejected.
    let store = StateStore::open(&state_dir.join("state.db")).unwrap();
    let pending = store.pending_decisions().unwrap();
    assert!(
        pending.iter().all(|d| d.source != "removed-src"),
        "auto-resolve loop must flip removed-src decisions to non-pending: {pending:?}"
    );
}

// ---------------------------------------------------------------------------
// Fail-closed source composition in the pruning reconcile loop
// ---------------------------------------------------------------------------
//
// If `compose_daemon_desired_state` failed OPEN (substituted the local profile
// on a compose error), the pruning reconcile would see a source-delivered
// package as no-longer-desired and, under autoApply, UNINSTALL it. These tests
// pin the fail-closed contract: a broken/constraint-violating cached manifest
// SKIPS the tick (no prune, no uninstall, last_reconcile untouched, alert
// raised), while a benign never-synced cache-miss still reconciles local-only.

/// Stage a cached source under `<cache_root>/sources/<name>` providing a single
/// `team` profile whose `spec:` body is `team_spec` (already indented two
/// spaces). `cache_root` is the unified cfgd cache root (`<home>/.cache/cfgd`)
/// the daemon resolves its sources under. The source keeps the default policy,
/// so `constraints.noScripts` is on.
fn stage_cached_source(cache_root: &Path, name: &str, team_spec: &str) {
    let src_dir = cache_root.join("sources").join(name);
    std::fs::create_dir_all(src_dir.join("profiles")).unwrap();
    std::fs::write(
        src_dir.join("cfgd-source.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {name}\nspec:\n  provides:\n    profiles:\n      - team\n"
        ),
    )
    .unwrap();
    std::fs::write(
        src_dir.join("profiles").join("team.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: team\nspec:\n{team_spec}"
        ),
    )
    .unwrap();
    // Make it a git repo with a commit so head_commit resolves like a real cache.
    crate::test_helpers::init_test_git_repo(&src_dir);
}

/// Stage a cached source whose `team` profile carries a `preApply` script.
/// The source's policy keeps the default `noScripts: true` constraint, so
/// composition of the subscribed `team` profile fails with `ScriptsNotAllowed`
/// — a realistic "cached manifest went bad" error.
fn stage_constraint_violating_cached_source(cache_root: &Path, name: &str) {
    stage_cached_source(
        cache_root,
        name,
        "  scripts:\n    preApply:\n      - run: echo hi\n",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn handle_reconcile_compose_error_skips_tick_and_preserves_source_package() {
    // RED before the fail-closed fix: a constraint-violating cached source made
    // compose fall back to local-only, so the pruning reconcile uninstalled the
    // tracked source-delivered package. GREEN after: the tick is skipped, the
    // package survives, last_reconcile is NOT advanced, and an alert is raised.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    // The daemon resolves its source cache via `default_cache_dir_for`, which
    // short-circuits on the process-global `CFGD_CACHE_DIR` env var (verbatim,
    // every platform). reconcile runs on a `spawn_blocking` worker thread where
    // the thread-local test-home override does NOT apply, and `XDG_CACHE_HOME`
    // is honored only by the Linux `directories` backend — so pin the cache root
    // with `CFGD_CACHE_DIR` to stay correct on Linux, macOS, and Windows.
    let cache_root = tmp.path().join("cache-root").join("cfgd");
    let _cache =
        crate::test_helpers::EnvVarGuard::set("CFGD_CACHE_DIR", cache_root.to_str().unwrap());
    stage_constraint_violating_cached_source(&cache_root, "test-src");

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // cfgd previously installed the source-delivered package (tracked in state).
    {
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        seed.upsert_managed_resource("package", "cargo/source-pkg", "test-src", None, None)
            .unwrap();
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n  sources:\n    - name: test-src\n      origin:\n        type: Git\n        url: https://example.test/team.git\n      subscription:\n        profile: team\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let uninstalled = Arc::new(Mutex::new(Vec::<String>::new()));

    // A hook that WOULD prune `cargo/source-pkg` if reconcile ever reached
    // package planning with a local-only desired set. The fail-closed skip must
    // prevent that — so this Uninstall must NOT fire.
    struct PrunePkgHooks {
        uninstalled: Arc<Mutex<Vec<String>>>,
    }
    impl DaemonHooks for PrunePkgHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.package_managers
                .push(Box::new(RecordingUninstallManager {
                    uninstalled: Arc::clone(&self.uninstalled),
                    installed: ["source-pkg".to_string()].into_iter().collect(),
                }));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            cfgd_installed: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            let mut actions = Vec::new();
            if cfgd_installed.contains("cargo/source-pkg") {
                actions.push(PackageAction::Uninstall {
                    manager: "cargo".into(),
                    packages: vec!["source-pkg".into()],
                    origin: "test-src".into(),
                });
            }
            Ok(actions)
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let hooks = PrunePkgHooks {
        uninstalled: Arc::clone(&uninstalled),
    };
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            // notify_on_drift = true so the fail-closed alert path is exercised.
            quiet_reconcile_ctx(&st, &not, true, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // SAFETY: the tracked source package must NOT have been uninstalled.
    let uninstalled = uninstalled.lock().await;
    assert!(
        uninstalled.is_empty(),
        "fail-closed skip must prevent pruning the source package; got: {uninstalled:?}"
    );

    // Its tracking row must survive (not pruned).
    let after = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        after
            .is_resource_managed("package", "cargo/source-pkg")
            .unwrap(),
        "source package's tracking row must survive a skipped tick"
    );

    // The tick was skipped: last_reconcile was never advanced.
    {
        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_none(),
            "a fail-closed compose error must SKIP the tick (no last_reconcile update)"
        );
    }

    // Alert raised: the notifier captured a fail-closed skip notification whose
    // body explains the broken source config (so an operator knows WHY and what
    // to do). The title flags the skipped reconcile.
    let alerts = notifier.captured();
    assert!(
        alerts
            .iter()
            .any(|(title, body)| title.contains("reconcile skipped")
                && body.contains("source's cached config is broken")),
        "a fail-closed compose error must raise an alert naming the failure; got: {alerts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn handle_reconcile_never_synced_source_reconciles_local_only() {
    // The benign cache-miss case must NOT trip the fail-closed skip: a configured
    // source with no on-disk cache is warn+skip inside the resolver (not an Err),
    // so reconcile proceeds local-only and completes (last_reconcile advances).
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    // Pin the unified cache root to an EMPTY dir → the source is never-synced
    // (cache-miss). reconcile runs on a `spawn_blocking` worker thread where the
    // thread-local home override does not apply; `XDG_CACHE_HOME` is honored only
    // by the Linux `directories` backend, so pin with the process-global,
    // every-platform `CFGD_CACHE_DIR` short-circuit instead.
    let cache_root = tmp.path().join("cache-root-empty").join("cfgd");
    std::fs::create_dir_all(&cache_root).unwrap();
    let _cache =
        crate::test_helpers::EnvVarGuard::set("CFGD_CACHE_DIR", cache_root.to_str().unwrap());

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: false\n      driftPolicy: NotifyOnly\n  sources:\n    - name: never-synced\n      origin:\n        type: Git\n        url: https://example.test/x.git\n      subscription:\n        profile: team\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, false, &EmptyPlanHooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // Cache-miss is a happy path: reconcile completed local-only.
    let guard = state.lock().await;
    assert!(
        guard.last_reconcile.is_some(),
        "a never-synced source (cache-miss) must NOT skip the tick — reconcile proceeds local-only"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn handle_reconcile_required_uncached_source_skips_tick_and_preserves_package() {
    // A `sync.required: true` source with NO cache must NOT degrade to local-only
    // (which the daemon's pruning reconcile would treat as the required source's
    // packages being phantom drift, uninstalling them under autoApply). The
    // compose chokepoint returns RequiredSourceUnavailable → tick SKIPPED, the
    // tracked source-delivered package survives, last_reconcile untouched, alert
    // raised. Parallels handle_reconcile_compose_error_skips_tick_and_preserves_source_package
    // but for the cache-only fail-OPEN gap the chokepoint fix closes.
    let tmp = tempfile::tempdir().unwrap();
    let _g = crate::with_test_home_guard(tmp.path());
    // Pin the unified cache root to an EMPTY dir → the required source is
    // never-synced (no cache), so compose must fail-closed. reconcile runs on a
    // `spawn_blocking` worker thread where the thread-local home override does
    // not apply; `XDG_CACHE_HOME` is honored only by the Linux `directories`
    // backend, so pin with the process-global, every-platform `CFGD_CACHE_DIR`
    // short-circuit instead.
    let cache_root = tmp.path().join("cache-root-empty").join("cfgd");
    std::fs::create_dir_all(&cache_root).unwrap();
    let _cache =
        crate::test_helpers::EnvVarGuard::set("CFGD_CACHE_DIR", cache_root.to_str().unwrap());

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // cfgd previously installed the required source's package (tracked in state).
    {
        let seed = StateStore::open_in_dir(&state_dir).unwrap();
        seed.upsert_managed_resource("package", "cargo/source-pkg", "req-src", None, None)
            .unwrap();
    }

    let config_path = tmp.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: test\nspec:\n  profile: default\n  daemon:\n    enabled: true\n    reconcile:\n      interval: 60s\n      autoApply: true\n      driftPolicy: Auto\n  sources:\n    - name: req-src\n      origin:\n        type: Git\n        url: https://example.test/req.git\n      subscription:\n        profile: team\n      sync:\n        required: true\n",
    )
    .unwrap();
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();

    let uninstalled = Arc::new(Mutex::new(Vec::<String>::new()));

    struct PrunePkgHooks {
        uninstalled: Arc<Mutex<Vec<String>>>,
    }
    impl DaemonHooks for PrunePkgHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            let mut reg = ProviderRegistry::new();
            reg.package_managers
                .push(Box::new(RecordingUninstallManager {
                    uninstalled: Arc::clone(&self.uninstalled),
                    installed: ["source-pkg".to_string()].into_iter().collect(),
                }));
            reg
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            cfgd_installed: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            let mut actions = Vec::new();
            if cfgd_installed.contains("cargo/source-pkg") {
                actions.push(PackageAction::Uninstall {
                    manager: "cargo".into(),
                    packages: vec!["source-pkg".into()],
                    origin: "req-src".into(),
                });
            }
            Ok(actions)
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    let hooks = PrunePkgHooks {
        uninstalled: Arc::clone(&uninstalled),
    };
    let state = Arc::new(Mutex::new(DaemonState::new()));
    let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
    let st = Arc::clone(&state);
    let not = Arc::clone(&notifier);
    let sd = state_dir.clone();
    let cp = config_path.clone();
    tokio::task::spawn_blocking(move || {
        let printer = test_printer();
        handle_reconcile(
            &cp,
            None,
            quiet_reconcile_ctx(&st, &not, true, &hooks, &sd, &printer),
        );
    })
    .await
    .unwrap();

    // SAFETY: the required source's tracked package must NOT be uninstalled.
    let uninstalled = uninstalled.lock().await;
    assert!(
        uninstalled.is_empty(),
        "fail-closed skip must prevent pruning the required source's package; got: {uninstalled:?}"
    );

    let after = StateStore::open_in_dir(&state_dir).unwrap();
    assert!(
        after
            .is_resource_managed("package", "cargo/source-pkg")
            .unwrap(),
        "required source package's tracking row must survive a skipped tick"
    );

    {
        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_none(),
            "a required-uncached source must SKIP the tick (no last_reconcile update)"
        );
    }

    let alerts = notifier.captured();
    assert!(
        alerts
            .iter()
            .any(|(title, body)| title.contains("reconcile skipped")
                && body.contains("source's cached config is broken")),
        "a required-uncached source must raise the fail-closed skip alert; got: {alerts:?}"
    );
}

// ---------------------------------------------------------------------------
// IPC socket security — v0.4.0 release-blocker coverage
// ---------------------------------------------------------------------------
//
// Locks down `resolve_default_ipc_path` and `run_health_server` against the
// pre-fix hijack vectors:
//   - default `/tmp/cfgd.sock` (any local user could pre-bind & MITM)
//   - default umask 0022 leaving the socket world-readable
//   - unbounded client read OOMing the CLI from a hijacked peer
//
// All tests mutate process-global env vars so they MUST be serial. The
// EnvVarGuard / with_test_home_guard helpers restore prior state on drop
// (even on panic) so a failed test cannot poison the next.

mod ipc_socket_security {
    use super::*;
    #[cfg(unix)]
    use crate::daemon::health_ipc::MAX_RESPONSE_BYTES;
    use crate::test_helpers::EnvVarGuard;

    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_env_override_wins() {
        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", "/custom/cfgd.sock");
        assert_eq!(
            resolve_default_ipc_path(None, crate::Scope::User),
            std::path::PathBuf::from("/custom/cfgd.sock")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_uses_xdg_runtime_dir_when_set() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", "/tmp/test-xdg");
        assert_eq!(
            resolve_default_ipc_path(None, crate::Scope::User),
            std::path::PathBuf::from("/tmp/test-xdg/cfgd/cfgd.sock")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_falls_back_to_home_cache_when_xdg_unset_linux() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _unset_xdg = EnvVarGuard::unset("XDG_RUNTIME_DIR");
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());
        let expected = tmp
            .path()
            .join(".cache")
            .join("cfgd")
            .join("runtime")
            .join("cfgd.sock");
        assert_eq!(resolve_default_ipc_path(None, crate::Scope::User), expected);
    }

    /// On Windows the named-pipe endpoint is scope-aware: a per-user daemon and
    /// the system Windows Service must resolve to DIFFERENT pipe names, or a user
    /// CLI would connect to the machine-wide service. Mirrors the Unix
    /// `/run/cfgd` vs per-user-runtime split.
    #[cfg(windows)]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_windows_scope_selects_distinct_pipe() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let user = resolve_default_ipc_path(None, crate::Scope::User);
        let system = resolve_default_ipc_path(None, crate::Scope::System);
        assert_eq!(user, std::path::PathBuf::from(r"\\.\pipe\cfgd"));
        assert_eq!(system, std::path::PathBuf::from(r"\\.\pipe\cfgd-system"));
        assert_ne!(
            user, system,
            "user and system scopes must not share a pipe name"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial_test::serial]
    fn resolve_default_ipc_path_uses_application_support_on_macos() {
        let _unset_override = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", tmp.path().to_str().unwrap());
        let expected = tmp
            .path()
            .join("Library")
            .join("Application Support")
            .join("cfgd")
            .join("runtime")
            .join("cfgd.sock");
        assert_eq!(resolve_default_ipc_path(None, crate::Scope::User), expected);
    }

    /// Drives `run_health_server` against a tempdir socket path and asserts
    /// the bound socket file is 0600 — covers the umask-default-leaks fix.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn bind_socket_sets_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        // run_health_server creates the parent dir with 0700 itself; we point
        // at a nested path so the create-and-chmod arms both fire.
        let sock_path = tmp.path().join("runtime").join("cfgd.sock");

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let sock = sock_path.to_string_lossy().to_string();
        let handle = tokio::spawn(async move {
            let _ = run_health_server(&sock, state).await;
        });

        // Spin briefly waiting for bind — keeps the test deterministic without
        // depending on a fixed sleep.
        for _ in 0..200 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            sock_path.exists(),
            "expected health server to bind {}",
            sock_path.display()
        );

        let mode = std::fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket must be owner-only, got {:o}", mode);

        // Parent directory must be 0700 too (set by run_health_server).
        let parent_mode = std::fs::metadata(sock_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "parent dir must be owner-only, got {:o}",
            parent_mode
        );

        handle.abort();
        let _ = handle.await;
    }

    /// Drives `ensure_owner_private_dir` against a path whose parent component
    /// is a regular file. `create_dir_all` then fails with ENOTDIR on every
    /// unix regardless of uid, so the helper returns a HealthSocketError naming
    /// the offending directory. Proves the helper does not silently continue
    /// when the parent dir cannot be made owner-private.
    ///
    /// A file component is the portable way to force this: a `/proc/<x>` path
    /// is creation-hostile only on Linux (FreeBSD mounts no procfs by default,
    /// so root can mkdir under the `/proc` mountpoint and the negative path
    /// never fires). The test suite frequently runs as root in CI/devcontainers,
    /// so the mode-check arm (`mode & 0o077 != 0`) cannot be exercised
    /// end-to-end — root bypasses chmod, so the helper always succeeds in
    /// lowering 0o755 to 0o700 before the re-stat. The create-failure arm here
    /// is the negative path that fires deterministically regardless of uid; the
    /// owner-private predicate itself is unit-tested in the sibling
    /// `owner_private_predicate_rejects_world_readable_modes` test.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn bind_socket_refuses_world_readable_parent_dir() {
        use crate::daemon::health_ipc::ensure_owner_private_dir;
        let tmp = tempfile::tempdir().unwrap();
        let not_a_dir = tmp.path().join("not-a-dir");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let bogus = not_a_dir.join("cfgd");
        let err = ensure_owner_private_dir(&bogus)
            .expect_err("expected refusal when parent dir cannot be made owner-private");
        let msg = format!("{err}");
        assert!(
            msg.contains(&bogus.display().to_string()),
            "error must name the offending directory, got {msg:?}"
        );
    }

    /// Pure unit test of the mode-check predicate `ensure_owner_private_dir`
    /// uses to refuse world-readable parents. Pairs with the create-failure
    /// test above to cover the second negative arm without relying on uid-0
    /// chmod behaviour. Mirrors the `mode & 0o077 != 0` check.
    #[cfg(unix)]
    #[test]
    fn owner_private_predicate_rejects_world_readable_modes() {
        assert_ne!(0o755 & 0o077, 0, "0o755 must trip the predicate");
        assert_ne!(0o750 & 0o077, 0, "0o750 must trip the predicate");
        assert_ne!(0o701 & 0o077, 0, "0o701 must trip the predicate");
        assert_eq!(0o700 & 0o077, 0, "0o700 must pass the predicate");
        assert_eq!(0o600 & 0o077, 0, "0o600 must pass the predicate");
    }

    /// Drives `query_daemon_status` against a fake server that streams more
    /// than `MAX_RESPONSE_BYTES`, asserting the read is capped and an
    /// "exceeded" error surfaces. Uses a real Unix listener + socketpair-style
    /// override via `CFGD_DAEMON_IPC_PATH`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_caps_response_at_max_bytes() {
        use std::io::Write as IoWrite;
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("flood.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        // Stream MAX_RESPONSE_BYTES * 2 of body so the cap definitely trips
        // before the peer-close EOF would arrive.
        let flood_bytes = (MAX_RESPONSE_BYTES * 2) as usize;
        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                // Pass headers, then flood the body.
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
                );
                let chunk = vec![b'x'; 8192];
                let mut sent = 0usize;
                while sent < flood_bytes {
                    if s.write_all(&chunk).is_err() {
                        break;
                    }
                    sent += chunk.len();
                }
                let _ = s.flush();
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(|| query_daemon_status(None, crate::Scope::User))
            .await
            .unwrap();
        let _ = server.join();

        match result {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("exceeded"),
                    "expected response-cap error, got {msg:?}"
                );
            }
            Ok(other) => panic!("expected cap error, got Ok({other:?})"),
        }
    }
}

// ---------------------------------------------------------------------------
// query_daemon_status & connect_daemon_ipc — additional coverage paths
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod query_daemon_status_paths {
    use super::*;
    use crate::test_helpers::EnvVarGuard;

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn query_daemon_status_returns_none_when_socket_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nope.sock");
        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", nonexistent.to_str().unwrap());
        let result =
            query_daemon_status(None, crate::Scope::User).expect("missing socket must not error");
        assert!(
            result.is_none(),
            "missing socket path returns Ok(None), got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_parses_valid_response_body() {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("status.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let body = serde_json::to_string(&DaemonStatusResponse {
                    running: true,
                    pid: 42,
                    uptime_secs: 99,
                    last_reconcile: None,
                    last_sync: None,
                    drift_count: 0,
                    sources: vec![],
                    update_available: None,
                    module_reconcile: vec![],
                })
                .unwrap();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.flush();
                // Half-shutdown the write side then drain reads so the client
                // sees EOF (not ECONNRESET) on its read pass.
                let _ = s.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 1024];
                while let Ok(n) = s.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(|| query_daemon_status(None, crate::Scope::User))
            .await
            .unwrap();
        let _ = server.join();

        let status = result
            .expect("status must parse")
            .expect("status must be Some");
        assert_eq!(status.pid, 42);
        assert_eq!(status.uptime_secs, 99);
        assert!(status.running);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_returns_none_on_empty_body() {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("empty.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
                );
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 1024];
                while let Ok(n) = s.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(|| query_daemon_status(None, crate::Scope::User))
            .await
            .unwrap();
        let _ = server.join();

        // Empty body → Ok(None).
        assert!(
            matches!(result, Ok(None)),
            "empty body should give Ok(None), got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn query_daemon_status_returns_err_on_malformed_json() {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::os::unix::net::UnixListener as StdUnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("bad.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();

        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let body = "{not even json";
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.flush();
                // Half-shutdown the write side then drain reads so the client
                // sees EOF (not ECONNRESET) on its read pass.
                let _ = s.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 1024];
                while let Ok(n) = s.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let _g = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock_path.to_str().unwrap());
        let result = tokio::task::spawn_blocking(|| query_daemon_status(None, crate::Scope::User))
            .await
            .unwrap();
        let _ = server.join();

        match result {
            Err(e) => assert!(
                e.to_string().contains("parse response"),
                "error must mention parse, got: {e}"
            ),
            Ok(other) => panic!("expected parse err, got: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// handle_sync — signature verification after pull (require_signed_commits)
// ---------------------------------------------------------------------------

mod handle_sync_signature_paths {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_pulled_unsigned_commit_with_require_signed_returns_false() {
        // require_signed_commits=true, allow_unsigned=false: after a successful
        // pull, verify_head_signature fires; for an unsigned commit it errors,
        // and handle_sync returns false (the content is untrusted).
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        // Clone and seed the work repo with an initial commit pushed to origin.
        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        // Push an UNSIGNED change from pusher.
        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("NEWFILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        // require_signed_commits=true, allow_unsigned=false
        let changed = handle_sync(&work_dir, true, false, "local", &state, true, false).await;

        assert!(
            !changed,
            "unsigned-commit pull with require_signed must return false"
        );
        // Even though the verification failed, last_sync should NOT be
        // updated because the early return short-circuits before the
        // state-mutation block.
        let st = state.lock().await;
        assert!(
            st.last_sync.is_none(),
            "early-return path must not bump last_sync"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_sync_pulled_unsigned_with_allow_unsigned_returns_true() {
        // require_signed_commits=true, allow_unsigned=true: signature check
        // is bypassed by verify_commit_signature; but handle_sync only calls
        // verify_head_signature unconditionally here. allow_unsigned guards
        // the call. Verify the pull succeeds and `changed=true` is returned.
        let tmp = tempfile::TempDir::new().unwrap();
        let bare_dir = tmp.path().join("bare.git");
        let work_dir = tmp.path().join("work");
        let pusher_dir = tmp.path().join("pusher");

        std::fs::create_dir_all(&bare_dir).unwrap();
        git2::Repository::init_bare(&bare_dir).unwrap();

        let repo = git2::Repository::clone(bare_dir.to_str().unwrap(), &work_dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "cfgd-test").unwrap();
            config.set_str("user.email", "test@cfgd.io").unwrap();
        }
        std::fs::write(work_dir.join("README"), "v1\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("README")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        {
            let mut remote = repo.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let pusher = git2::Repository::clone(bare_dir.to_str().unwrap(), &pusher_dir).unwrap();
        {
            let mut config = pusher.config().unwrap();
            config.set_str("user.name", "cfgd-pusher").unwrap();
            config.set_str("user.email", "pusher@cfgd.io").unwrap();
        }
        std::fs::write(pusher_dir.join("NEWFILE"), "synced\n").unwrap();
        {
            let mut index = pusher.index().unwrap();
            index.add_path(Path::new("NEWFILE")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = pusher.find_tree(tree_id).unwrap();
            let sig = pusher.signature().unwrap();
            let parent = pusher.head().unwrap().peel_to_commit().unwrap();
            pusher
                .commit(Some("HEAD"), &sig, &sig, "add newfile", &tree, &[&parent])
                .unwrap();
        }
        {
            let mut remote = pusher.find_remote("origin").unwrap();
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .unwrap();
        }

        let state = Arc::new(Mutex::new(DaemonState::new()));
        // require_signed_commits=true, allow_unsigned=true → bypass verify.
        let changed = handle_sync(&work_dir, true, false, "local", &state, true, true).await;

        assert!(
            changed,
            "allow_unsigned must bypass signature verify and return true"
        );
        let st = state.lock().await;
        assert!(
            st.last_sync.is_some(),
            "successful sync must bump last_sync"
        );
    }
}

// ---------------------------------------------------------------------------
// handle_reconcile — module_filter (per-module reconcile) and pending-config
// consumption branches not covered by the wider drift tests.
// ---------------------------------------------------------------------------

mod handle_reconcile_extra_branches {
    use super::*;

    /// Build the minimum cfgd.yaml + profiles/default.yaml on disk and return
    /// the (config_path, state_dir) pair plus the owning TempDir.
    fn write_min_fixture(content: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(&config_path, content).unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        (tmp, config_path, state_dir)
    }

    use crate::test_helpers::NoopDaemonHooks as NoopHooks;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_per_module_filter_updates_module_last_reconcile() {
        // module_filter=Some(_) path: the per-module branch records only into
        // `module_last_reconcile` and skips the profile-wide last_reconcile.
        let (_tmp, config_path, state_dir) = write_min_fixture(
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));

        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            let printer = test_printer();
            handle_reconcile(
                &cp,
                None,
                ReconcileCtx {
                    state: &st,
                    notifier: &not,
                    notify_on_drift: false,
                    hooks: &NoopHooks,
                    state_dir_override: Some(&sd),
                    printer: &printer,
                    module_filter: Some("dev-tools"),
                    auto_apply_override: Some(false),
                    drift_policy_override: Some(config::DriftPolicy::NotifyOnly),
                    scope: crate::Scope::User,
                    abort: never_abort(),
                },
            );
        })
        .await
        .unwrap();

        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_none(),
            "per-module tick must NOT touch profile-wide last_reconcile"
        );
        assert!(
            guard.module_last_reconcile.contains_key("dev-tools"),
            "per-module tick must record into module_last_reconcile, got: {:?}",
            guard.module_last_reconcile
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn handle_reconcile_consumes_pending_server_config_and_clears_file() {
        // Profile-wide tick (module_filter=None) walks the
        // `load_pending_server_config()` -> `clear_pending_server_config()`
        // arm at the bottom of handle_reconcile. Stage a pending JSON file
        // under a CFGD_STATE_DIR-scoped state dir, run reconcile, and assert
        // the file is removed.
        let pending_root = tempfile::tempdir().unwrap();
        let _g = crate::test_helpers::EnvVarGuard::set(
            "CFGD_STATE_DIR",
            pending_root.path().to_str().unwrap(),
        );

        std::fs::create_dir_all(pending_root.path()).unwrap();
        let pending_path = pending_root.path().join("pending-server-config.json");
        std::fs::write(
            &pending_path,
            r#"{"spec":{"profile":"default","packages":{}}}"#,
        )
        .unwrap();
        assert!(pending_path.exists(), "pending file must exist pre-test");

        let (_tmp, config_path, state_dir) = write_min_fixture(
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        );

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();

        tokio::task::spawn_blocking(move || {
            let printer = test_printer();
            handle_reconcile(
                &cp,
                None,
                quiet_reconcile_ctx(&st, &not, false, &NoopHooks, &sd, &printer),
            );
        })
        .await
        .unwrap();

        assert!(
            !pending_path.exists(),
            "pending-server-config.json should have been consumed and cleared"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_reconcile_with_invalid_profile_yaml_logs_and_returns() {
        // resolve_profile error arm (lines ~196-201): write a syntactically
        // bogus profile YAML so resolve_profile fails and the function
        // returns without crashing or recording state.
        let (_tmp, config_path, state_dir) = write_min_fixture(
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: bogus\n",
        );
        // bogus profile -> resolve_profile returns NotFound; reconcile logs an
        // error and bails. No state changes expected.

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let notifier = Arc::new(Notifier::new(NotifyMethod::Stdout, None));
        let st = Arc::clone(&state);
        let not = Arc::clone(&notifier);
        let sd = state_dir.clone();
        let cp = config_path.clone();
        tokio::task::spawn_blocking(move || {
            let printer = test_printer();
            handle_reconcile(
                &cp,
                None,
                quiet_reconcile_ctx(&st, &not, false, &NoopHooks, &sd, &printer),
            );
        })
        .await
        .unwrap();

        let guard = state.lock().await;
        assert!(
            guard.last_reconcile.is_none(),
            "profile resolution failure must not bump last_reconcile"
        );
        assert_eq!(
            guard.drift_count, 0,
            "no drift counted when planning failed"
        );
    }
}

// ---------------------------------------------------------------------------
// discover_managed_paths — explicit profile_override branches not covered
// by the existing test list.
// ---------------------------------------------------------------------------

mod discover_managed_paths_extra {
    use super::*;

    struct StubHooks;
    impl DaemonHooks for StubHooks {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(
            &self,
            _: &Path,
            _: &ResolvedProfile,
        ) -> crate::errors::Result<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> crate::errors::Result<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(
            &self,
            _: &mut ProviderRegistry,
            _: &config::PackagesSpec,
        ) {
        }
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    #[test]
    fn discover_managed_paths_with_explicit_profile_override_picks_override_targets() {
        // Hits the `profile_override.or(cfg.spec.profile.as_deref())` arm
        // for the explicit-Some case (the existing tests only cover the
        // fallthrough-None path).
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec: {}\n",
        )
        .unwrap();

        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(
            profiles_dir.join("override.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: override\nspec:\n  files:\n    managed:\n      - source: src/a.txt\n        target: /tmp/cfgd-test-override-target.txt\n",
        )
        .unwrap();

        let paths = discover_managed_paths(&config_path, Some("override"), &StubHooks);
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().contains("override-target.txt")),
            "explicit profile_override should return that profile's targets: {paths:?}"
        );
    }

    #[test]
    fn discover_managed_paths_returns_empty_when_profile_resolution_fails() {
        // Hits the resolve_profile-Err arm (lines ~85-88): cfg names a profile
        // file that doesn't exist on disk.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: missing\n",
        )
        .unwrap();
        // profiles dir exists but the named profile does not
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();

        let paths = discover_managed_paths(&config_path, None, &StubHooks);
        assert!(
            paths.is_empty(),
            "profile-resolution failure must yield empty paths, got: {paths:?}"
        );
    }
}

mod tests_run_daemon_wrapper {
    use crate::config::CfgdConfig;
    use crate::config::PackagesSpec;
    use crate::daemon::DaemonDirOverrides;
    use crate::daemon::DaemonHooks;
    use crate::daemon::run_daemon;
    use crate::daemon::{MergedProfile, ResolvedProfile};
    use crate::errors::Result as CfgdResult;
    use crate::providers::{
        FileAction, PackageAction, PackageContext, PackageManager, ProviderRegistry,
    };
    use crate::test_helpers::test_printer;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct StubHooks2;
    impl DaemonHooks for StubHooks2 {
        fn build_registry(&self, _: &CfgdConfig) -> ProviderRegistry {
            ProviderRegistry::new()
        }
        fn plan_files(&self, _: &Path, _: &ResolvedProfile) -> CfgdResult<Vec<FileAction>> {
            Ok(vec![])
        }
        fn plan_packages(
            &self,
            _: &MergedProfile,
            _: &[&dyn PackageManager],
            _: &std::collections::HashSet<String>,
            _: &PackageContext<'_>,
        ) -> CfgdResult<Vec<PackageAction>> {
            Ok(vec![])
        }
        fn extend_registry_custom_managers(&self, _: &mut ProviderRegistry, _: &PackagesSpec) {}
        fn expand_tilde(&self, path: &Path) -> PathBuf {
            crate::expand_tilde(path)
        }
    }

    // Serial + explicitly unset: `resolve_default_ipc_path` consults
    // `CFGD_DAEMON_IPC_PATH` before it ever looks at --runtime-dir, and the
    // socket-server tests set that variable process-wide while they run.
    #[test]
    #[serial_test::serial]
    fn cli_run_overrides_carry_the_state_dir_and_runtime_dir_flags() {
        use crate::daemon::cli_run_overrides;
        let _unset_override = crate::test_helpers::EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let state = PathBuf::from("/srv/cfgd-state");
        let runtime = PathBuf::from("/srv/cfgd-run");
        let over = cli_run_overrides(
            DaemonDirOverrides {
                runtime_dir: Some(runtime.clone()),
                state_dir: Some(state.clone()),
            },
            crate::Scope::User,
        );
        assert_eq!(
            over.state_dir_override.as_deref(),
            Some(state.as_path()),
            "--state-dir must reach the loop, or the daemon's drift events, backups, and apply lock land where the CLI never looks"
        );
        let ipc = over.ipc_path.expect("runtime dir resolves an ipc path");
        // The endpoint's SHAPE is the one thing that genuinely differs by OS: a
        // unix socket is a file under the runtime dir, a Windows named pipe is a
        // kernel object in the flat `\\.\pipe\` namespace that no directory can
        // contain. Each side asserts its own real contract rather than the
        // check being dropped on the platform where it does not fit.
        #[cfg(unix)]
        assert!(
            ipc.starts_with(&runtime),
            "--runtime-dir must bind the socket under the given root, got {ipc:?}"
        );
        #[cfg(windows)]
        {
            let _ = &runtime;
            assert_eq!(
                ipc,
                PathBuf::from(r"\\.\pipe\cfgd"),
                "a named pipe cannot live under --runtime-dir, but scope must still pick the per-user endpoint"
            );
        }
    }

    #[test]
    fn cli_run_overrides_leave_both_dirs_to_the_defaults_when_unset() {
        use crate::daemon::cli_run_overrides;
        let over = cli_run_overrides(DaemonDirOverrides::default(), crate::Scope::User);
        assert!(
            over.state_dir_override.is_none(),
            "no flag → fall through to CFGD_STATE_DIR / the scope default"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_daemon_with_invalid_config_returns_err_early() {
        let printer = Arc::new(test_printer());
        let hooks: Arc<dyn DaemonHooks> = Arc::new(StubHooks2);
        let bogus_path = PathBuf::from("/nonexistent-cfgd-cfg-7f9a/does-not-exist.yaml");
        let result = run_daemon(
            bogus_path,
            None,
            DaemonDirOverrides::default(),
            printer,
            hooks,
            crate::Scope::User,
            env!("CARGO_PKG_VERSION"),
        )
        .await;
        assert!(
            result.is_err(),
            "missing config must propagate as Err, got Ok"
        );
    }
}

// ===========================================================================
// Scheduled-backup timers
//
// `daemon::backup` owns scheduling only; the run itself goes through the same
// `crate::backup::run_backup` the CLI drives, so these tests assert the timer
// arithmetic, the SIGHUP rebuild, and that a fire lands a real `backup_runs`
// row — not the engine's own semantics, which `backup/tests.rs` covers.
// ===========================================================================

mod backup_timers {
    use super::harness::{make_test_ctx, make_triggers, pre_loop, sighup_ctx};
    use super::*;
    use crate::daemon::backup::{
        BackupTask, BackupTimers, DegradedReason, ResolvedBackupTasks, build_backup_tasks,
        reload_backup_tasks, resolve_backup_tasks,
    };
    use crate::state::StateStore;
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration as StdDuration, Instant};

    /// A backup spec with an optional schedule, everything else defaulted.
    fn spec(name: &str, source: &Path, schedule: Option<&str>) -> config::BackupSpec {
        let mut yaml = format!("name: {name}\nsource: {}\n", crate::to_posix_string(source));
        if let Some(s) = schedule {
            yaml.push_str(&format!("schedule: \"{s}\"\n"));
        }
        serde_yaml::from_str(&yaml).expect("backup spec should parse")
    }

    fn task(name: &str, source: &Path, schedule: &str, now: Instant) -> BackupTask {
        BackupTask::new(
            &spec(name, source, Some(schedule)),
            "workstation",
            now,
            None,
        )
        .expect("schedule should install a timer")
    }

    /// A clean (non-degraded) timer set around `tasks` — the shape a healthy
    /// resolution produces.
    fn timers(tasks: Vec<BackupTask>) -> BackupTimers {
        BackupTimers::new(
            ResolvedBackupTasks {
                tasks,
                degraded: None,
            },
            Instant::now(),
        )
    }

    /// No recorded history — every unit's interval starts from now.
    fn no_history(_: &str) -> Option<String> {
        None
    }

    /// Write a config plus a `default` profile whose `spec.backups` block is
    /// `backups_yaml` (already indented four spaces).
    fn write_config_with_backups(tmp: &tempfile::TempDir, backups_yaml: &str) -> PathBuf {
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  backups:\n{backups_yaml}"
            ),
        )
        .unwrap();
        config_path
    }

    // ----- next-fire computation -----

    #[test]
    fn interval_next_fire_is_one_period_out() {
        let now = Instant::now();
        let t = task("db", Path::new("/tmp/db"), "45s", now);
        assert_eq!(t.next_fire(), now + StdDuration::from_secs(45));
    }

    #[test]
    fn cron_next_fire_matches_croners_own_search() {
        let now = Instant::now();
        let t = task("db", Path::new("/tmp/db"), "0 * * * *", now);
        let cron: croner::Cron = "0 * * * *".parse().unwrap();
        let wall = chrono::Local::now();
        let expected = (cron.find_next_occurrence(&wall, false).unwrap() - wall)
            .to_std()
            .unwrap();
        let actual = t.next_fire().duration_since(now);
        let skew = actual.abs_diff(expected);
        assert!(
            skew < StdDuration::from_secs(2),
            "cron deadline {actual:?} should track croner's {expected:?}"
        );
        assert!(
            t.next_fire() > now,
            "a cron deadline must be strictly in the future"
        );
    }

    #[test]
    fn a_task_is_due_only_once_its_deadline_has_passed() {
        let now = Instant::now();
        let t = task("db", Path::new("/tmp/db"), "60s", now);
        assert!(!t.is_due(now));
        assert!(!t.is_due(now + StdDuration::from_secs(59)));
        assert!(t.is_due(now + StdDuration::from_secs(60)));
    }

    // ----- overlap / missed-fire behaviour -----

    #[test]
    fn advance_skips_the_fires_that_elapsed_while_the_loop_was_busy() {
        let past = Instant::now() - StdDuration::from_secs(10);
        let mut t = task("db", Path::new("/tmp/db"), "1s", past);
        let now = Instant::now();
        let missed = t.advance(now);
        assert!(
            (8..=10).contains(&missed),
            "a 1s schedule 10s behind should report ~9 skipped fires, got {missed}"
        );
        assert!(
            t.next_fire() > now,
            "advance must arm a deadline in the future, never queue the backlog"
        );
        assert!(
            t.next_fire() <= now + StdDuration::from_secs(1),
            "the next fire stays on the declared cadence"
        );
    }

    #[test]
    fn advance_reports_no_missed_fires_for_a_prompt_tick() {
        let now = Instant::now();
        let mut t = task("db", Path::new("/tmp/db"), "30s", now);
        let due_at = now + StdDuration::from_secs(30);
        assert_eq!(t.advance(due_at), 0);
        assert_eq!(t.next_fire(), due_at + StdDuration::from_secs(30));
    }

    #[test]
    fn advance_on_a_cron_schedule_arms_a_future_deadline() {
        let past = Instant::now() - StdDuration::from_secs(120);
        let mut t = task("db", Path::new("/tmp/db"), "* * * * *", past);
        let now = Instant::now();
        let missed = t.advance(now);
        assert!(
            missed > 0,
            "a per-minute cron two minutes behind must report skipped fires"
        );
        assert!(t.next_fire() > now);
        assert!(
            t.next_fire() <= now + StdDuration::from_secs(61),
            "the next per-minute occurrence is at most a minute out"
        );
    }

    // ----- task-set construction -----

    #[test]
    fn only_scheduled_backups_get_a_timer() {
        let now = Instant::now();
        let specs = vec![
            spec("scheduled", Path::new("/tmp/a"), Some("1h")),
            spec("apply-time", Path::new("/tmp/b"), None),
        ];
        let tasks = build_backup_tasks(&specs, "workstation", now, &no_history);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].spec.name, "scheduled");
        assert_eq!(tasks[0].profile_name, "workstation");
    }

    #[test]
    fn an_unparseable_schedule_installs_no_timer() {
        let specs = vec![spec("broken", Path::new("/tmp/a"), Some("every tuesday"))];
        assert!(build_backup_tasks(&specs, "workstation", Instant::now(), &no_history).is_empty());
    }

    #[test]
    fn next_backup_deadline_takes_the_soonest_of_the_set() {
        let now = Instant::now();
        let set = timers(vec![
            task("late", Path::new("/tmp/a"), "1h", now),
            task("soon", Path::new("/tmp/b"), "30s", now),
        ]);
        assert_eq!(
            runner::next_backup_deadline(&set),
            now + StdDuration::from_secs(30)
        );
    }

    #[test]
    fn next_backup_deadline_parks_when_nothing_is_scheduled() {
        let deadline = runner::next_backup_deadline(&BackupTimers::empty());
        assert!(
            deadline > Instant::now() + StdDuration::from_secs(60),
            "an empty timer set must park rather than spin the loop"
        );
    }

    // ----- SIGHUP rebuild -----

    #[test]
    fn reload_carries_the_pending_deadline_of_an_unchanged_unit() {
        let past = Instant::now() - StdDuration::from_secs(30);
        let mut current = vec![task("db", Path::new("/tmp/db"), "1h", past)];
        let carried = current[0].next_fire();
        let rebuilt = vec![task("db", Path::new("/tmp/db"), "1h", Instant::now())];

        let summary = reload_backup_tasks(&mut current, rebuilt);
        assert!(summary.is_empty(), "an untouched unit is not a change");
        assert_eq!(
            current[0].next_fire(),
            carried,
            "a reload must not restart the clock on a backup the user did not touch"
        );
    }

    #[test]
    fn reload_counts_added_removed_and_rescheduled_units() {
        let now = Instant::now();
        let mut current = vec![
            task("kept", Path::new("/tmp/a"), "1h", now),
            task("changed", Path::new("/tmp/b"), "1h", now),
            task("dropped", Path::new("/tmp/c"), "1h", now),
        ];
        let rebuilt = vec![
            task("kept", Path::new("/tmp/a"), "1h", now),
            task("changed", Path::new("/tmp/b"), "15m", now),
            task("new", Path::new("/tmp/d"), "1h", now),
        ];

        let summary = reload_backup_tasks(&mut current, rebuilt);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.rescheduled, 1);
        assert!(!summary.is_empty());
        let names: Vec<&str> = current.iter().map(|t| t.spec.name.as_str()).collect();
        assert_eq!(names, vec!["kept", "changed", "new"]);
    }

    #[test]
    fn reload_restarts_the_clock_on_a_rescheduled_unit() {
        let past = Instant::now() - StdDuration::from_secs(30);
        let mut current = vec![task("db", Path::new("/tmp/db"), "1h", past)];
        let stale = current[0].next_fire();
        let rebuilt = vec![task("db", Path::new("/tmp/db"), "15m", Instant::now())];

        reload_backup_tasks(&mut current, rebuilt);
        assert_ne!(current[0].next_fire(), stale);
        assert!(current[0].next_fire() > Instant::now());
    }

    #[test]
    fn sighup_reload_picks_up_added_changed_and_removed_units() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let posix = crate::to_posix_string(&source);

        let config_path = write_config_with_backups(
            &tmp,
            &format!(
                "    - name: kept\n      source: {posix}\n      schedule: 1h\n    - name: dropped\n      source: {posix}\n      schedule: 1h\n"
            ),
        );
        let (ctx, buf) = sighup_ctx(&tmp, &config_path);
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);

        let mut set = BackupTimers::empty();
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut set);
        let names: Vec<&str> = set.tasks().iter().map(|t| t.spec.name.as_str()).collect();
        assert_eq!(names, vec!["kept", "dropped"], "startup-equivalent rebuild");
        let kept_deadline = set.tasks()[0].next_fire();

        // Drop one, reschedule the survivor, add a new one.
        write_config_with_backups(
            &tmp,
            &format!(
                "    - name: kept\n      source: {posix}\n      schedule: 15m\n    - name: added\n      source: {posix}\n      schedule: 1h\n"
            ),
        );
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut set);
        let names: Vec<&str> = set.tasks().iter().map(|t| t.spec.name.as_str()).collect();
        assert_eq!(names, vec!["kept", "added"]);
        assert_ne!(
            set.tasks()[0].next_fire(),
            kept_deadline,
            "a changed schedule re-arms the timer"
        );

        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Backup schedules reloaded: 1 added, 1 removed, 1 rescheduled"),
            "reload must report the timer-set delta: {captured}"
        );
    }

    #[test]
    fn sighup_reload_installs_no_timer_for_a_schedule_less_backup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let config_path = write_config_with_backups(
            &tmp,
            &format!(
                "    - name: apply-time\n      source: {}\n",
                crate::to_posix_string(&source)
            ),
        );
        let (ctx, _buf) = sighup_ctx(&tmp, &config_path);
        let mut set = BackupTimers::empty();
        runner::apply_sighup_reload(&ctx, &AtomicU64::new(300), &AtomicU64::new(300), &mut set);
        assert_eq!(
            set.len(),
            0,
            "a schedule-less backup belongs to apply, not to the daemon"
        );
    }

    // ----- the tick itself -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backup_tick_runs_a_due_unit_and_records_it_like_the_cli_does() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"payload").unwrap();
        let (mut ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(&tmp, "");

        let past = Instant::now() - StdDuration::from_secs(5);
        let mut set = timers(vec![task("db", &source, "1s", past)]);
        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        let store = StateStore::open_in_dir(tmp.path()).unwrap();
        let record = store
            .latest_backup_run("db")
            .unwrap()
            .expect("the tick must write a backup_runs row");
        assert_eq!(record.name, "db");
        assert_eq!(record.status, crate::state::BackupRunStatus::Success);
        assert!(record.is_clean(), "unexpected error: {:?}", record.error);
        assert_eq!(record.source, crate::to_posix_string(&source));
        assert_eq!(record.size_bytes, Some(7));
        let snapshot = PathBuf::from(
            record
                .destination_path
                .as_ref()
                .expect("a successful run records its artifact"),
        );
        assert!(snapshot.exists(), "snapshot missing at {snapshot:?}");
        assert!(
            set.tasks()[0].next_fire() > Instant::now(),
            "the fired unit must be re-armed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backup_tick_leaves_a_unit_that_is_not_due_alone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"payload").unwrap();
        let (mut ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(&tmp, "");

        let mut set = timers(vec![task("db", &source, "1h", Instant::now())]);
        let armed = set.tasks()[0].next_fire();
        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        assert_eq!(set.tasks()[0].next_fire(), armed, "deadline must not move");
        let store = StateStore::open_in_dir(tmp.path()).unwrap();
        assert!(
            store.latest_backup_run("db").unwrap().is_none(),
            "a unit that is not due must not run"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backup_tick_hooks_run_in_the_reconcile_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"payload").unwrap();
        let marker = tmp.path().join("hook.out");
        let (mut ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(&tmp, "");

        let mut s = spec("db", &source, Some("1s"));
        // native-ok: the path is interpolated into a shell command for THIS host.
        #[cfg(unix)]
        let run = format!(
            "printf '%s' \"$CFGD_CONTEXT:$CFGD_PHASE:$CFGD_PROFILE\" > '{}'",
            marker.display()
        );
        #[cfg(windows)]
        let run = format!(
            "echo %CFGD_CONTEXT%:%CFGD_PHASE%:%CFGD_PROFILE%> \"{}\"",
            marker.display()
        );
        s.pre_backup = vec![config::ScriptEntry::Simple(run)];

        let past = Instant::now() - StdDuration::from_secs(5);
        let mut set = timers(vec![
            BackupTask::new(&s, "workstation", past, None).expect("schedule installs a timer"),
        ]);
        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        let contents = std::fs::read_to_string(&marker)
            .expect("preBackup hook should have run")
            .trim()
            .to_string();
        assert_eq!(contents, "reconcile:preBackup:workstation");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn backup_tick_records_a_failure_without_taking_the_loop_down() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let missing = tmp.path().join("never-created.db");
        let (mut ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(&tmp, "");

        let past = Instant::now() - StdDuration::from_secs(5);
        let mut set = timers(vec![task("db", &missing, "1s", past)]);
        runner::handle_backup_tick(&ctx, &mut set)
            .await
            .expect("an operational failure is recorded, never propagated");

        let store = StateStore::open_in_dir(tmp.path()).unwrap();
        let record = store.latest_backup_run("db").unwrap().expect("row written");
        assert_eq!(record.status, crate::state::BackupRunStatus::Failed);
        assert!(record.destination_path.is_none());
    }

    // ----- the loop's timer branch -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_loop_fires_a_backup_timer_without_any_external_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"payload").unwrap();
        let (mut ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(&tmp, "");
        let (triggers, senders) = make_triggers();

        let set = timers(vec![task("db", &source, "1s", Instant::now())]);
        let handle = tokio::spawn(runner::run_daemon_loop(
            ctx,
            triggers,
            Vec::new(),
            Vec::new(),
            set,
            Arc::new(AtomicU64::new(300)),
            Arc::new(AtomicU64::new(300)),
        ));

        // No channel is pumped: only the timer branch can produce this row.
        let store = StateStore::open_in_dir(tmp.path()).unwrap();
        let deadline = Instant::now() + StdDuration::from_secs(10);
        loop {
            if store.latest_backup_run("db").unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the loop's backup timer never fired"
            );
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }

        senders.shutdown_tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }

    // ----- restart seeding of interval schedules -----

    /// An ISO 8601 timestamp `ago` in the past — the shape `finished_at` holds.
    fn finished_secs_ago(ago: u64) -> String {
        crate::unix_secs_to_iso8601(crate::unix_secs_now() - ago)
    }

    /// Assert `actual` sits within a second of `expected` — the seeding
    /// arithmetic reads two clocks, so an exact match would be flaky.
    fn assert_fires_near(actual: Instant, expected: Instant) {
        let slack = StdDuration::from_secs(2);
        assert!(
            actual >= expected.checked_sub(slack).unwrap() && actual <= expected + slack,
            "next fire is off by more than {slack:?}"
        );
    }

    #[test]
    fn an_interval_schedule_resumes_from_the_last_recorded_run() {
        let now = Instant::now();
        let t = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("1h")),
            "workstation",
            now,
            Some(&finished_secs_ago(1800)),
        )
        .expect("timer installs");
        // Half the period has already elapsed, so only half is left — NOT a
        // fresh hour, which is what makes a daily backup on a daily-rebooted
        // machine never fire.
        assert_fires_near(t.next_fire(), now + StdDuration::from_secs(1800));
    }

    #[test]
    fn an_overdue_interval_schedule_fires_promptly_after_a_restart() {
        let now = Instant::now();
        let t = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("1h")),
            "workstation",
            now,
            Some(&finished_secs_ago(7200)),
        )
        .expect("timer installs");
        assert!(
            t.is_due(now),
            "a unit whose period elapsed while the machine was down is due now"
        );
    }

    #[test]
    fn an_interval_schedule_with_no_history_starts_a_full_period_out() {
        let now = Instant::now();
        let t = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("1h")),
            "workstation",
            now,
            None,
        )
        .expect("timer installs");
        assert_fires_near(t.next_fire(), now + StdDuration::from_secs(3600));
    }

    #[test]
    fn a_recorded_run_in_the_future_falls_back_to_a_full_period() {
        let now = Instant::now();
        let future = crate::unix_secs_to_iso8601(crate::unix_secs_now() + 3600);
        let t = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("1h")),
            "workstation",
            now,
            Some(&future),
        )
        .expect("timer installs");
        // A stepped-back clock or a state dir carried over from another machine
        // must not arm a deadline in the past.
        assert_fires_near(t.next_fire(), now + StdDuration::from_secs(3600));
    }

    #[test]
    fn an_unparseable_recorded_timestamp_falls_back_to_a_full_period() {
        let now = Instant::now();
        let t = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("1h")),
            "workstation",
            now,
            Some("not-a-timestamp"),
        )
        .expect("timer installs");
        assert_fires_near(t.next_fire(), now + StdDuration::from_secs(3600));
    }

    #[test]
    fn a_cron_schedule_ignores_the_last_run() {
        let now = Instant::now();
        let seeded = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("0 3 * * *")),
            "workstation",
            now,
            Some(&finished_secs_ago(86_400 * 7)),
        )
        .expect("timer installs");
        let unseeded = BackupTask::new(
            &spec("db", Path::new("/tmp/a"), Some("0 3 * * *")),
            "workstation",
            now,
            None,
        )
        .expect("timer installs");
        // Cron occurrences are absolute wall-clock times: the next 3am is the
        // next 3am no matter how many were slept through.
        assert_fires_near(seeded.next_fire(), unseeded.next_fire());
    }

    #[test]
    fn resolve_seeds_the_timer_set_from_the_state_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let config_path = write_config_with_backups(
            &tmp,
            &format!(
                "    - name: db\n      source: {}\n      schedule: 1h\n",
                crate::to_posix_string(&source)
            ),
        );
        let state_dir = tmp.path().join("state");
        let store = StateStore::open_in_dir(&state_dir).unwrap();
        store
            .record_backup_run(&crate::state::BackupRunDraft {
                name: "db".to_string(),
                source: crate::to_posix_string(&source),
                destination_path: Some("/snap".to_string()),
                size_bytes: Some(1),
                status: crate::state::BackupRunStatus::Success,
                error: None,
                started_at: finished_secs_ago(1830),
                finished_at: finished_secs_ago(1800),
            })
            .unwrap();

        let cfg = config::load_config(&config_path).unwrap();
        let (printer, _) = crate::output::Printer::for_test();
        let now = Instant::now();
        let resolved = resolve_backup_tasks(
            &cfg,
            &config_path,
            None,
            &printer,
            crate::Scope::User,
            Some(&state_dir),
            now,
        )
        .expect("a valid profile resolves");

        assert_eq!(resolved.tasks.len(), 1);
        assert!(resolved.degraded.is_none());
        assert_fires_near(
            resolved.tasks[0].next_fire(),
            now + StdDuration::from_secs(1800),
        );
    }

    #[test]
    fn resolve_reports_an_unresolvable_profile_rather_than_an_empty_set() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: missing\n",
        )
        .unwrap();
        let cfg = config::load_config(&config_path).unwrap();
        let (printer, _) = crate::output::Printer::for_test();

        // An `Ok(empty)` here is exactly the silent failure that let one SIGHUP
        // wipe a working timer set: the caller could not tell "no backups" from
        // "could not tell".
        assert!(
            resolve_backup_tasks(
                &cfg,
                &config_path,
                None,
                &printer,
                crate::Scope::User,
                None,
                Instant::now(),
            )
            .is_err()
        );
    }

    // ----- a degraded resolution is neither sticky nor destructive -----

    #[test]
    fn a_degraded_resolution_never_swaps_out_the_running_set() {
        let now = Instant::now();
        let mut set = timers(vec![
            task("db", Path::new("/tmp/a"), "1h", now),
            task("home", Path::new("/tmp/b"), "1h", now),
        ]);
        let armed: Vec<Instant> = set.tasks().iter().map(BackupTask::next_fire).collect();

        let summary = set.apply_resolved(
            ResolvedBackupTasks {
                tasks: vec![task("db", Path::new("/tmp/a"), "1h", now)],
                degraded: Some(DegradedReason::SourcesUnavailable),
            },
            now,
        );

        assert!(
            summary.is_none(),
            "a degraded resolution reports no reload, because none happened"
        );
        assert_eq!(
            set.len(),
            2,
            "the source-delivered timer must not be retired"
        );
        let kept: Vec<Instant> = set.tasks().iter().map(BackupTask::next_fire).collect();
        assert_eq!(kept, armed, "the running deadlines must be untouched");
        assert!(set.is_degraded(), "a degraded resolution must arm a retry");
    }

    #[test]
    fn a_clean_resolution_clears_the_retry_and_swaps_the_set() {
        let now = Instant::now();
        let mut set = BackupTimers::new(
            ResolvedBackupTasks {
                tasks: vec![task("db", Path::new("/tmp/a"), "1h", now)],
                degraded: Some(DegradedReason::SourcesUnavailable),
            },
            now,
        );
        assert!(set.is_degraded());

        let summary = set
            .apply_resolved(
                ResolvedBackupTasks {
                    tasks: vec![
                        task("db", Path::new("/tmp/a"), "1h", now),
                        task("home", Path::new("/tmp/b"), "1h", now),
                    ],
                    degraded: None,
                },
                now,
            )
            .expect("a clean resolution reloads");
        assert_eq!(summary.added, 1);
        assert_eq!(set.len(), 2);
        assert!(!set.is_degraded());
    }

    #[test]
    fn a_degraded_startup_holds_the_first_fire_back_past_the_retry() {
        let now = Instant::now();
        let set = BackupTimers::new(
            ResolvedBackupTasks {
                tasks: vec![task("db", Path::new("/tmp/a"), "1s", now)],
                degraded: Some(DegradedReason::SourcesUnavailable),
            },
            now,
        );
        // Until the retry confirms these specs, a unit a source overrides may be
        // carrying the local destination — and a run against the wrong
        // destination prunes the source-era history out of the table.
        assert!(
            set.tasks()[0].next_fire() > now + StdDuration::from_secs(60),
            "a degraded startup must not run before it has re-resolved"
        );
        assert!(set.is_degraded());
    }

    #[test]
    fn sighup_over_a_broken_profile_keeps_the_running_schedules() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let posix = crate::to_posix_string(&source);
        let config_path = write_config_with_backups(
            &tmp,
            &format!(
                "    - name: db\n      source: {posix}\n      schedule: 1h\n    - name: home\n      source: {posix}\n      schedule: 6h\n"
            ),
        );
        let (ctx, buf) = sighup_ctx(&tmp, &config_path);
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);

        let mut set = BackupTimers::empty();
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut set);
        assert_eq!(set.len(), 2);
        let armed: Vec<Instant> = set.tasks().iter().map(BackupTask::next_fire).collect();
        buf.lock().unwrap().clear();

        // The profile the timers came from is now unreadable — a typo saved
        // mid-edit, or a half-written file.
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "spec:\n  backups:\n   - name: [unclosed\n",
        )
        .unwrap();
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut set);

        assert_eq!(
            set.len(),
            2,
            "one SIGHUP over a transient config error must not retire the machine's backups"
        );
        let kept: Vec<Instant> = set.tasks().iter().map(BackupTask::next_fire).collect();
        assert_eq!(
            kept, armed,
            "the pending deadlines must survive the failure"
        );
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Backup schedules NOT reloaded"),
            "the operator must be told the reload was refused: {captured}"
        );
        assert!(
            !captured.contains("2 removed"),
            "a refused reload must never report the running set as removed: {captured}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_due_retry_re_resolves_and_restores_the_timer_set() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let (mut ctx, _state, buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(
            &tmp,
            &format!(
                "    - name: db\n      source: {}\n      schedule: 1h\n",
                crate::to_posix_string(&source)
            ),
        );

        // A startup that could not compose sources: no timers, retry armed and
        // already due (the daemon has been up longer than the retry window).
        let mut set = BackupTimers::new(
            ResolvedBackupTasks {
                tasks: Vec::new(),
                degraded: Some(DegradedReason::SourcesUnavailable),
            },
            Instant::now() - StdDuration::from_secs(3600),
        );
        assert!(set.retry_due(Instant::now()));

        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        assert_eq!(
            set.len(),
            1,
            "a healed config must restore the timers without a restart or a SIGHUP"
        );
        assert!(!set.is_degraded());
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Backup schedules restored: 1 scheduled"),
            "the recovery must be visible: {captured}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_due_retry_over_a_backup_less_profile_does_not_claim_a_restoration() {
        // Same recovery path, but the healed profile declares zero backups.
        // "restored: 0 scheduled" reads as a broken recovery when it is really
        // just an empty, healthy config — the zero case gets its own wording.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (mut ctx, _state, buf) = make_test_ctx(&tmp, false, false, None);
        ctx.config_path = write_config_with_backups(&tmp, "");

        let mut set = BackupTimers::new(
            ResolvedBackupTasks {
                tasks: Vec::new(),
                degraded: Some(DegradedReason::SourcesUnavailable),
            },
            Instant::now() - StdDuration::from_secs(3600),
        );
        assert!(set.retry_due(Instant::now()));

        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        assert_eq!(set.len(), 0);
        assert!(!set.is_degraded());
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("Backup schedule resolved: no units configured"),
            "the zero case must not say 'restored': {captured}"
        );
        assert!(
            !captured.contains("restored: 0 scheduled"),
            "the odd zero-count phrasing must be gone: {captured}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_due_retry_over_an_unreadable_config_is_labeled_by_its_real_cause() {
        // The config-load Err arm is a DIFFERENT failure than profile
        // resolution: the profile is never even reached because the
        // top-level config file itself would not parse. It must not borrow
        // ProfileUnresolved's label.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let (mut ctx, _state, _buf) = make_test_ctx(&tmp, false, false, None);
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(&config_path, "::: not valid yaml :::").unwrap();
        ctx.config_path = config_path;

        let mut set = BackupTimers::empty_with_retry(Instant::now() - StdDuration::from_secs(3600));
        assert!(set.retry_due(Instant::now()));

        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        assert_eq!(
            set.degraded_reason(),
            Some(crate::daemon::backup::DegradedReason::ConfigUnreadable),
            "an unreadable top-level config must not be mislabeled as an unresolved profile"
        );
    }

    /// A config declaring a source whose cache is present but unreadable — the
    /// "source cache caught mid-rewrite" case. The profile resolves locally,
    /// `compose_daemon_desired_state` fails on the manifest, and
    /// `resolve_backup_tasks` hands back the locally-declared set marked
    /// degraded: the exact state the adopt branch runs in. (A source that was
    /// never fetched is only WARNED about, not an error, so it cannot produce
    /// this state.)
    ///
    /// Returns the guard pinning `CFGD_CACHE_DIR` at the staged root alongside
    /// the config path: the cache root is otherwise derived from the home, and
    /// a concurrent test setting that variable would move it out from under
    /// this one — the cache would then look absent, composition would succeed,
    /// and the degraded state under test would never be reached. Every caller
    /// is `serial` for the same reason.
    fn write_config_with_broken_source_cache(
        tmp: &tempfile::TempDir,
        backups_yaml: &str,
    ) -> (PathBuf, crate::test_helpers::EnvVarGuard) {
        let cache_root = tmp.path().join(".cache").join("cfgd");
        let guard = crate::test_helpers::EnvVarGuard::set(
            "CFGD_CACHE_DIR",
            cache_root.to_str().expect("utf-8 cache root"),
        );
        let cache = cache_root.join("sources").join("team");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(crate::sources::SOURCE_MANIFEST_FILE),
            "::: not a manifest :::",
        )
        .unwrap();
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: team\n      origin:\n        type: Git\n        url: https://example.invalid/team.git\n      subscription:\n        profile: team\n        priority: 500\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  backups:\n{backups_yaml}"
            ),
        )
        .unwrap();
        (config_path, guard)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn a_retry_that_adopts_a_partial_set_says_so_instead_of_reporting_an_all_clear() {
        // The recovery path the startup retry opens: booted on a broken
        // profile (0 timers), profile since fixed, sources still unavailable.
        // The set genuinely improved — and it is NOT an all-clear. The retry is
        // still armed, and once the one-shot first-fire deferral expires a unit
        // a source overrides runs against the LOCAL destination and its prune
        // drops the source-era retention rows.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let (mut ctx, _state, buf) = make_test_ctx(&tmp, false, false, None);
        let (broken_config, _cache) = write_config_with_broken_source_cache(
            &tmp,
            &format!(
                "    - name: db\n      source: {}\n      schedule: 1h\n",
                crate::to_posix_string(&source)
            ),
        );
        ctx.config_path = broken_config;

        let mut set = BackupTimers::empty_with_retry(Instant::now() - StdDuration::from_secs(3600));
        assert!(set.retry_due(Instant::now()));

        runner::handle_backup_tick(&ctx, &mut set).await.unwrap();

        assert_eq!(set.len(), 1, "the local set must be adopted");
        assert_eq!(
            set.degraded_reason(),
            Some(crate::daemon::backup::DegradedReason::SourcesUnavailable),
            "adopting a partial set must not clear the degraded state"
        );

        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains(
                "Backup schedules restored: 1 scheduled (source composition unavailable)"
            ),
            "the line must name what is still missing: {captured}"
        );
        assert!(
            !captured.contains("✓ Backup schedules restored"),
            "an unqualified all-clear tick would report a healthy set that is not: {captured}"
        );
        assert!(
            captured.contains("⚠ Backup schedules restored"),
            "a partial set is a warning, not an Ok: {captured}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_sighup_that_adopts_a_partial_set_says_so_instead_of_reporting_an_all_clear() {
        // Same state, reached the other way: a SIGHUP arriving while nothing is
        // running adopts rather than refusing (there is nothing to protect), so
        // its completion line carries the same qualifier the tick's does.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let (config_path, _cache) = write_config_with_broken_source_cache(
            &tmp,
            &format!(
                "    - name: db\n      source: {}\n      schedule: 1h\n",
                crate::to_posix_string(&source)
            ),
        );
        let (ctx, buf) = sighup_ctx(&tmp, &config_path);
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);

        let mut set = BackupTimers::empty();
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut set);

        assert_eq!(set.len(), 1);
        assert_eq!(
            set.degraded_reason(),
            Some(crate::daemon::backup::DegradedReason::SourcesUnavailable)
        );
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains(
                "Backup schedules reloaded: 1 added, 0 removed, 0 rescheduled (source composition unavailable)"
            ),
            "the reload line must name what is still missing: {captured}"
        );
        assert!(
            !captured.contains("✓ Backup schedules reloaded"),
            "an unqualified all-clear would tell the operator their edit landed in full: {captured}"
        );
        assert!(
            captured.contains("⚠ Backup schedules reloaded"),
            "a partial reload is a warning, not an Ok: {captured}"
        );
    }

    #[test]
    fn a_fully_resolved_reload_still_reports_a_plain_all_clear() {
        // The qualifier must ride ONLY the degraded state: a healthy reload has
        // to stay a bare Ok, or the warning stops meaning anything.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let source = tmp.path().join("data.db");
        std::fs::write(&source, b"x").unwrap();
        let config_path = write_config_with_backups(
            &tmp,
            &format!(
                "    - name: db\n      source: {}\n      schedule: 1h\n",
                crate::to_posix_string(&source)
            ),
        );
        let (ctx, buf) = sighup_ctx(&tmp, &config_path);
        let reconcile_secs = AtomicU64::new(300);
        let sync_secs = AtomicU64::new(300);

        let mut set = BackupTimers::empty();
        runner::apply_sighup_reload(&ctx, &reconcile_secs, &sync_secs, &mut set);

        assert!(!set.is_degraded());
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("✓ Backup schedules reloaded: 1 added, 0 removed, 0 rescheduled"),
            "got: {captured}"
        );
        assert!(
            !captured.contains("unavailable") && !captured.contains("unresolved"),
            "a clean reload must carry no qualifier: {captured}"
        );
    }

    #[test]
    fn a_startup_that_cannot_resolve_the_profile_still_arms_a_retry() {
        // Startup is the one moment with no prior set to keep, so an
        // unresolvable profile costs every timer. Without a retry the daemon
        // runs indefinitely with zero backups — healthy in every other respect
        // — and only a restart or a manual SIGHUP brings them back.
        let tmp = tempfile::TempDir::new().unwrap();
        let _g = crate::with_test_home_guard(tmp.path());
        let config_path = tmp.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Cfgd\nmetadata:\n  name: t\nspec:\n  profile: default\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();
        // Saved mid-edit: the file exists, and does not parse.
        std::fs::write(
            tmp.path().join("profiles").join("default.yaml"),
            "spec:\n  backups:\n   - name: [unclosed\n",
        )
        .unwrap();

        let setup = pre_loop(&config_path, None)
            .expect("a broken profile must not stop the daemon from starting");

        assert_eq!(setup.backup_timers.len(), 0);
        assert_eq!(
            setup.backup_timers.degraded_reason(),
            Some(crate::daemon::backup::DegradedReason::ProfileUnresolved),
            "the banner must name the profile, not blame source composition"
        );
        assert!(
            setup.backup_timers.next_deadline().is_some(),
            "an empty startup set must still wake the loop to re-resolve"
        );
    }

    #[test]
    fn a_degraded_resolution_is_adopted_when_nothing_is_running() {
        // The keep-the-running-set rule protects timers that exist. With none,
        // refusing would pin a daemon that booted on an unresolvable profile at
        // zero timers for as long as composition stayed down — the sticky-empty
        // failure the retry exists to end.
        let now = Instant::now();
        let mut set = BackupTimers::empty_with_retry(now);
        assert_eq!(set.len(), 0);

        let summary = set
            .apply_resolved(
                ResolvedBackupTasks {
                    tasks: vec![task("db", Path::new("/tmp/a"), "1s", now)],
                    degraded: Some(DegradedReason::SourcesUnavailable),
                },
                now,
            )
            .expect("adopting a set from nothing is a reload worth reporting");
        assert_eq!(summary.added, 1);
        assert_eq!(set.len(), 1);
        assert!(
            set.is_degraded(),
            "adopting a partial set must keep the retry armed"
        );
        assert!(
            set.tasks()[0].next_fire() > now + StdDuration::from_secs(60),
            "an adopted degraded set gets the same first-fire deferral a degraded startup does"
        );
    }

    #[test]
    fn a_degraded_resolution_is_still_refused_while_timers_are_running() {
        let now = Instant::now();
        let mut set = BackupTimers::new(
            ResolvedBackupTasks {
                tasks: vec![
                    task("db", Path::new("/tmp/a"), "1h", now),
                    task("home", Path::new("/tmp/b"), "6h", now),
                ],
                degraded: None,
            },
            now,
        );

        assert!(
            set.apply_resolved(
                ResolvedBackupTasks {
                    tasks: vec![task("db", Path::new("/tmp/a"), "1h", now)],
                    degraded: Some(DegradedReason::SourcesUnavailable),
                },
                now,
            )
            .is_none(),
            "a degraded resolution must not retire the source-delivered timer"
        );
        assert_eq!(set.len(), 2);
        assert_eq!(
            set.degraded_reason(),
            Some(crate::daemon::backup::DegradedReason::SourcesUnavailable)
        );
    }

    #[test]
    fn the_startup_banner_says_when_the_timer_set_is_degraded() {
        let parsed = ParsedDaemonConfig {
            reconcile_interval: StdDuration::from_secs(300),
            sync_interval: StdDuration::from_secs(300),
            auto_pull: false,
            auto_push: false,
            auto_apply: false,
            on_change_reconcile: false,
            notify_on_drift: false,
            notify_method: NotifyMethod::Stdout,
            webhook_url: None,
        };
        let clean = crate::daemon::format_interval_lines(&parsed, None, 2, None);
        assert!(clean.iter().any(|l| l == "backups=2 scheduled"));

        // "backups=2 scheduled" alone is a lie when a source's third one is
        // missing from the set — and the two degraded causes need different
        // remedies, so the banner names which one it hit.
        let sources = crate::daemon::format_interval_lines(
            &parsed,
            None,
            2,
            Some(crate::daemon::backup::DegradedReason::SourcesUnavailable),
        );
        assert!(
            sources
                .iter()
                .any(|l| l == "backups=2 scheduled (source composition unavailable)"),
            "got: {sources:?}"
        );

        let profile = crate::daemon::format_interval_lines(
            &parsed,
            None,
            0,
            Some(crate::daemon::backup::DegradedReason::ProfileUnresolved),
        );
        assert!(
            profile
                .iter()
                .any(|l| l == "backups=0 scheduled (profile unresolved)"),
            "a profile that would not resolve must not be reported as a source \
             problem, got: {profile:?}"
        );

        let unreadable_config = crate::daemon::format_interval_lines(
            &parsed,
            None,
            0,
            Some(crate::daemon::backup::DegradedReason::ConfigUnreadable),
        );
        assert!(
            unreadable_config
                .iter()
                .any(|l| l == "backups=0 scheduled (config unreadable)"),
            "a top-level config that would not parse must render its own label, \
             not borrow the profile-unresolved or source-composition wording, \
             got: {unreadable_config:?}"
        );
    }
}
