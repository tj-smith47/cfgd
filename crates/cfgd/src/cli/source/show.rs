use super::*;
use crate::cli::output_types::SourcePolicyOutput;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{ConfigSourceDocument, PolicyItems, SourceConstraints, SourceSpec};
use cfgd_core::output::{Doc, Printer, Role, doc::SectionBuilder, renderer::Table};

const SHORT_COMMIT_LEN: usize = 12;

/// Build the not-found error returned by `cmd_source_show`. The central error
/// sink (`main.rs::render_cli_error`) renders the structured `{error, name,
/// available}` payload for `-o json` consumers and the user-visible `✗` line
/// exactly once; the available-sources hint is carried as a human-mode hint.
pub fn build_source_not_found_error(name: &str, available: &[String]) -> anyhow::Error {
    let mut hints = Vec::new();
    if !available.is_empty() {
        hints.push(format!("Available sources: {}", available.join(", ")));
    }
    // Carry the typed `SourceError::NotFound` in the chain so the exit-code
    // downcast in `main.rs` resolves to ExitCode::NotFound (6); the attached
    // CliErrorMeta still drives the rich `not_found` payload + hints.
    crate::cli::cli_error_ctx_with_hints(
        cfgd_core::errors::CfgdError::Source(cfgd_core::errors::SourceError::NotFound {
            name: name.to_string(),
        })
        .into(),
        name,
        "not_found",
        format!("Source '{}' not found", name),
        serde_json::json!({ "available": available }),
        hints,
    )
}

pub fn build_source_show_doc(
    output: &SourceShowOutput,
    manifest: Option<&ConfigSourceDocument>,
) -> Doc {
    // A `Label: value` title, never a `kind:name` owner token: an owner names
    // whose the rows below it are, which is a section's job, and hanging the
    // whole report off one nests every block a level deeper than `module show`
    // renders the same shape at — where the renderer's blank separation between
    // top-level sections is what makes ~60 rows of eight blocks readable.
    let mut doc = Doc::new()
        .heading_title("Source", &output.name)
        .kv("URL", &output.url)
        .kv("Branch", &output.branch)
        .kv("Priority", output.priority.to_string())
        .kv("Accept Recommended", output.accept_recommended.to_string());

    if let Some(ref profile) = output.profile {
        doc = doc.kv("Profile", profile);
    }
    doc = doc
        .kv("Sync Interval", &output.sync_interval)
        .kv("Auto Apply", output.auto_apply.to_string());
    if let Some(ref pin) = output.pin_version {
        doc = doc.kv("Pin Version", pin);
    }

    if let Some(ref state_info) = output.state {
        doc = doc.section("State", |s| {
            let mut s = s.kv("Status", &state_info.status);
            if let Some(ref fetched) = state_info.last_fetched {
                s = s.kv("Last Fetched", fetched);
            }
            if let Some(ref commit) = state_info.last_commit {
                let short = &commit[..commit.len().min(SHORT_COMMIT_LEN)];
                s = s.kv("Last Commit", short);
            }
            if let Some(ref version) = state_info.version {
                s = s.kv("Version", version);
            }
            if let Some(ref locked_ref) = state_info.locked_ref {
                s = s.kv("Locked Ref", locked_ref);
            }
            if let Some(ref locked_commit) = state_info.locked_commit {
                let short = &locked_commit[..locked_commit.len().min(SHORT_COMMIT_LEN)];
                s = s.kv("Locked Commit", short);
            }
            s
        });
    }

    doc = doc.section_if_nonempty(
        "Managed Resources",
        &output.managed_resources,
        |s, resources| {
            let mut table = Table::new(["Type", "Resource"]);
            for r in resources {
                table = table.row([r.resource_type.clone(), r.resource_id.clone()]);
            }
            s.table(table)
        },
    );

    // Modules this source DELIVERS — its manifest `provides.modules` allow-list
    // (the bodies a subscriber can resolve from this source).
    doc = doc.section_if_nonempty("Modules", &output.modules, |s, modules| {
        let mut s = s;
        for m in modules {
            s = s.status(Role::Info, m.clone());
        }
        s
    });

    // What this source enforces, combining the manifest's constraints with
    // this subscriber's own overrides — an operator auditing a source reads
    // this instead of opening its manifest YAML.
    if let Some(ref policy) = output.policy {
        doc = doc.section("Policy", |s| {
            // `allowUnsigned` bypasses the demand entirely — the screen must
            // say so beside the flag, or `true` reads as enforced when the
            // check never runs.
            let require_signed_commits_value = if policy.signed_commits_bypassed {
                format!(
                    "{} (bypassed: security.allowUnsigned)",
                    policy.require_signed_commits
                )
            } else {
                policy.require_signed_commits.to_string()
            };
            let mut s = s
                .kv("Require Signed Commits", require_signed_commits_value)
                .kv("Scripts Allowed", policy.scripts_allowed.to_string())
                .kv(
                    "Secrets Read Allowed",
                    policy.secrets_read_allowed.to_string(),
                )
                .kv(
                    "System Changes Allowed",
                    policy.system_changes_allowed.to_string(),
                );
            if !policy.allowed_target_paths.is_empty() {
                s = s.kv(
                    "Allowed Target Paths",
                    policy.allowed_target_paths.join(", "),
                );
            }
            if let Some(ref enc) = policy.encryption {
                s = s.subsection("Encryption", |sub| {
                    let mut sub = sub;
                    if !enc.required_targets.is_empty() {
                        sub = sub.kv("Required Targets", enc.required_targets.join(", "));
                    }
                    if let Some(ref backend) = enc.backend {
                        sub = sub.kv("Backend", backend);
                    }
                    if let Some(ref mode) = enc.mode {
                        sub = sub.kv("Mode", mode);
                    }
                    sub
                });
            }
            s
        });
    }

    if let Some(m) = manifest {
        doc = doc.section("Manifest", |s| {
            let mut s = s.kv("Name", &m.metadata.name);
            if let Some(ref desc) = m.metadata.description {
                s = s.kv("Description", desc);
            }

            let policy = &m.spec.policy;
            let locked = count_policy_items(&policy.locked);
            let required = count_policy_items(&policy.required);
            let recommended = count_policy_items(&policy.recommended);

            if locked + required + recommended > 0 {
                s = s.subsection("Policy Summary", |sub| {
                    let sub = if locked > 0 {
                        sub.subsection("Locked", |inner| {
                            append_policy_items(
                                inner.kv("Count", locked.to_string()),
                                &policy.locked,
                            )
                        })
                    } else {
                        sub
                    };
                    let sub = if required > 0 {
                        sub.subsection("Required", |inner| {
                            append_policy_items(
                                inner.kv("Count", required.to_string()),
                                &policy.required,
                            )
                        })
                    } else {
                        sub
                    };
                    if recommended > 0 {
                        sub.subsection("Recommended", |inner| {
                            append_policy_items(
                                inner.kv("Count", recommended.to_string()),
                                &policy.recommended,
                            )
                        })
                    } else {
                        sub
                    }
                });
            }
            s
        });
    }

    doc.with_data(output)
}

/// What `source` enforces, combining the subscriber's own overrides with the
/// manifest's `policy.constraints` — the derivation `cmd_source_show` renders
/// and serializes so a `source show` reader never has to open the manifest
/// YAML to answer "what is enforced here". `allow_unsigned` is the
/// subscriber's own `spec.security.allowUnsigned`, which bypasses signature
/// verification entirely regardless of what `require_signed_commits`
/// demands — the returned `require_signed_commits` states the DEMAND, and
/// `signed_commits_bypassed` states whether this subscriber actually
/// enforces it, so the screen never renders an unqualified `true` for a
/// check that never runs.
fn effective_source_policy(
    source_spec: &SourceSpec,
    constraints: &SourceConstraints,
    allow_unsigned: bool,
) -> SourcePolicyOutput {
    let require_signed_commits =
        source_spec.requires_signed_commits(constraints.require_signed_commits);
    SourcePolicyOutput {
        require_signed_commits,
        signed_commits_bypassed: require_signed_commits && allow_unsigned,
        scripts_allowed: source_spec.subscription.allow_scripts || !constraints.no_scripts,
        secrets_read_allowed: !constraints.no_secrets_read,
        system_changes_allowed: constraints.allow_system_changes,
        allowed_target_paths: constraints.allowed_target_paths.clone(),
        encryption: constraints.encryption.as_ref().map(|enc| {
            crate::cli::output_types::SourceEncryptionOutput {
                required_targets: enc.required_targets.clone(),
                backend: enc.backend.clone(),
                mode: enc.mode.as_ref().map(|m| m.as_str().to_string()),
            }
        }),
    }
}

fn append_policy_items(mut s: SectionBuilder, items: &PolicyItems) -> SectionBuilder {
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            for f in &brew.formulae {
                s = s.status_with(Role::Info, "brew formula", |sf| sf.qualifier(f.clone()));
            }
            for c in &brew.casks {
                s = s.status_with(Role::Info, "brew cask", |sf| sf.qualifier(c.clone()));
            }
        }
        if let Some(ref apt) = pkgs.apt {
            for p in &apt.packages {
                s = s.status_with(Role::Info, "apt", |sf| sf.qualifier(p.clone()));
            }
        }
        if let Some(ref cargo) = pkgs.cargo {
            for p in &cargo.packages {
                s = s.status_with(Role::Info, "cargo", |sf| sf.qualifier(p.clone()));
            }
        }
        for p in &pkgs.pipx {
            s = s.status_with(Role::Info, "pipx", |sf| sf.qualifier(p.clone()));
        }
        for p in &pkgs.dnf {
            s = s.status_with(Role::Info, "dnf", |sf| sf.qualifier(p.clone()));
        }
        if let Some(ref npm) = pkgs.npm {
            for p in &npm.global {
                s = s.status_with(Role::Info, "npm", |sf| sf.qualifier(p.clone()));
            }
        }
    }
    for f in &items.files {
        s = s.status_with(Role::Info, "file", |sf| {
            sf.qualifier(f.target.posix().to_string())
        });
    }
    for ev in &items.env {
        s = s.status_with(Role::Info, "env", |sf| sf.qualifier(ev.name.clone()));
    }
    for k in items.system.keys() {
        s = s.status_with(Role::Info, "system", |sf| sf.qualifier(k.clone()));
    }
    s
}

pub fn cmd_source_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let mut cfg = config::load_config(&config_path)?;
    drain_config_deprecations(printer, &mut cfg);

    let source_spec = match cfg.spec.sources.iter().find(|s| s.name == name) {
        Some(spec) => spec,
        None => {
            let available: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();
            return Err(build_source_not_found_error(name, &available));
        }
    };

    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
    let state_info = state.config_source_by_name(name)?;
    let resources = state.managed_resources_by_source(name)?;

    let config_dir = config_dir(cli);
    let lock_entry = match cfgd_core::load_sources_lockfile(&config_dir) {
        Ok(lf) => lf.sources.into_iter().find(|e| e.name == name),
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Could not read sources.lock: {}",
                    cfgd_core::output::collapse_to_subject_line(&e),
                ),
            );
            None
        }
    };

    let state_with_lock = state_info.map(|s| SourceStateInfo {
        status: s.status,
        last_fetched: s.last_fetched,
        last_commit: s.last_commit,
        version: s.source_version,
        locked_ref: lock_entry.as_ref().and_then(|e| e.resolved_ref.clone()),
        locked_commit: lock_entry.as_ref().map(|e| e.resolved_commit.clone()),
    });
    // When there is no state DB row yet (source added but never synced), still
    // surface lockfile data so callers can inspect the resolved SHA.
    let state = state_with_lock.or_else(|| {
        lock_entry.as_ref().map(|lock| SourceStateInfo {
            status: "pending".to_string(),
            last_fetched: None,
            last_commit: None,
            version: None,
            locked_ref: lock.resolved_ref.clone(),
            locked_commit: Some(lock.resolved_commit.clone()),
        })
    });

    let mut output = SourceShowOutput {
        name: name.to_string(),
        url: source_spec.origin.url.clone(),
        branch: source_spec.origin.branch.clone(),
        priority: source_spec.subscription.priority,
        accept_recommended: source_spec.subscription.accept_recommended,
        profile: source_spec.subscription.profile.clone(),
        sync_interval: source_spec.sync.interval.clone(),
        auto_apply: source_spec.sync.auto_apply,
        pin_version: source_spec.sync.pin_version.clone(),
        state,
        managed_resources: resources
            .iter()
            .map(|r| SourceResourceEntry {
                resource_type: r.resource_type.clone(),
                resource_id: r.resource_id.clone(),
            })
            .collect(),
        modules: Vec::new(),
        policy: None,
    };

    let allow_unsigned = cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned);
    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(allow_unsigned);
    let silent_printer = printer.at_verbosity(cfgd_core::output::Verbosity::Quiet);
    // `show` is a read path (Report mode, output-module.md): it renders
    // whatever `add`/`update`/`sync` already cached rather than fetching, so
    // inspecting a source never needs network or credentials of its own.
    // `load_source` would clone/fetch on every call, which also means its
    // failure is a live one — `at_verbosity(Quiet)` still surfaces a
    // `Role::Fail` line, and that line reaches this printer's own sink, not a
    // discarded one, so it would sit beside the `-o json` payload every time
    // the machine were offline.
    // The spinner runs on the OWNING printer, not `silent_printer`: the quiet
    // one exists to keep the load's own chatter out of a `-o json` payload, and
    // routing the wait through it would suppress the wait too. A cached load
    // still verifies the manifest's signature, which is a cosign subprocess.
    // Silent on success and the same single Warn on failure, so the permanent
    // output either way is what it was before the spinner existed.
    let load_spinner = printer.spinner(format!("Loading source:{name}"));
    match mgr.load_source_cached(source_spec, &silent_printer) {
        Ok(()) => load_spinner.finish_silent(),
        Err(e) => {
            let _ = load_spinner
                .finish_warn("Failed to load source manifest")
                .qualifier(cfgd_core::output::collapse_to_subject_line(&e));
        }
    }
    let manifest = mgr.get(name).map(|c| &c.manifest);

    // Modules this source DELIVERS — its manifest `provides.modules` allow-list
    // (the bodies it offers to subscribers), distinct from the policy resources
    // above. Empty when the manifest could not be loaded.
    if let Some(m) = manifest {
        output.modules = m.spec.provides.modules.clone();
        output.policy = Some(effective_source_policy(
            source_spec,
            &m.spec.policy.constraints,
            allow_unsigned,
        ));
    }

    printer.emit(build_source_show_doc(&output, manifest));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_spec(allow_scripts: bool, require_signed_commits: bool) -> SourceSpec {
        let mut spec: SourceSpec = serde_yaml::from_str(
            "name: acme\norigin:\n  type: Git\n  url: https://example.com/acme.git\n",
        )
        .unwrap();
        spec.subscription.allow_scripts = allow_scripts;
        spec.subscription.require_signed_commits = require_signed_commits;
        spec
    }

    #[test]
    fn require_signed_commits_is_the_or_of_subscriber_and_manifest() {
        let manifest_only = SourceConstraints {
            require_signed_commits: true,
            ..SourceConstraints::default()
        };
        assert!(
            effective_source_policy(&source_spec(false, false), &manifest_only, false)
                .require_signed_commits,
            "the manifest alone must be enough"
        );

        assert!(
            effective_source_policy(
                &source_spec(false, true),
                &SourceConstraints::default(),
                false
            )
            .require_signed_commits,
            "the subscriber alone must be enough"
        );

        assert!(
            !effective_source_policy(
                &source_spec(false, false),
                &SourceConstraints::default(),
                false
            )
            .require_signed_commits,
            "neither side asking must stay false"
        );
    }

    #[test]
    fn scripts_allowed_is_the_subscribers_opt_in_or_no_constraint_at_all() {
        assert!(
            !effective_source_policy(
                &source_spec(false, false),
                &SourceConstraints::default(),
                false
            )
            .scripts_allowed,
            "the default constraint (no_scripts: true) with no opt-in must disallow"
        );

        assert!(
            effective_source_policy(
                &source_spec(true, false),
                &SourceConstraints::default(),
                false
            )
            .scripts_allowed,
            "the subscriber's own opt-in must override the constraint"
        );

        let unconstrained = SourceConstraints {
            no_scripts: false,
            ..SourceConstraints::default()
        };
        assert!(
            effective_source_policy(&source_spec(false, false), &unconstrained, false)
                .scripts_allowed,
            "a manifest that does not constrain scripts needs no opt-in"
        );
    }

    #[test]
    fn secrets_system_and_target_paths_pass_through_the_manifests_constraints() {
        let constraints = SourceConstraints {
            no_secrets_read: false,
            allow_system_changes: true,
            allowed_target_paths: vec!["~/.config/**".to_string()],
            ..SourceConstraints::default()
        };
        let policy = effective_source_policy(&source_spec(false, false), &constraints, false);
        assert!(policy.secrets_read_allowed);
        assert!(policy.system_changes_allowed);
        assert_eq!(
            policy.allowed_target_paths,
            vec!["~/.config/**".to_string()]
        );
    }

    /// `security.allowUnsigned` bypasses `require_signed_commits` for THIS
    /// subscriber, and the payload must say so explicitly rather than
    /// leaving `require_signed_commits: true` to read as enforced when the
    /// check never runs. Bypassed only when there was a demand to bypass —
    /// `allow_unsigned` with no demand at all reports no bypass, since
    /// nothing was skipped.
    #[test]
    fn allow_unsigned_bypasses_the_demand_and_says_so() {
        let manifest_demands = SourceConstraints {
            require_signed_commits: true,
            ..SourceConstraints::default()
        };
        let bypassed = effective_source_policy(&source_spec(false, false), &manifest_demands, true);
        assert!(bypassed.require_signed_commits, "the demand still stands");
        assert!(
            bypassed.signed_commits_bypassed,
            "allowUnsigned must say it bypassed the demand"
        );

        let enforced =
            effective_source_policy(&source_spec(false, false), &manifest_demands, false);
        assert!(
            !enforced.signed_commits_bypassed,
            "without allowUnsigned the demand is enforced, not bypassed"
        );

        let no_demand = effective_source_policy(
            &source_spec(false, false),
            &SourceConstraints::default(),
            true,
        );
        assert!(
            !no_demand.signed_commits_bypassed,
            "allowUnsigned with no demand at all bypasses nothing"
        );
    }

    /// The manifest's `policy.constraints.encryption` must reach the
    /// rendered/serialized policy — a prior revision omitted it entirely,
    /// so `source show` never told an operator a source requires encrypted
    /// files at all.
    #[test]
    fn encryption_constraint_passes_through_to_the_policy_output() {
        let constraints = SourceConstraints {
            encryption: Some(cfgd_core::config::EncryptionConstraint {
                required_targets: vec!["secrets/**".to_string()],
                backend: Some("sops".to_string()),
                mode: Some(cfgd_core::config::EncryptionMode::Always),
            }),
            ..SourceConstraints::default()
        };
        let policy = effective_source_policy(&source_spec(false, false), &constraints, false);
        let enc = policy
            .encryption
            .expect("encryption constraint must pass through");
        assert_eq!(enc.required_targets, vec!["secrets/**".to_string()]);
        assert_eq!(enc.backend.as_deref(), Some("sops"));
        assert_eq!(enc.mode.as_deref(), Some("Always"));
    }
}
