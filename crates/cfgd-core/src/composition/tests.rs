use super::*;
use crate::config::*;

fn make_local_profile() -> ResolvedProfile {
    ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "editor".into(),
                    value: "vim".into(),
                }],
                packages: Some(PackagesSpec {
                    cargo: Some(CargoSpec {
                        file: None,
                        packages: vec!["bat".into()],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            env: vec![EnvVar {
                name: "editor".into(),
                value: "vim".into(),
            }],
            packages: PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

fn make_source_input(name: &str, priority: u32) -> CompositionInput {
    CompositionInput {
        source_name: name.into(),
        priority,
        policy: ConfigSourcePolicy {
            required: PolicyItems {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["git-secrets".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            recommended: PolicyItems {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["k9s".into(), "stern".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                env: vec![EnvVar {
                    name: "EDITOR".into(),
                    value: "code --wait".into(),
                }],
                ..Default::default()
            },
            locked: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "security/policy.yaml".into(),
                    target: "~/.config/company/security-policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    }
}

#[test]
fn compose_with_no_sources() {
    let local = make_local_profile();
    let result = compose(&local, &[]).unwrap();
    assert_eq!(
        result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "editor")
            .map(|e| &e.value),
        Some(&"vim".to_string())
    );
    assert!(result.conflicts.is_empty());
}

#[test]
fn compose_applies_required_packages() {
    let local = make_local_profile();
    let input = make_source_input("acme", 500);
    let result = compose(&local, &[input]).unwrap();

    let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
    assert!(brew.formulae.contains(&"git-secrets".into()));
}

#[test]
fn compose_applies_recommended_when_accepted() {
    let local = make_local_profile();
    let input = make_source_input("acme", 500);
    let result = compose(&local, &[input]).unwrap();

    let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
    assert!(brew.formulae.contains(&"k9s".into()));
    assert!(brew.formulae.contains(&"stern".into()));
}

#[test]
fn compose_skips_recommended_when_not_accepted() {
    let local = make_local_profile();
    let mut input = make_source_input("acme", 500);
    input.subscription.accept_recommended = false;
    let result = compose(&local, &[input]).unwrap();

    // k9s should NOT be present (recommended not accepted)
    let has_k9s = result
        .resolved
        .merged
        .packages
        .brew
        .as_ref()
        .map(|b| b.formulae.contains(&"k9s".into()))
        .unwrap_or(false);
    assert!(!has_k9s);
}

#[test]
fn compose_rejects_recommended_packages() {
    let local = make_local_profile();
    let mut input = make_source_input("acme", 500);

    // Reject kubectx from recommended
    let reject_yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
        packages:
          brew:
            formulae:
              - stern
        "#,
    )
    .unwrap();
    input.subscription.reject = reject_yaml;

    let result = compose(&local, &[input]).unwrap();
    let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
    assert!(brew.formulae.contains(&"k9s".into()));
    assert!(!brew.formulae.contains(&"stern".into()));
}

#[test]
fn compose_records_locked_conflicts() {
    let local = make_local_profile();
    let input = make_source_input("acme", 500);
    let result = compose(&local, &[input]).unwrap();

    let locked_conflicts: Vec<_> = result
        .conflicts
        .iter()
        .filter(|c| c.resolution_type == ResolutionType::Locked)
        .collect();
    assert!(!locked_conflicts.is_empty());
}

#[test]
fn compose_is_deterministic() {
    let local = make_local_profile();
    let input1 = make_source_input("acme", 500);
    let input2 = make_source_input("acme", 500);

    let result1 = compose(&local, &[input1]).unwrap();
    let result2 = compose(&local, &[input2]).unwrap();

    // Same packages in same order
    assert_eq!(
        result1.resolved.merged.packages.cargo,
        result2.resolved.merged.packages.cargo
    );
}

#[test]
fn validate_constraints_scripts_blocked() {
    let constraints = SourceConstraints {
        no_scripts: true,
        ..Default::default()
    };
    let spec = ProfileSpec {
        scripts: Some(ScriptSpec {
            pre_reconcile: vec![ScriptEntry::Simple("setup.sh".to_string())],
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = validate_constraints("acme", &constraints, &spec, false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("acme") && msg.contains("scripts"),
        "error should mention source name and scripts: {msg}"
    );
}

#[test]
fn validate_constraints_scripts_blocked_all_hooks() {
    // Verify ALL script hook types are checked, not just pre_reconcile
    let constraints = SourceConstraints {
        no_scripts: true,
        ..Default::default()
    };
    for (label, spec) in [
        (
            "post_apply",
            ProfileSpec {
                scripts: Some(ScriptSpec {
                    post_apply: vec![ScriptEntry::Simple("hook.sh".into())],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "on_drift",
            ProfileSpec {
                scripts: Some(ScriptSpec {
                    on_drift: vec![ScriptEntry::Simple("drift.sh".into())],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "on_change",
            ProfileSpec {
                scripts: Some(ScriptSpec {
                    on_change: vec![ScriptEntry::Simple("change.sh".into())],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ] {
        assert!(
            validate_constraints("src", &constraints, &spec, false).is_err(),
            "no_scripts should block {label} hooks"
        );
    }
}

#[test]
fn validate_constraints_scripts_empty_allowed() {
    // no_scripts=true but spec has Scripts with all empty vecs — should pass
    let constraints = SourceConstraints {
        no_scripts: true,
        ..Default::default()
    };
    let spec = ProfileSpec {
        scripts: Some(ScriptSpec::default()),
        ..Default::default()
    };
    assert!(
        validate_constraints("acme", &constraints, &spec, false).is_ok(),
        "no_scripts with empty script lists should pass"
    );
}

#[test]
fn validate_constraints_scripts_allowed() {
    let constraints = SourceConstraints {
        no_scripts: false,
        ..Default::default()
    };
    let spec = ProfileSpec {
        scripts: Some(ScriptSpec {
            pre_reconcile: vec![ScriptEntry::Simple("setup.sh".to_string())],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(validate_constraints("acme", &constraints, &spec, false).is_ok());
}

#[test]
fn validate_constraints_scripts_permitted_by_subscriber_opt_in() {
    // no_scripts=true would normally reject, but allowScripts opt-in (the 4th
    // arg = true) permits profile-layer scripts.
    let constraints = SourceConstraints {
        no_scripts: true,
        ..Default::default()
    };
    let spec = ProfileSpec {
        scripts: Some(ScriptSpec {
            pre_reconcile: vec![ScriptEntry::Simple("setup.sh".to_string())],
            ..Default::default()
        }),
        ..Default::default()
    };
    // Blocked without opt-in, permitted with it.
    assert!(validate_constraints("acme", &constraints, &spec, false).is_err());
    assert!(
        validate_constraints("acme", &constraints, &spec, true).is_ok(),
        "allowScripts opt-in must permit profile-layer scripts under no_scripts"
    );
}

#[test]
fn validate_constraints_path_containment() {
    let constraints = SourceConstraints {
        allowed_target_paths: vec!["~/.config/acme/".into()],
        ..Default::default()
    };
    let spec = ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "evil.sh".into(),
                target: "/etc/sudoers".into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = validate_constraints("acme", &constraints, &spec, false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("/etc/sudoers") && msg.contains("acme"),
        "error should mention the offending path and source: {msg}"
    );
}

#[test]
fn validate_constraints_path_allowed() {
    let constraints = SourceConstraints {
        allowed_target_paths: vec!["~/.config/acme/*".into()],
        ..Default::default()
    };
    let spec = ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "config.yaml".into(),
                target: "~/.config/acme/config.yaml".into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(validate_constraints("acme", &constraints, &spec, false).is_ok());
}

#[test]
fn validate_constraints_system_changes_blocked() {
    let constraints = SourceConstraints {
        allow_system_changes: false,
        ..Default::default()
    };
    let spec = ProfileSpec {
        system: HashMap::from([("shell".into(), serde_yaml::Value::String("/bin/zsh".into()))]),
        ..Default::default()
    };
    let err = validate_constraints("acme", &constraints, &spec, false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("acme") && msg.contains("system setting") && msg.contains("shell"),
        "error should name source, mention system setting, and name the offending key: {msg}"
    );
}

#[test]
fn validate_constraints_system_changes_allowed() {
    // allow_system_changes defaults to true
    let constraints = SourceConstraints {
        allow_system_changes: true,
        ..Default::default()
    };
    let spec = ProfileSpec {
        system: HashMap::from([("shell".into(), serde_yaml::Value::String("/bin/zsh".into()))]),
        ..Default::default()
    };
    assert!(validate_constraints("acme", &constraints, &spec, false).is_ok());
}

#[test]
fn path_matches_glob_pattern() {
    assert!(path_matches_any(
        "~/.config/acme/config.yaml",
        &["~/.config/acme/*".into()]
    ));
    assert!(!path_matches_any(
        "/etc/sudoers",
        &["~/.config/acme/*".into()]
    ));
}

#[test]
fn path_matches_prefix() {
    assert!(path_matches_any(
        "~/.config/acme/deep/file.yaml",
        &["~/.config/acme/".into()]
    ));
}

#[test]
fn path_matches_exact() {
    assert!(path_matches_any(
        "~/.eslintrc.json",
        &["~/.eslintrc.json".into()]
    ));
}

#[test]
fn filter_rejected_removes_packages() {
    let recommended = PolicyItems {
        packages: Some(PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["k9s".into(), "stern".into(), "kubectx".into()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let reject: serde_yaml::Value = serde_yaml::from_str(
        r#"
        packages:
          brew:
            formulae:
              - kubectx
        "#,
    )
    .unwrap();

    let filtered = filter_rejected(&recommended, &reject);
    let brew = filtered.packages.unwrap().brew.unwrap();
    assert!(brew.formulae.contains(&"k9s".into()));
    assert!(brew.formulae.contains(&"stern".into()));
    assert!(!brew.formulae.contains(&"kubectx".into()));
}

#[test]
fn filter_rejected_noop_on_null() {
    let recommended = PolicyItems {
        env: vec![EnvVar {
            name: "EDITOR".into(),
            value: "code".into(),
        }],
        ..Default::default()
    };

    let filtered = filter_rejected(&recommended, &serde_yaml::Value::Null);
    assert_eq!(filtered.env.len(), 1);
}

#[test]
fn multiple_sources_priority_ordering() {
    let local = make_local_profile();
    let source_a = CompositionInput {
        source_name: "alpha".into(),
        priority: 300,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "theme".into(),
                    value: "dark".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let source_b = CompositionInput {
        source_name: "beta".into(),
        priority: 700,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "theme".into(),
                    value: "light".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source_a, source_b]).unwrap();
    // Local (1000) wins over both sources, so "editor" = "vim" still
    assert_eq!(
        result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "editor")
            .map(|e| &e.value),
        Some(&"vim".to_string())
    );
    // Between sources, local wins (priority 1000), but for "theme" which is only
    // in sources, higher priority (beta=700) processed after lower priority (alpha=300)
    // Local layers are priority 1000 so processed last, but "theme" isn't in local
    // so beta's value should remain
    assert_eq!(
        result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "theme")
            .map(|e| &e.value),
        Some(&"light".to_string())
    );
}

#[test]
fn required_resource_cannot_be_overridden() {
    let local = make_local_profile();
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            required: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "corp/policy.yaml".into(),
                    target: "~/.config/policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    // Second source tries to override the same file
    let source_b = CompositionInput {
        source_name: "rogue".into(),
        priority: 600,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "rogue/policy.yaml".into(),
                    target: "~/.config/policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source, source_b]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("required resource"));
    assert!(err.contains("policy.yaml"));
}

#[test]
fn file_conflict_between_sources_records_resolution() {
    let local = make_local_profile();
    let source_a = CompositionInput {
        source_name: "alpha".into(),
        priority: 300,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "alpha/tool.conf".into(),
                    target: "~/.config/tool.conf".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let source_b = CompositionInput {
        source_name: "beta".into(),
        priority: 700,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "beta/tool.conf".into(),
                    target: "~/.config/tool.conf".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source_a, source_b]).unwrap();
    // Beta wins (higher priority)
    let file = result
        .resolved
        .merged
        .files
        .managed
        .iter()
        .find(|f| f.target.to_string_lossy().contains("tool.conf"))
        .unwrap();
    assert_eq!(file.source, "beta/tool.conf");
    // Conflict recorded
    let conflict = result
        .conflicts
        .iter()
        .find(|c| c.resource_id.contains("tool.conf"));
    assert!(conflict.is_some());
    assert_eq!(conflict.unwrap().winning_source, "beta");
}

#[test]
fn equal_priority_file_conflict_is_unresolvable() {
    let local = make_local_profile();
    let source_a = CompositionInput {
        source_name: "team-a".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "team-a/settings.json".into(),
                    target: "~/.config/settings.json".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let source_b = CompositionInput {
        source_name: "team-b".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "team-b/settings.json".into(),
                    target: "~/.config/settings.json".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source_a, source_b]);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("conflict"));
    assert!(err.contains("settings.json"));
}

#[test]
fn required_modules_always_included() {
    let local = make_local_profile();
    let mut source = make_source_input("acme", 500);
    source.policy.required.modules = vec!["corp-vpn".into(), "corp-certs".into()];
    let result = compose(&local, &[source]).unwrap();
    assert!(
        result
            .resolved
            .merged
            .modules
            .contains(&"corp-vpn".to_string())
    );
    assert!(
        result
            .resolved
            .merged
            .modules
            .contains(&"corp-certs".to_string())
    );
}

#[test]
fn recommended_modules_included_when_accepted() {
    let local = make_local_profile();
    let mut source = make_source_input("acme", 500);
    source.policy.recommended.modules = vec!["editor".into()];
    source.subscription.accept_recommended = true;
    let result = compose(&local, &[source]).unwrap();
    assert!(
        result
            .resolved
            .merged
            .modules
            .contains(&"editor".to_string())
    );
}

#[test]
fn recommended_modules_rejected() {
    let local = make_local_profile();
    let mut source = make_source_input("acme", 500);
    source.policy.recommended.modules = vec!["editor".into()];
    source.subscription.accept_recommended = true;
    source.subscription.reject = serde_yaml::from_str("modules: [editor]").unwrap();
    let result = compose(&local, &[source]).unwrap();
    assert!(
        !result
            .resolved
            .merged
            .modules
            .contains(&"editor".to_string())
    );
    // Verify rejection is recorded in conflicts
    assert!(
        result
            .conflicts
            .iter()
            .any(|c| c.resource_id == "module:editor"
                && c.resolution_type == ResolutionType::Rejected)
    );
}

#[test]
fn module_policy_conflicts_recorded() {
    let local = make_local_profile();
    let mut source = make_source_input("acme", 500);
    source.policy.required.modules = vec!["corp-vpn".into()];
    let result = compose(&local, &[source]).unwrap();
    assert!(result.conflicts.iter().any(
        |c| c.resource_id == "module:corp-vpn" && c.resolution_type == ResolutionType::Required
    ));
}

#[test]
fn local_modules_and_source_modules_union() {
    let mut local = make_local_profile();
    local.layers[0].spec.modules = vec!["nvim".into()];
    local.merged.modules = vec!["nvim".into()];
    let mut source = make_source_input("acme", 500);
    source.policy.required.modules = vec!["corp-vpn".into()];
    let result = compose(&local, &[source]).unwrap();
    assert!(result.resolved.merged.modules.contains(&"nvim".to_string()));
    assert!(
        result
            .resolved
            .merged
            .modules
            .contains(&"corp-vpn".to_string())
    );
}

#[test]
fn count_policy_tier_items_includes_modules() {
    let items = PolicyItems {
        modules: vec!["a".into(), "b".into()],
        ..Default::default()
    };
    assert_eq!(count_policy_tier_items(&items), 2);
}

// --- Encryption constraint tests ---

fn make_encryption_constraint(patterns: &[&str], backend: Option<&str>) -> SourceConstraints {
    SourceConstraints {
        encryption: Some(crate::config::EncryptionConstraint {
            required_targets: patterns.iter().map(|s| s.to_string()).collect(),
            backend: backend.map(|s| s.to_string()),
            mode: None,
        }),
        ..Default::default()
    }
}

fn make_file_spec_with_encryption(target: &str, backend: Option<&str>) -> ProfileSpec {
    ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "source/file".into(),
                target: target.into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: backend.map(|b| crate::config::EncryptionSpec {
                    backend: b.to_string(),
                    mode: crate::config::EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn encryption_required_target_without_encryption_is_error() {
    let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
    let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
    let result = validate_constraints("corp", &constraints, &spec, false);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("~/.ssh/id_rsa"), "msg: {msg}");
    assert!(msg.contains("~/.ssh/*"), "msg: {msg}");
    assert!(msg.contains("encryption"), "msg: {msg}");
}

#[test]
fn encryption_required_target_with_encryption_passes() {
    let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
    let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", Some("sops"));
    assert!(validate_constraints("corp", &constraints, &spec, false).is_ok());
}

#[test]
fn encryption_non_matching_target_without_encryption_passes() {
    let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
    // ~/.zshrc does not match ~/.ssh/* — no enforcement
    let spec = make_file_spec_with_encryption("~/.zshrc", None);
    assert!(validate_constraints("corp", &constraints, &spec, false).is_ok());
}

#[test]
fn encryption_empty_required_targets_no_enforcement() {
    let constraints = SourceConstraints {
        encryption: Some(crate::config::EncryptionConstraint {
            required_targets: vec![],
            backend: Some("sops".into()),
            mode: None,
        }),
        ..Default::default()
    };
    // Even though a backend is specified, empty requiredTargets means no enforcement
    let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
    assert!(validate_constraints("corp", &constraints, &spec, false).is_ok());
}

#[test]
fn encryption_no_constraint_field_no_enforcement() {
    let constraints = SourceConstraints::default();
    // No encryption constraint at all
    let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
    assert!(validate_constraints("corp", &constraints, &spec, false).is_ok());
}

#[test]
fn encryption_wrong_backend_is_error() {
    let constraints = make_encryption_constraint(&["~/.aws/*"], Some("sops"));
    let spec = make_file_spec_with_encryption("~/.aws/credentials", Some("age"));
    let result = validate_constraints("corp", &constraints, &spec, false);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("~/.aws/credentials"), "msg: {msg}");
    assert!(msg.contains("age"), "msg: {msg}");
    assert!(msg.contains("sops"), "msg: {msg}");
}

#[test]
fn encryption_correct_backend_passes() {
    let constraints = make_encryption_constraint(&["~/.aws/*"], Some("sops"));
    let spec = make_file_spec_with_encryption("~/.aws/credentials", Some("sops"));
    assert!(validate_constraints("corp", &constraints, &spec, false).is_ok());
}

#[test]
fn encryption_constraint_matches_exact_path() {
    let constraints = make_encryption_constraint(&["~/.gnupg/secring.gpg"], None);
    // Exact path match
    let spec = make_file_spec_with_encryption("~/.gnupg/secring.gpg", None);
    let result = validate_constraints("corp", &constraints, &spec, false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("~/.gnupg/secring.gpg")
    );
}

// --- Determinism: full merged output comparison ---

#[test]
fn compose_is_deterministic_full_output() {
    let local = make_local_profile();
    let input1 = make_source_input("acme", 500);
    let input2 = make_source_input("acme", 500);

    let result1 = compose(&local, &[input1]).unwrap();
    let result2 = compose(&local, &[input2]).unwrap();

    // Serialize both merged profiles and compare the full output
    let yaml1 = serde_yaml::to_string(&result1.resolved.merged).unwrap();
    let yaml2 = serde_yaml::to_string(&result2.resolved.merged).unwrap();
    assert_eq!(
        yaml1, yaml2,
        "Full merged output must be identical across runs"
    );

    // Also check conflict counts and types match
    assert_eq!(result1.conflicts.len(), result2.conflicts.len());
    for (c1, c2) in result1.conflicts.iter().zip(result2.conflicts.iter()) {
        assert_eq!(c1.resource_id, c2.resource_id);
        assert_eq!(c1.resolution_type, c2.resolution_type);
        assert_eq!(c1.winning_source, c2.winning_source);
    }
}

#[test]
fn compose_deterministic_with_multiple_sources() {
    let local = make_local_profile();
    // Run twice with same sources in same order
    let mk = || {
        vec![
            CompositionInput {
                source_name: "alpha".into(),
                priority: 300,
                policy: ConfigSourcePolicy {
                    recommended: PolicyItems {
                        packages: Some(PackagesSpec {
                            brew: Some(BrewSpec {
                                formulae: vec!["ripgrep".into(), "fd".into()],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        env: vec![EnvVar {
                            name: "PAGER".into(),
                            value: "less".into(),
                        }],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                constraints: Default::default(),
                layers: vec![],
                subscription: SubscriptionConfig {
                    accept_recommended: true,
                    ..Default::default()
                },
                allow_scripts: false,
            },
            CompositionInput {
                source_name: "beta".into(),
                priority: 700,
                policy: ConfigSourcePolicy {
                    recommended: PolicyItems {
                        packages: Some(PackagesSpec {
                            brew: Some(BrewSpec {
                                formulae: vec!["jq".into()],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        env: vec![EnvVar {
                            name: "PAGER".into(),
                            value: "bat".into(),
                        }],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                constraints: Default::default(),
                layers: vec![],
                subscription: SubscriptionConfig {
                    accept_recommended: true,
                    ..Default::default()
                },
                allow_scripts: false,
            },
        ]
    };

    let r1 = compose(&local, &mk()).unwrap();
    let r2 = compose(&local, &mk()).unwrap();
    let yaml1 = serde_yaml::to_string(&r1.resolved.merged).unwrap();
    let yaml2 = serde_yaml::to_string(&r2.resolved.merged).unwrap();
    assert_eq!(yaml1, yaml2);
}

// --- Conflict resolution: env var priority ---

#[test]
fn higher_priority_source_wins_env_var() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };
    let low = CompositionInput {
        source_name: "low".into(),
        priority: 200,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "THEME".into(),
                    value: "solarized".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let high = CompositionInput {
        source_name: "high".into(),
        priority: 800,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "THEME".into(),
                    value: "dracula".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[low, high]).unwrap();
    let theme = result
        .resolved
        .merged
        .env
        .iter()
        .find(|e| e.name == "THEME")
        .expect("THEME env var must exist");
    // Higher priority (800) source processed after lower (200), so it overwrites
    assert_eq!(theme.value, "dracula");
}

#[test]
fn local_env_wins_over_source_env_at_same_name() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "EDITOR".into(),
                    value: "nvim".into(),
                }],
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            env: vec![EnvVar {
                name: "EDITOR".into(),
                value: "nvim".into(),
            }],
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "EDITOR".into(),
                    value: "code --wait".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    let editor = result
        .resolved
        .merged
        .env
        .iter()
        .find(|e| e.name == "EDITOR")
        .expect("EDITOR env var must exist");
    // Local priority 1000 > source priority 500
    assert_eq!(editor.value, "nvim");
}

// --- Conflict resolution: files with different content ---

#[test]
fn higher_priority_source_wins_file_content() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };
    let low = CompositionInput {
        source_name: "low-src".into(),
        priority: 200,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "low/gitconfig".into(),
                    target: "~/.gitconfig".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let high = CompositionInput {
        source_name: "high-src".into(),
        priority: 800,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "high/gitconfig".into(),
                    target: "~/.gitconfig".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[low, high]).unwrap();
    let file = result
        .resolved
        .merged
        .files
        .managed
        .iter()
        .find(|f| f.target.to_string_lossy().contains("gitconfig"))
        .expect("gitconfig file must exist in merged output");
    // Higher priority source's file content wins
    assert_eq!(file.source, "high/gitconfig");
}

// --- Merging with an empty source ---

#[test]
fn merging_with_empty_source_does_not_affect_result() {
    let local = make_local_profile();
    let result_no_sources = compose(&local, &[]).unwrap();

    let empty_source = CompositionInput {
        source_name: "empty".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    };
    let result_with_empty = compose(&local, &[empty_source]).unwrap();

    let yaml_without = serde_yaml::to_string(&result_no_sources.resolved.merged).unwrap();
    let yaml_with = serde_yaml::to_string(&result_with_empty.resolved.merged).unwrap();
    assert_eq!(
        yaml_without, yaml_with,
        "Empty source must not change the merged output"
    );
}

// --- Edge case: single source ---

#[test]
fn single_source_merges_correctly() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["git".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["git".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "tools".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["fzf".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                env: vec![EnvVar {
                    name: "FZF_DEFAULT_OPTS".into(),
                    value: "--height 40%".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
    assert!(brew.formulae.contains(&"git".to_string()));
    assert!(brew.formulae.contains(&"fzf".to_string()));
    assert!(
        result
            .resolved
            .merged
            .env
            .iter()
            .any(|e| e.name == "FZF_DEFAULT_OPTS")
    );
}

// --- Edge case: overlapping packages across sources ---

#[test]
fn overlapping_packages_are_unioned_not_duplicated() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["git".into(), "curl".into()],
                        ..Default::default()
                    }),
                    pipx: vec!["black".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            packages: PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["git".into(), "curl".into()],
                    ..Default::default()
                }),
                pipx: vec!["black".into()],
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let source_a = CompositionInput {
        source_name: "alpha".into(),
        priority: 300,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["git".into(), "ripgrep".into()],
                        ..Default::default()
                    }),
                    pipx: vec!["ruff".into(), "black".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let source_b = CompositionInput {
        source_name: "beta".into(),
        priority: 700,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                packages: Some(PackagesSpec {
                    brew: Some(BrewSpec {
                        formulae: vec!["ripgrep".into(), "jq".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source_a, source_b]).unwrap();
    let brew = result.resolved.merged.packages.brew.as_ref().unwrap();

    // All unique formulae present
    assert!(brew.formulae.contains(&"git".to_string()));
    assert!(brew.formulae.contains(&"curl".to_string()));
    assert!(brew.formulae.contains(&"ripgrep".to_string()));
    assert!(brew.formulae.contains(&"jq".to_string()));

    // No duplicates: count occurrences of "git" and "ripgrep"
    assert_eq!(
        brew.formulae.iter().filter(|f| *f == "git").count(),
        1,
        "git must appear exactly once"
    );
    assert_eq!(
        brew.formulae.iter().filter(|f| *f == "ripgrep").count(),
        1,
        "ripgrep must appear exactly once"
    );

    // pipx: black should not be duplicated
    let pipx = &result.resolved.merged.packages.pipx;
    assert!(pipx.contains(&"black".to_string()));
    assert!(pipx.contains(&"ruff".to_string()));
    assert_eq!(
        pipx.iter().filter(|p| *p == "black").count(),
        1,
        "black must appear exactly once"
    );
}

// --- Edge case: source_env populated correctly ---

#[test]
fn source_env_tracks_per_source_env_vars() {
    let local = make_local_profile();
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "CORP_VAR".into(),
                    value: "corp-value".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "LAYER_VAR".into(),
                    value: "from-layer".into(),
                }],
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    let corp_env = result.source_env.get("corp").expect("corp env must exist");
    // source_env is built from input.layers (not policy), so it should contain LAYER_VAR
    assert!(corp_env.iter().any(|e| e.name == "LAYER_VAR"));
}

// --- Edge case: compose with empty local profile ---

#[test]
fn compose_with_empty_local_and_no_sources() {
    let local = ResolvedProfile {
        layers: vec![],
        merged: MergedProfile::default(),
    };
    let result = compose(&local, &[]).unwrap();
    assert!(result.resolved.merged.env.is_empty());
    assert!(result.resolved.merged.modules.is_empty());
    assert!(result.resolved.merged.files.managed.is_empty());
    assert!(result.conflicts.is_empty());
}

// --- Edge case: aliases with same name across priorities ---

#[test]
fn higher_priority_source_wins_alias() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                aliases: vec![ShellAlias {
                    name: "ll".into(),
                    command: "ls -la".into(),
                }],
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            aliases: vec![ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
            }],
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                aliases: vec![ShellAlias {
                    name: "ll".into(),
                    command: "exa -la".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    let ll = result
        .resolved
        .merged
        .aliases
        .iter()
        .find(|a| a.name == "ll")
        .expect("ll alias must exist");
    // Local (priority 1000) processed after source (500), so local wins
    assert_eq!(ll.command, "ls -la");
}

// --- Additional coverage tests ---

#[test]
fn has_content_all_empty() {
    let items = PolicyItems::default();
    assert!(!has_content(&items));
}

#[test]
fn has_content_with_packages() {
    let items = PolicyItems {
        packages: Some(PackagesSpec::default()),
        ..Default::default()
    };
    assert!(has_content(&items));
}

#[test]
fn has_content_with_env() {
    let items = PolicyItems {
        env: vec![EnvVar {
            name: "A".into(),
            value: "1".into(),
        }],
        ..Default::default()
    };
    assert!(has_content(&items));
}

#[test]
fn has_content_with_aliases() {
    let items = PolicyItems {
        aliases: vec![ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }],
        ..Default::default()
    };
    assert!(has_content(&items));
}

#[test]
fn has_content_with_secrets() {
    let items = PolicyItems {
        secrets: vec![crate::config::SecretSpec {
            source: "vault://test".into(),
            target: None,
            template: None,
            backend: None,
            envs: None,
        }],
        ..Default::default()
    };
    assert!(has_content(&items));
}

#[test]
fn has_content_with_system() {
    let items = PolicyItems {
        system: std::collections::HashMap::from([(
            "shell".into(),
            serde_yaml::Value::String("/bin/zsh".into()),
        )]),
        ..Default::default()
    };
    assert!(has_content(&items));
}

#[test]
fn has_content_with_profiles() {
    let items = PolicyItems {
        profiles: vec!["base".into()],
        ..Default::default()
    };
    assert!(has_content(&items));
}

#[test]
fn policy_items_to_spec_converts_all_fields() {
    let items = PolicyItems {
        packages: Some(PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["git".into()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        files: vec![ManagedFileSpec {
            source: "f.yaml".into(),
            target: "~/.config/f.yaml".into(),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }],
        env: vec![EnvVar {
            name: "A".into(),
            value: "1".into(),
        }],
        aliases: vec![ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }],
        modules: vec!["nvim".into()],
        secrets: vec![crate::config::SecretSpec {
            source: "vault://test".into(),
            target: None,
            template: None,
            backend: None,
            envs: None,
        }],
        ..Default::default()
    };

    let spec = policy_items_to_spec(&items);
    assert!(spec.packages.is_some());
    assert!(spec.files.is_some());
    assert_eq!(spec.files.unwrap().managed.len(), 1);
    assert_eq!(spec.env.len(), 1);
    assert_eq!(spec.aliases.len(), 1);
    assert_eq!(spec.modules.len(), 1);
    assert_eq!(spec.secrets.len(), 1);
}

#[test]
fn policy_items_to_spec_empty_files_means_no_files_spec() {
    let items = PolicyItems {
        files: vec![],
        ..Default::default()
    };
    let spec = policy_items_to_spec(&items);
    assert!(spec.files.is_none());
}

#[test]
fn check_locked_violations_no_conflict() {
    let locked = PolicyItems::default();
    let merged = MergedProfile::default();
    assert!(check_locked_violations("src", &locked, &merged).is_ok());
}

#[test]
fn check_locked_violations_detects_override() {
    let locked = PolicyItems {
        files: vec![ManagedFileSpec {
            source: "corp/policy.yaml".into(),
            target: "~/.config/policy.yaml".into(),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }],
        ..Default::default()
    };
    let mut merged = MergedProfile::default();
    merged.files.managed.push(ManagedFileSpec {
        source: "local/override.yaml".into(),   // different source
        target: "~/.config/policy.yaml".into(), // same target
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    });
    let result = check_locked_violations("corp", &locked, &merged);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("locked"));
    assert!(err.contains("policy.yaml"));
}

#[test]
fn check_locked_violations_same_source_is_ok() {
    let locked = PolicyItems {
        files: vec![ManagedFileSpec {
            source: "corp/policy.yaml".into(),
            target: "~/.config/policy.yaml".into(),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }],
        ..Default::default()
    };
    let mut merged = MergedProfile::default();
    merged.files.managed.push(ManagedFileSpec {
        source: "corp/policy.yaml".into(), // same source
        target: "~/.config/policy.yaml".into(),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    });
    assert!(check_locked_violations("corp", &locked, &merged).is_ok());
}

#[test]
fn detect_permission_changes_new_source() {
    let old: Vec<CompositionInput> = vec![];
    let new = vec![CompositionInput {
        source_name: "new-src".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let changes = detect_permission_changes(&old, &new);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].source, "new-src");
    assert!(changes[0].description.contains("New source"));
}

#[test]
fn detect_permission_changes_locked_items_increased() {
    let old = vec![CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            locked: PolicyItems::default(), // 0 locked items
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let new = vec![CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            locked: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "new-lock.yaml".into(),
                    target: "~/.lock".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let changes = detect_permission_changes(&old, &new);
    assert!(
        changes
            .iter()
            .any(|c| c.description.contains("Locked items increased"))
    );
}

#[test]
fn detect_permission_changes_scripts_enabled() {
    let old = vec![CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: crate::config::SourceConstraints {
            no_scripts: true,
            ..Default::default()
        },
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let new = vec![CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: crate::config::SourceConstraints {
            no_scripts: false,
            ..Default::default()
        },
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let changes = detect_permission_changes(&old, &new);
    assert!(
        changes
            .iter()
            .any(|c| c.description.contains("Scripts have been enabled"))
    );
}

#[test]
fn detect_permission_changes_paths_expanded() {
    let old = vec![CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: crate::config::SourceConstraints {
            allowed_target_paths: vec!["~/.config/corp/".into()],
            ..Default::default()
        },
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let new = vec![CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: crate::config::SourceConstraints {
            allowed_target_paths: vec!["~/.config/corp/".into(), "~/.config/extra/".into()],
            ..Default::default()
        },
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    }];
    let changes = detect_permission_changes(&old, &new);
    assert!(
        changes
            .iter()
            .any(|c| c.description.contains("target paths expanded"))
    );
}

#[test]
fn detect_permission_changes_no_changes() {
    let mk = || CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    };
    // Same old and new - no changes
    let changes = detect_permission_changes(&[mk()], &[mk()]);
    assert!(changes.is_empty());
}

#[test]
fn count_policy_tier_items_comprehensive() {
    let items = PolicyItems {
        packages: Some(PackagesSpec {
            brew: Some(BrewSpec {
                formulae: vec!["a".into(), "b".into()],
                casks: vec!["c".into()],
                taps: vec!["t".into()],
                ..Default::default()
            }),
            apt: Some(crate::config::AptSpec {
                file: None,
                packages: vec!["d".into()],
            }),
            cargo: Some(crate::config::CargoSpec {
                file: None,
                packages: vec!["e".into()],
            }),
            pipx: vec!["f".into()],
            dnf: vec!["g".into()],
            npm: Some(crate::config::NpmSpec {
                file: None,
                global: vec!["h".into()],
            }),
            ..Default::default()
        }),
        files: vec![ManagedFileSpec {
            source: "s".into(),
            target: "t".into(),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }],
        env: vec![EnvVar {
            name: "A".into(),
            value: "1".into(),
        }],
        aliases: vec![ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }],
        system: HashMap::from([("shell".into(), serde_yaml::Value::Null)]),
        modules: vec!["mod1".into(), "mod2".into()],
        ..Default::default()
    };
    // brew: 2 formulae + 1 cask + 1 tap = 4
    // apt: 1, cargo: 1, pipx: 1, dnf: 1, npm: 1 = 5
    // files: 1, env: 1, aliases: 1, system: 1, modules: 2
    assert_eq!(count_policy_tier_items(&items), 4 + 5 + 1 + 1 + 1 + 1 + 2);
}

#[test]
fn filter_rejected_removes_env() {
    let recommended = PolicyItems {
        env: vec![
            EnvVar {
                name: "EDITOR".into(),
                value: "code".into(),
            },
            EnvVar {
                name: "PAGER".into(),
                value: "less".into(),
            },
        ],
        ..Default::default()
    };
    let reject: serde_yaml::Value = serde_yaml::from_str("env:\n  EDITOR: ~").unwrap();
    let filtered = filter_rejected(&recommended, &reject);
    assert_eq!(filtered.env.len(), 1);
    assert_eq!(filtered.env[0].name, "PAGER");
}

#[test]
fn filter_rejected_removes_aliases() {
    let recommended = PolicyItems {
        aliases: vec![
            ShellAlias {
                name: "vim".into(),
                command: "nvim".into(),
            },
            ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
            },
        ],
        ..Default::default()
    };
    let reject: serde_yaml::Value = serde_yaml::from_str("aliases:\n  vim: ~").unwrap();
    let filtered = filter_rejected(&recommended, &reject);
    assert_eq!(filtered.aliases.len(), 1);
    assert_eq!(filtered.aliases[0].name, "ll");
}

#[test]
fn filter_rejected_removes_modules() {
    let recommended = PolicyItems {
        modules: vec!["nvim".into(), "tmux".into(), "rust".into()],
        ..Default::default()
    };
    let reject: serde_yaml::Value = serde_yaml::from_str("modules:\n  - tmux\n  - rust").unwrap();
    let filtered = filter_rejected(&recommended, &reject);
    assert_eq!(filtered.modules, vec!["nvim".to_string()]);
}

#[test]
fn filter_rejected_removes_apt_packages() {
    let recommended = PolicyItems {
        packages: Some(PackagesSpec {
            apt: Some(crate::config::AptSpec {
                file: None,
                packages: vec!["curl".into(), "wget".into()],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let reject: serde_yaml::Value =
        serde_yaml::from_str("packages:\n  apt:\n    packages:\n      - curl").unwrap();
    let filtered = filter_rejected(&recommended, &reject);
    let apt = filtered.packages.unwrap().apt.unwrap();
    assert_eq!(apt.packages, vec!["wget".to_string()]);
}

#[test]
fn filter_rejected_removes_pipx_packages() {
    let recommended = PolicyItems {
        packages: Some(PackagesSpec {
            pipx: vec!["black".into(), "ruff".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let reject: serde_yaml::Value =
        serde_yaml::from_str("packages:\n  pipx:\n    - black").unwrap();
    let filtered = filter_rejected(&recommended, &reject);
    assert_eq!(filtered.packages.unwrap().pipx, vec!["ruff".to_string()]);
}

#[test]
fn merge_packages_snap_and_flatpak() {
    let mut target = PackagesSpec::default();
    let source = PackagesSpec {
        snap: Some(crate::config::SnapSpec {
            packages: vec!["firefox".into()],
            classic: vec!["code".into()],
        }),
        flatpak: Some(crate::config::FlatpakSpec {
            packages: vec!["org.gimp.GIMP".into()],
            remote: Some("flathub".into()),
        }),
        ..Default::default()
    };
    merge_packages(&mut target, &source);
    let snap = target.snap.unwrap();
    assert_eq!(snap.packages, vec!["firefox".to_string()]);
    assert_eq!(snap.classic, vec!["code".to_string()]);
    let flatpak = target.flatpak.unwrap();
    assert_eq!(flatpak.packages, vec!["org.gimp.GIMP".to_string()]);
    assert_eq!(flatpak.remote, Some("flathub".to_string()));
}

#[test]
fn merge_packages_nix_go_winget() {
    let mut target = PackagesSpec {
        nix: vec!["existing".into()],
        ..Default::default()
    };
    let source = PackagesSpec {
        nix: vec!["new-nix".into(), "existing".into()],
        go: vec!["gopls".into()],
        winget: vec!["Git.Git".into()],
        chocolatey: vec!["cmake".into()],
        scoop: vec!["gcc".into()],
        ..Default::default()
    };
    merge_packages(&mut target, &source);
    assert!(target.nix.contains(&"existing".to_string()));
    assert!(target.nix.contains(&"new-nix".to_string()));
    // No duplicates
    assert_eq!(target.nix.iter().filter(|n| *n == "existing").count(), 1);
    assert_eq!(target.go, vec!["gopls".to_string()]);
    assert_eq!(target.winget, vec!["Git.Git".to_string()]);
    assert_eq!(target.chocolatey, vec!["cmake".to_string()]);
    assert_eq!(target.scoop, vec!["gcc".to_string()]);
}

#[test]
fn merge_packages_custom_managers() {
    let mut target = PackagesSpec {
        custom: vec![crate::config::CustomManagerSpec {
            name: "mise".into(),
            check: "mise --version".into(),
            list_installed: "mise list".into(),
            install: "mise install {package}".into(),
            uninstall: "mise remove {package}".into(),
            update: None,
            packages: vec!["node".into()],
        }],
        ..Default::default()
    };
    let source = PackagesSpec {
        custom: vec![crate::config::CustomManagerSpec {
            name: "mise".into(),
            check: "mise --version".into(),
            list_installed: "mise list".into(),
            install: "mise install {package}".into(),
            uninstall: "mise remove {package}".into(),
            update: Some("mise upgrade {package}".into()),
            packages: vec!["python".into(), "node".into()],
        }],
        ..Default::default()
    };
    merge_packages(&mut target, &source);
    assert_eq!(target.custom.len(), 1);
    let mise = &target.custom[0];
    assert!(mise.packages.contains(&"node".to_string()));
    assert!(mise.packages.contains(&"python".to_string()));
    // No duplicate "node"
    assert_eq!(mise.packages.iter().filter(|p| *p == "node").count(), 1);
    // update was merged from source
    assert!(mise.update.is_some());
}

#[test]
fn compose_scripts_appended_in_order() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                scripts: Some(ScriptSpec {
                    pre_apply: vec![ScriptEntry::Simple("local-pre.sh".into())],
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            scripts: ScriptSpec {
                pre_apply: vec![ScriptEntry::Simple("local-pre.sh".into())],
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: crate::config::SourceConstraints {
            no_scripts: false,
            ..Default::default()
        },
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                scripts: Some(ScriptSpec {
                    pre_apply: vec![ScriptEntry::Simple("corp-pre.sh".into())],
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    let scripts = &result.resolved.merged.scripts.pre_apply;
    assert_eq!(scripts.len(), 2);
    // corp processed first (lower priority), then local
    assert_eq!(scripts[0].run_str(), "corp-pre.sh");
    assert_eq!(scripts[1].run_str(), "local-pre.sh");
}

#[test]
fn compose_secrets_deduplicated_by_source() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                secrets: vec![crate::config::SecretSpec {
                    source: "vault://secret/data/token".into(),
                    target: Some("/tmp/token".into()),
                    template: None,
                    backend: None,
                    envs: None,
                }],
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            secrets: vec![crate::config::SecretSpec {
                source: "vault://secret/data/token".into(),
                target: Some("/tmp/token".into()),
                template: None,
                backend: None,
                envs: None,
            }],
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: Default::default(),
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                secrets: vec![crate::config::SecretSpec {
                    source: "vault://secret/data/token".into(),
                    target: Some("/tmp/token-corp".into()),
                    template: None,
                    backend: None,
                    envs: None,
                }],
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    // Same source key — should be deduplicated (local wins, later layer)
    let secrets = &result.resolved.merged.secrets;
    let vault_secrets: Vec<_> = secrets
        .iter()
        .filter(|s| s.source.contains("vault://secret/data/token"))
        .collect();
    assert_eq!(
        vault_secrets.len(),
        1,
        "secrets with same source should deduplicate"
    );
    // Local (higher priority, processed last) should win
    assert_eq!(vault_secrets[0].target, Some("/tmp/token".into()));
}

#[test]
fn compose_system_deep_merges() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                system: HashMap::from([(
                    "shell".into(),
                    serde_yaml::Value::String("/bin/zsh".into()),
                )]),
                ..Default::default()
            },
        }],
        merged: MergedProfile {
            system: HashMap::from([("shell".into(), serde_yaml::Value::String("/bin/zsh".into()))]),
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: crate::config::SourceConstraints {
            allow_system_changes: true,
            ..Default::default()
        },
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                system: HashMap::from([(
                    "sysctl".into(),
                    serde_yaml::Value::String("value".into()),
                )]),
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    assert!(result.resolved.merged.system.contains_key("shell"));
    assert!(result.resolved.merged.system.contains_key("sysctl"));
}

#[test]
fn validate_constraints_encryption_mode_mismatch() {
    let constraints = crate::config::SourceConstraints {
        encryption: Some(crate::config::EncryptionConstraint {
            required_targets: vec!["~/.ssh/*".into()],
            backend: None,
            mode: Some(crate::config::EncryptionMode::Always),
        }),
        ..Default::default()
    };
    // File has InRepo mode but constraint requires Always
    let spec = ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "key".into(),
                target: "~/.ssh/id_rsa".into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "sops".into(),
                    mode: crate::config::EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = validate_constraints("corp", &constraints, &spec, false);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("mode"),
        "expected mode mismatch error, got: {msg}"
    );
}

#[test]
fn check_locked_violations_empty_locked_files() {
    // Locked items with no files but other content should not trigger violations
    let locked = PolicyItems {
        modules: vec!["corp-vpn".into()],
        ..Default::default()
    };
    let merged = MergedProfile {
        modules: vec!["corp-vpn".into()],
        ..Default::default()
    };
    // No file conflict — locked only has modules, not files
    assert!(check_locked_violations("corp", &locked, &merged).is_ok());
}

#[test]
fn compose_file_origins_tagged_for_source_files() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile::default(),
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: Default::default(),
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                files: Some(FilesSpec {
                    managed: vec![ManagedFileSpec {
                        source: "corp/tool.conf".into(),
                        target: "~/.config/tool.conf".into(),
                        strategy: None,
                        private: false,
                        origin: None,
                        encryption: None,
                        permissions: None,
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let result = compose(&local, &[source]).unwrap();
    let file = result
        .resolved
        .merged
        .files
        .managed
        .iter()
        .find(|f| f.target.to_string_lossy().contains("tool.conf"))
        .unwrap();
    // File origin should be tagged with source name
    assert_eq!(file.origin, Some("corp".to_string()));
}

#[test]
fn record_rejections_with_modules() {
    let recommended = PolicyItems::default();
    let reject: serde_yaml::Value =
        serde_yaml::from_str("modules:\n  - bad-mod\n  - other-mod").unwrap();
    let mut conflicts = Vec::new();
    record_rejections("corp", &recommended, &reject, &mut conflicts);
    assert_eq!(conflicts.len(), 2);
    assert!(
        conflicts
            .iter()
            .all(|c| c.resolution_type == ResolutionType::Rejected)
    );
    assert!(conflicts.iter().any(|c| c.resource_id == "module:bad-mod"));
    assert!(
        conflicts
            .iter()
            .any(|c| c.resource_id == "module:other-mod")
    );
}

#[test]
fn record_rejections_null_does_nothing() {
    let recommended = PolicyItems::default();
    let mut conflicts = Vec::new();
    record_rejections(
        "corp",
        &recommended,
        &serde_yaml::Value::Null,
        &mut conflicts,
    );
    assert!(conflicts.is_empty());
}

#[test]
fn record_policy_conflicts_secrets_and_aliases() {
    let items = PolicyItems {
        aliases: vec![ShellAlias {
            name: "g".into(),
            command: "git".into(),
        }],
        secrets: vec![crate::config::SecretSpec {
            source: "vault://test".into(),
            target: None,
            template: None,
            backend: None,
            envs: None,
        }],
        ..Default::default()
    };
    let mut conflicts = Vec::new();
    record_policy_conflicts("corp", &items, ResolutionType::Required, &mut conflicts);
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts.iter().any(|c| c.resource_id == "alias:g"));
    assert!(
        conflicts
            .iter()
            .any(|c| c.resource_id == "secret:vault://test")
    );
}

#[test]
fn collect_package_names_all_managers() {
    let pkgs = PackagesSpec {
        brew: Some(BrewSpec {
            formulae: vec!["git".into()],
            casks: vec!["firefox".into()],
            ..Default::default()
        }),
        apt: Some(crate::config::AptSpec {
            file: None,
            packages: vec!["curl".into()],
        }),
        cargo: Some(crate::config::CargoSpec {
            file: None,
            packages: vec!["bat".into()],
        }),
        npm: Some(crate::config::NpmSpec {
            file: None,
            global: vec!["prettier".into()],
        }),
        pipx: vec!["black".into()],
        dnf: vec!["vim".into()],
        ..Default::default()
    };
    let names = collect_package_names(&pkgs);
    assert_eq!(names.len(), 7);
    assert!(
        names
            .iter()
            .any(|n| n.contains("git") && n.contains("brew"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("firefox") && n.contains("brew cask"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("curl") && n.contains("apt"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("bat") && n.contains("cargo"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("prettier") && n.contains("npm"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("black") && n.contains("pipx"))
    );
    assert!(names.iter().any(|n| n.contains("vim") && n.contains("dnf")));
}

#[test]
fn build_source_layers_optional_opt_in() {
    let input = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            optional: PolicyItems {
                profiles: vec!["extra".into()],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "extra".into(),
            priority: 500,
            policy: LayerPolicy::Optional,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "EXTRA".into(),
                    value: "yes".into(),
                }],
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: false,
            opt_in: vec!["extra".into()],
            ..Default::default()
        },
        allow_scripts: false,
    };
    let mut conflicts = Vec::new();
    let layers = build_source_layers(&input, &mut conflicts).unwrap();
    // Should include the "extra" layer via opt-in
    assert!(
        layers.iter().any(|l| l.profile_name == "extra"),
        "opt-in profile should be included"
    );
}

#[test]
fn resolution_type_labels() {
    assert_eq!(ResolutionType::Locked.label(), "LOCKED");
    assert_eq!(ResolutionType::Required.label(), "REQUIRED");
    assert_eq!(ResolutionType::Override.label(), "OVERRIDE");
    assert_eq!(ResolutionType::Rejected.label(), "REJECTED");
    assert_eq!(ResolutionType::Default.label(), "DEFAULT");
}

#[test]
fn find_matching_pattern_prefix() {
    let result = find_matching_pattern(
        "~/.config/corp/deep/nested/file.yaml",
        &["~/.config/corp/".into()],
    );
    assert!(result.is_some());
    assert_eq!(result.unwrap(), "~/.config/corp/");
}

#[test]
fn find_matching_pattern_no_match() {
    let result =
        find_matching_pattern("/etc/sudoers", &["~/.config/*".into(), "~/.local/*".into()]);
    assert!(result.is_none());
}

#[test]
fn encryption_module_file_matching_required_target_without_encryption_is_error() {
    // Module files come through as ProfileSpec files just like profile files;
    // validate_constraints sees them the same way.
    let constraints = make_encryption_constraint(&["~/.config/secrets/*"], None);
    let spec = ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![
                // First file does NOT match the pattern — OK
                ManagedFileSpec {
                    source: "files/.zshrc".into(),
                    target: "~/.zshrc".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
                // Second file MATCHES the pattern and has no encryption — error
                ManagedFileSpec {
                    source: "files/api-key".into(),
                    target: "~/.config/secrets/api-key".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                },
            ],
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = validate_constraints("corp", &constraints, &spec, false);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("~/.config/secrets/api-key"), "msg: {msg}");
}

// --- Direct `matches!`-pinned variant tests ---
// These tests construct the production code paths that produce each
// CompositionError variant and assert via `matches!` so a future rename or
// signature change to the variant trips the test rather than silently
// reshaping the error surface. One variant per test.

/// Helper: unwrap `CfgdError::Composition(Box<CompositionError>)` to its inner
/// variant for `matches!` assertions. Panics with the actual error if the
/// outer wrap shape changed.
fn unwrap_composition_err(err: crate::errors::CfgdError) -> crate::errors::CompositionError {
    match err {
        crate::errors::CfgdError::Composition(boxed) => *boxed,
        other => panic!("expected CfgdError::Composition, got: {other:?}"),
    }
}

#[test]
fn composition_error_variant_locked_resource() {
    let locked = PolicyItems {
        files: vec![ManagedFileSpec {
            source: "corp/policy.yaml".into(),
            target: "~/.config/policy.yaml".into(),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }],
        ..Default::default()
    };
    let mut merged = MergedProfile::default();
    merged.files.managed.push(ManagedFileSpec {
        source: "local/override.yaml".into(),
        target: "~/.config/policy.yaml".into(),
        strategy: None,
        private: false,
        origin: None,
        encryption: None,
        permissions: None,
    });
    let err = check_locked_violations("corp", &locked, &merged).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::LockedResource { .. }
    ));
}

#[test]
fn composition_error_variant_required_resource() {
    let local = make_local_profile();
    let source_required = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            required: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "corp/policy.yaml".into(),
                    target: "~/.config/policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let source_overrider = CompositionInput {
        source_name: "rogue".into(),
        priority: 600,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "rogue/policy.yaml".into(),
                    target: "~/.config/policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let err = compose(&local, &[source_required, source_overrider]).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::RequiredResource { .. }
    ));
}

#[test]
fn composition_error_variant_path_not_allowed() {
    let constraints = SourceConstraints {
        allowed_target_paths: vec!["~/.config/acme/".into()],
        ..Default::default()
    };
    let spec = ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "evil.sh".into(),
                target: "/etc/sudoers".into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = validate_constraints("acme", &constraints, &spec, false).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::PathNotAllowed { .. }
    ));
}

#[test]
fn composition_error_variant_scripts_not_allowed() {
    let constraints = SourceConstraints {
        no_scripts: true,
        ..Default::default()
    };
    let spec = ProfileSpec {
        scripts: Some(ScriptSpec {
            pre_apply: vec![ScriptEntry::Simple("setup.sh".into())],
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = validate_constraints("acme", &constraints, &spec, false).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::ScriptsNotAllowed { .. }
    ));
}

#[test]
fn composition_error_variant_system_change_not_allowed() {
    let constraints = SourceConstraints {
        allow_system_changes: false,
        ..Default::default()
    };
    let spec = ProfileSpec {
        system: HashMap::from([("shell".into(), serde_yaml::Value::String("/bin/zsh".into()))]),
        ..Default::default()
    };
    let err = validate_constraints("acme", &constraints, &spec, false).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::SystemChangeNotAllowed { .. }
    ));
}

#[test]
fn composition_error_variant_unresolvable_conflict() {
    let local = make_local_profile();
    let source_a = CompositionInput {
        source_name: "team-a".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "team-a/settings.json".into(),
                    target: "~/.config/settings.json".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let source_b = CompositionInput {
        source_name: "team-b".into(),
        priority: 500, // same priority → unresolvable
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "team-b/settings.json".into(),
                    target: "~/.config/settings.json".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: Default::default(),
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };

    let err = compose(&local, &[source_a, source_b]).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::UnresolvableConflict { .. }
    ));
}

#[test]
fn composition_error_variant_encryption_required() {
    let constraints = make_encryption_constraint(&["~/.ssh/*"], None);
    let spec = make_file_spec_with_encryption("~/.ssh/id_rsa", None);
    let err = validate_constraints("corp", &constraints, &spec, false).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::EncryptionRequired { .. }
    ));
}

#[test]
fn composition_error_variant_encryption_backend_mismatch() {
    let constraints = make_encryption_constraint(&["~/.aws/*"], Some("sops"));
    let spec = make_file_spec_with_encryption("~/.aws/credentials", Some("age"));
    let err = validate_constraints("corp", &constraints, &spec, false).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::EncryptionBackendMismatch { .. }
    ));
}

#[test]
fn composition_error_variant_encryption_mode_mismatch() {
    let constraints = SourceConstraints {
        encryption: Some(crate::config::EncryptionConstraint {
            required_targets: vec!["~/.ssh/*".into()],
            backend: None,
            mode: Some(crate::config::EncryptionMode::Always),
        }),
        ..Default::default()
    };
    let spec = ProfileSpec {
        files: Some(FilesSpec {
            managed: vec![ManagedFileSpec {
                source: "key".into(),
                target: "~/.ssh/id_rsa".into(),
                strategy: None,
                private: false,
                origin: None,
                encryption: Some(crate::config::EncryptionSpec {
                    backend: "sops".into(),
                    mode: crate::config::EncryptionMode::InRepo,
                }),
                permissions: None,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = validate_constraints("corp", &constraints, &spec, false).unwrap_err();
    let inner = unwrap_composition_err(err);
    assert!(matches!(
        inner,
        crate::errors::CompositionError::EncryptionModeMismatch { .. }
    ));
}

#[test]
fn composition_error_variant_template_sandbox_violation() {
    // The variant is produced by the binary crate's template renderer when a
    // source template references a variable outside its sandbox. The variant
    // itself is constructed in production code; here we directly instantiate
    // and `matches!`-pin so a rename or signature change trips the test. The
    // production code path is exercised in
    // `crates/cfgd/src/files/tests.rs::source_template_sandbox_violation_pins_variant`.
    let err = crate::errors::CompositionError::TemplateSandboxViolation {
        source_name: "acme-corp".into(),
        variable: "personal_var".into(),
    };
    assert!(matches!(
        err,
        crate::errors::CompositionError::TemplateSandboxViolation { .. }
    ));
}

#[test]
fn record_policy_conflicts_env_vars() {
    let items = PolicyItems {
        env: vec![
            EnvVar {
                name: "EDITOR".into(),
                value: "vim".into(),
            },
            EnvVar {
                name: "PAGER".into(),
                value: "less".into(),
            },
        ],
        ..Default::default()
    };
    let mut conflicts = Vec::new();
    record_policy_conflicts("corp", &items, ResolutionType::Locked, &mut conflicts);
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts.iter().any(|c| c.resource_id == "env:EDITOR"));
    assert!(conflicts.iter().any(|c| c.resource_id == "env:PAGER"));
    assert!(conflicts.iter().all(|c| c.winning_source == "corp"));
    assert!(
        conflicts
            .iter()
            .all(|c| c.resolution_type == ResolutionType::Locked)
    );
}

#[test]
fn record_policy_conflicts_modules() {
    let items = PolicyItems {
        modules: vec!["kubernetes".into(), "docker".into()],
        ..Default::default()
    };
    let mut conflicts = Vec::new();
    record_policy_conflicts("corp", &items, ResolutionType::Required, &mut conflicts);
    assert_eq!(conflicts.len(), 2);
    assert!(
        conflicts
            .iter()
            .any(|c| c.resource_id == "module:kubernetes")
    );
    assert!(conflicts.iter().any(|c| c.resource_id == "module:docker"));
}

#[test]
fn record_policy_conflicts_files() {
    let file: crate::config::ManagedFileSpec =
        serde_yaml::from_str("source: hosts.tmpl\ntarget: /etc/hosts").unwrap();
    let items = PolicyItems {
        files: vec![file],
        ..Default::default()
    };
    let mut conflicts = Vec::new();
    record_policy_conflicts("corp", &items, ResolutionType::Override, &mut conflicts);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].resource_id, "/etc/hosts");
    assert_eq!(conflicts[0].resolution_type, ResolutionType::Override);
}

#[test]
fn record_rejections_packages_flat_list() {
    let recommended = PolicyItems::default();
    let reject: serde_yaml::Value =
        serde_yaml::from_str("packages:\n  brew:\n    - unwanted-pkg\n    - bad-tool").unwrap();
    let mut conflicts = Vec::new();
    record_rejections("corp", &recommended, &reject, &mut conflicts);
    assert_eq!(conflicts.len(), 2);
    assert!(
        conflicts
            .iter()
            .all(|c| c.resolution_type == ResolutionType::Rejected)
    );
    assert!(conflicts.iter().any(|c| c.resource_id == "unwanted-pkg"));
    assert!(conflicts.iter().any(|c| c.resource_id == "bad-tool"));
}

#[test]
fn record_rejections_packages_nested_mapping() {
    let recommended = PolicyItems::default();
    let reject: serde_yaml::Value = serde_yaml::from_str(
        "packages:\n  brew:\n    formulae:\n      - ripgrep\n    casks:\n      - slack",
    )
    .unwrap();
    let mut conflicts = Vec::new();
    record_rejections("corp", &recommended, &reject, &mut conflicts);
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts.iter().any(|c| c.resource_id == "ripgrep"));
    assert!(conflicts.iter().any(|c| c.resource_id == "slack"));
    assert!(conflicts.iter().all(|c| c.winning_source == "local"));
}

#[test]
fn resolution_type_label_all_variants() {
    assert_eq!(ResolutionType::Locked.label(), "LOCKED");
    assert_eq!(ResolutionType::Required.label(), "REQUIRED");
    assert_eq!(ResolutionType::Override.label(), "OVERRIDE");
    assert_eq!(ResolutionType::Rejected.label(), "REJECTED");
    assert_eq!(ResolutionType::Default.label(), "DEFAULT");
}

// --- subscriber overrides composition (Finding A) ---

/// Build a source input whose recommended tier sets `EDITOR=vim` and an npm
/// recommendation of `eslint`, with the given `overrides` YAML applied.
fn override_test_input(priority: u32, overrides_yaml: &str) -> CompositionInput {
    let overrides: serde_yaml::Value = serde_yaml::from_str(overrides_yaml).unwrap();
    CompositionInput {
        source_name: "team".into(),
        priority,
        policy: ConfigSourcePolicy {
            recommended: PolicyItems {
                env: vec![EnvVar {
                    name: "EDITOR".into(),
                    value: "vim".into(),
                }],
                packages: Some(PackagesSpec {
                    npm: Some(NpmSpec {
                        global: vec!["eslint".into()],
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
        subscription: SubscriptionConfig {
            accept_recommended: true,
            overrides,
            ..Default::default()
        },
        allow_scripts: false,
    }
}

#[test]
fn override_env_beats_sources_own_recommendation() {
    let local = ResolvedProfile {
        layers: vec![],
        merged: MergedProfile::default(),
    };
    let input = override_test_input(500, "env:\n  EDITOR: nvim");
    let result = compose(&local, &[input]).unwrap();

    assert_eq!(
        result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "EDITOR")
            .map(|e| e.value.as_str()),
        Some("nvim"),
        "subscriber override should beat the source's own recommended EDITOR=vim"
    );
}

#[test]
fn override_env_stays_below_local_config() {
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "EDITOR".into(),
                    value: "code".into(),
                }],
                ..Default::default()
            },
        }],
        merged: MergedProfile::default(),
    };
    let input = override_test_input(500, "env:\n  EDITOR: nvim");
    let result = compose(&local, &[input]).unwrap();

    assert_eq!(
        result
            .resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "EDITOR")
            .map(|e| e.value.as_str()),
        Some("code"),
        "local config (priority 1000) must beat the override layer at priority+1"
    );
}

#[test]
fn override_packages_union_does_not_replace_recommended() {
    // packages merge via union: the override ADDS prettier, eslint survives.
    let local = ResolvedProfile {
        layers: vec![],
        merged: MergedProfile::default(),
    };
    let input = override_test_input(500, "packages:\n  npm:\n    global: [prettier]");
    let result = compose(&local, &[input]).unwrap();

    let npm = result
        .resolved
        .merged
        .packages
        .npm
        .as_ref()
        .expect("npm packages present");
    assert!(
        npm.global.contains(&"eslint".to_string()),
        "source-recommended eslint must survive the override (union, not replace)"
    );
    assert!(
        npm.global.contains(&"prettier".to_string()),
        "override prettier must be added"
    );
}

#[test]
fn override_unknown_field_errors() {
    let local = ResolvedProfile {
        layers: vec![],
        merged: MergedProfile::default(),
    };
    let input = override_test_input(500, "bogusField: x");
    let result = compose(&local, &[input]);
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("invalid overrides"),
        "deny_unknown_fields should reject a typo'd override key, got: {err}"
    );
}

#[test]
fn override_empty_or_null_builds_no_layer() {
    // An empty mapping or a null `overrides` must not produce an `…/overrides`
    // layer (the guard at build_source_layers), leaving the source's own
    // recommendation intact.
    let local = ResolvedProfile {
        layers: vec![],
        merged: MergedProfile::default(),
    };
    for overrides_yaml in ["{}", "null"] {
        let input = override_test_input(500, overrides_yaml);
        let result = compose(&local, &[input]).unwrap();
        assert!(
            !result
                .resolved
                .layers
                .iter()
                .any(|l| l.profile_name == "team/overrides"),
            "overrides '{overrides_yaml}' must build no override layer"
        );
        assert_eq!(
            result
                .resolved
                .merged
                .env
                .iter()
                .find(|e| e.name == "EDITOR")
                .map(|e| e.value.as_str()),
            Some("vim"),
            "with no override, the source's own EDITOR=vim must remain"
        );
    }
}

// ---------------------------------------------------------------------------
// compose() is the FATAL source-enforcement chokepoint
// ---------------------------------------------------------------------------

#[test]
fn compose_rejects_source_scripts_when_no_scripts() {
    // A source with no_scripts=true (the default) whose profile layer carries a
    // lifecycle script must abort composition, not warn-and-execute.
    let local = make_local_profile();
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy::default(),
        constraints: SourceConstraints {
            no_scripts: true,
            ..Default::default()
        },
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                scripts: Some(ScriptSpec {
                    pre_apply: vec![ScriptEntry::Simple("evil.sh".into())],
                    ..Default::default()
                }),
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let err = compose(&local, &[source]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("corp") && msg.contains("scripts"),
        "compose must abort on disallowed source scripts: {msg}"
    );
}

#[test]
fn compose_rejects_policy_tier_file_outside_allowed_paths() {
    // A locked-tier file delivered to a target OUTSIDE allowedTargetPaths must
    // abort composition. The policy tiers were never path-checked before.
    let local = make_local_profile();
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            locked: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "evil.sh".into(),
                    target: "/etc/sudoers".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints {
            allowed_target_paths: vec!["~/.config/corp/".into()],
            ..Default::default()
        },
        layers: vec![],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let err = compose(&local, &[source]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("/etc/sudoers") && msg.contains("corp"),
        "compose must path-check policy-tier files: {msg}"
    );
}

#[test]
fn compose_rejects_required_tier_file_outside_allowed_paths() {
    // Same path-escape via the `required` tier (no subscription.profile in play)
    // — enforcement happens regardless of subscription.profile.
    let local = make_local_profile();
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            required: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "rc".into(),
                    target: "/etc/profile.d/corp.sh".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints {
            allowed_target_paths: vec!["~/.config/corp/".into()],
            ..Default::default()
        },
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    };
    let err = compose(&local, &[source]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("/etc/profile.d/corp.sh") && msg.contains("corp"),
        "compose must path-check required-tier files with no subscription profile: {msg}"
    );
}

#[test]
fn compose_rejects_local_override_of_locked_resource() {
    // Local config managing a file the source has locked must abort.
    let local = ResolvedProfile {
        layers: vec![ProfileLayer {
            source: "local".into(),
            profile_name: "default".into(),
            priority: 1000,
            policy: LayerPolicy::Local,
            spec: ProfileSpec::default(),
        }],
        merged: MergedProfile {
            files: FilesSpec {
                managed: vec![ManagedFileSpec {
                    source: "local/policy.yaml".into(),
                    target: "~/.config/company/security-policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            locked: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "corp/policy.yaml".into(),
                    target: "~/.config/company/security-policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    };
    let err = compose(&local, &[source]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security-policy.yaml") && msg.contains("corp"),
        "compose must abort when local overrides a locked resource: {msg}"
    );
}

#[test]
fn compose_rejects_unknown_reject_key() {
    let local = make_local_profile();
    let mut source = make_source_input("acme", 500);
    source.subscription.reject = serde_yaml::from_str(
        r#"
        packagess:
          brew:
            formulae:
              - stern
        "#,
    )
    .unwrap();
    let err = compose(&local, &[source]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("packagess") && msg.contains("acme"),
        "compose must reject a typo'd reject key: {msg}"
    );
}

#[test]
fn compose_accepts_valid_reject_mapping() {
    let local = make_local_profile();
    let mut source = make_source_input("acme", 500);
    source.subscription.reject = serde_yaml::from_str(
        r#"
        packages:
          brew:
            formulae:
              - stern
        "#,
    )
    .unwrap();
    let result = compose(&local, &[source]).unwrap();
    let brew = result.resolved.merged.packages.brew.as_ref().unwrap();
    assert!(brew.formulae.contains(&"k9s".into()));
    assert!(!brew.formulae.contains(&"stern".into()));
}

#[test]
fn compose_accepts_compliant_source() {
    // A compliant source: no scripts, files within allowedTargetPaths. Guards
    // against over-rejection by the chokepoint.
    let local = make_local_profile();
    let source = CompositionInput {
        source_name: "corp".into(),
        priority: 500,
        policy: ConfigSourcePolicy {
            required: PolicyItems {
                files: vec![ManagedFileSpec {
                    source: "corp/policy.yaml".into(),
                    target: "~/.config/corp/policy.yaml".into(),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints {
            no_scripts: true,
            allowed_target_paths: vec!["~/.config/corp/".into()],
            ..Default::default()
        },
        layers: vec![ProfileLayer {
            source: "corp".into(),
            profile_name: "corp/base".into(),
            priority: 500,
            policy: LayerPolicy::Recommended,
            spec: ProfileSpec {
                env: vec![EnvVar {
                    name: "CORP".into(),
                    value: "1".into(),
                }],
                ..Default::default()
            },
        }],
        subscription: SubscriptionConfig {
            accept_recommended: true,
            ..Default::default()
        },
        allow_scripts: false,
    };
    let result = compose(&local, &[source]).unwrap();
    assert!(
        result
            .resolved
            .merged
            .files
            .managed
            .iter()
            .any(|f| f.target.to_string_lossy().contains("corp/policy.yaml")),
        "compliant source must compose successfully"
    );
}

#[test]
fn compose_max_priority_source_no_overflow_and_correct_rank() {
    // A source at MAX_SOURCE_PRIORITY must not panic (overflow) in debug builds
    // and its required layer must rank strictly below the locked sentinel (u32::MAX)
    // and strictly above local config (1000) — priority inversion would defeat
    // the "required beats normal/local" guarantee.
    let source = CompositionInput {
        source_name: "max-prio".into(),
        priority: crate::config::MAX_SOURCE_PRIORITY,
        policy: ConfigSourcePolicy {
            required: PolicyItems {
                env: vec![EnvVar {
                    name: "REQUIRED_VAR".into(),
                    value: "enforced".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        constraints: SourceConstraints::default(),
        layers: vec![],
        subscription: SubscriptionConfig::default(),
        allow_scripts: false,
    };

    // Must not panic; without saturating_add this panics in debug builds.
    let layers = build_source_layers(&source, &mut vec![])
        .expect("build_source_layers must not error at MAX_SOURCE_PRIORITY");

    let required_layer = layers
        .iter()
        .find(|l| l.profile_name.ends_with("/required"))
        .expect("required layer must be present");

    // Strictly below locked sentinel.
    assert!(
        required_layer.priority < u32::MAX,
        "required layer rank must be below locked sentinel (u32::MAX), got {}",
        required_layer.priority
    );
    // Strictly above local config rank.
    assert!(
        required_layer.priority > 1000,
        "required layer rank must beat local config rank (1000), got {}",
        required_layer.priority
    );
}
