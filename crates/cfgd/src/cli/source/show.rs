use super::*;
use crate::cli::output_types::SourcePolicyOutput;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{ConfigSourceDocument, PolicyItems, SourceConstraints, SourceSpec};
use cfgd_core::output::{Doc, KvPair, Printer, Role, doc::SectionBuilder, renderer::Table};
use cfgd_core::state::source_status_display;

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
    profiles_dir: Option<&Path>,
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
            // The two commit rows sit together and Version closes the block:
            // a reader comparing what is checked out against what the lockfile
            // pins is comparing two SHAs, and a row between them makes that a
            // search instead of a glance.
            let (status, role) = source_status_display(&state_info.status);
            let mut rows = vec![KvPair::role_valued("Status", status, role)];
            if let Some(ref fetched) = state_info.last_fetched {
                rows.push(KvPair::new("Last Fetched", fetched));
            }
            if let Some(ref commit) = state_info.last_commit {
                rows.push(KvPair::new("Last Commit", short_commit(commit)));
            }
            if let Some(ref locked_commit) = state_info.locked_commit {
                rows.push(KvPair::new("Locked Commit", short_commit(locked_commit)));
            }
            if let Some(ref locked_ref) = state_info.locked_ref {
                rows.push(KvPair::new("Locked Ref", locked_ref));
            }
            if let Some(ref version) = state_info.version {
                rows.push(KvPair::new("Version", version));
            }
            s.kv_rows(rows)
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

    if let Some(m) = manifest {
        // The policy the payload carries IS the policy the human render shows:
        // this builder is pure, so re-deriving one here from a spec would let a
        // caller's `-o json` disagree with its own screen.
        doc = source_manifest_doc_sections(doc, m, output.policy.as_ref(), profiles_dir);
    }

    doc.with_data(output)
}

/// The ONE render of what a config source IS — its manifest, the profiles it
/// provides (with their content), and the policy it enforces.
///
/// `cfgd source add` shows it before the subscribe confirm and `cfgd source
/// show` after the subscription's own state; before this existed the two
/// rendered the same `ConfigSourcePolicy` two different ways (status lines
/// against kv rows, item counts against the items themselves) and neither
/// rendered what the source's profiles actually declare — so what a subscriber
/// approved and what they could inspect afterwards were different screens.
///
/// `policy` is the ALREADY-DERIVED effective policy — [`effective_source_policy`]
/// runs once at the call site and feeds both this render and the caller's
/// payload, so no screen can contradict its own `-o json`. `None` renders no
/// Policy block at all, which is the honest answer for a caller that could not
/// derive one. `profiles_dir` is the source's checked-out `profiles/`
/// directory — absent, or unreadable for one profile, the profile still gets
/// its heading and description and simply carries no inventory.
pub fn source_manifest_doc_sections(
    doc: Doc,
    manifest: &ConfigSourceDocument,
    policy: Option<&SourcePolicyOutput>,
    profiles_dir: Option<&Path>,
) -> Doc {
    let mut doc = doc.section("Manifest", |s| {
        let mut rows = vec![KvPair::new("Name", &manifest.metadata.name)];
        if let Some(ref version) = manifest.metadata.version {
            rows.push(KvPair::new("Version", version));
        }
        if let Some(ref desc) = manifest.metadata.description {
            rows.push(KvPair::new("Description", desc));
        }
        s.kv_rows(rows)
    });

    let provided = cfgd_core::config::source_profile_names(&manifest.spec.provides);
    doc = doc.section_if_nonempty("Profiles", &provided, |s, names| {
        names.iter().fold(s, |s, name| {
            let entry = manifest
                .spec
                .provides
                .profile_details
                .iter()
                .find(|e| &e.name == name);
            // The same `profile:<name>` token an apply header names a layer
            // with — one screen must not name a profile two ways.
            s.subsection_owner(
                &cfgd_core::output::OwnerLabel::new("profile", name),
                |sub| {
                    let mut sub = match entry.and_then(|e| e.description.as_deref()) {
                        Some(desc) => sub.paragraph(desc),
                        None => sub,
                    };
                    // Three outcomes, three different facts, and only ONE of
                    // them is the source promising a profile it does not ship.
                    // "There is no checkout to look in" claims nothing; a
                    // profile whose manifest is right there but malformed
                    // (unparseable YAML, an inheritance cycle, an invalid
                    // secret/file/backup spec) sends the operator hunting for a
                    // missing file if it reads as absence — the same "could not
                    // look" vs "is absent" split `try_file_identity` draws.
                    match profiles_dir.map(|dir| cfgd_core::config::resolve_profile(name, dir)) {
                        Some(Ok(resolved)) => {
                            for (block, rows) in
                                crate::cli::profile::show::profile_inventory_blocks(&resolved)
                            {
                                if rows.is_empty() {
                                    continue;
                                }
                                sub = sub.subsection(block, |b| b.kv_rows(rows));
                            }
                            sub
                        }
                        Some(Err(e)) if profile_is_absent(&e) => sub.status(
                            Role::Warn,
                            "declared by the manifest but not found in the source",
                        ),
                        Some(Err(e)) => sub.status_with(
                            Role::Warn,
                            format!("profile {name} could not be loaded"),
                            |f| f.detail(cfgd_core::output::collapse_to_subject_line(&e)),
                        ),
                        None => sub,
                    }
                },
            )
        })
    });

    // The Policy block holds two independent halves: the DERIVED constraint
    // rows (which need `policy`) and the manifest's own TIER items (which do
    // not). A caller with no derived policy still renders the tiers — dropping
    // them would hide what the source locks and requires over a value that
    // describes something else entirely.
    let tiers = &manifest.spec.policy;
    let tiers_rendered: Vec<(&str, &PolicyItems)> = [
        ("Locked", &tiers.locked),
        ("Required", &tiers.required),
        ("Recommended", &tiers.recommended),
    ]
    .into_iter()
    .filter(|(_, items)| count_policy_items(items) > 0)
    .collect();
    if policy.is_none() && tiers_rendered.is_empty() {
        return doc;
    }
    doc.section("Policy", |s| {
        let mut s = s;
        if let Some(policy) = policy {
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
            let mut rows = vec![
                KvPair::new("Require Signed Commits", require_signed_commits_value),
                KvPair::new("Scripts Allowed", policy.scripts_allowed.to_string()),
                KvPair::new(
                    "Secrets Read Allowed",
                    policy.secrets_read_allowed.to_string(),
                ),
                KvPair::new(
                    "System Changes Allowed",
                    policy.system_changes_allowed.to_string(),
                ),
            ];
            if !policy.allowed_target_paths.is_empty() {
                rows.push(KvPair::new(
                    "Allowed Target Paths",
                    policy.allowed_target_paths.join(", "),
                ));
            }
            s = s.kv_rows(rows);
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
        }
        // The rows ARE the count: a `Count` row above them restated what the
        // reader can see, and disagreed with it the moment a tier carried an
        // item kind `append_policy_items` does not render.
        for (name, items) in tiers_rendered {
            s = s.subsection(name, |inner| append_policy_items(inner, items));
        }
        s
    })
}

/// The manifest as a structured payload — the `-o json` counterpart of
/// [`source_manifest_doc_sections`]'s `Manifest` and `Profiles` sections, so a
/// machine consumer reads the same facts the human render shows.
pub fn source_manifest_output(
    manifest: &ConfigSourceDocument,
) -> crate::cli::output_types::SourceManifestOutput {
    use crate::cli::output_types::{SourceManifestOutput, SourceManifestProfileOutput};
    let details = &manifest.spec.provides.profile_details;
    SourceManifestOutput {
        name: manifest.metadata.name.clone(),
        version: manifest.metadata.version.clone(),
        description: manifest.metadata.description.clone(),
        profiles: cfgd_core::config::source_profile_names(&manifest.spec.provides)
            .into_iter()
            .map(|name| {
                let entry = details.iter().find(|e| e.name == name);
                SourceManifestProfileOutput {
                    description: entry.and_then(|e| e.description.clone()),
                    inherits: entry.map(|e| e.inherits.clone()).unwrap_or_default(),
                    name,
                }
            })
            .collect(),
        modules: manifest.spec.provides.modules.clone(),
    }
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
/// `source_spec` is `None` for a caller that has no subscription yet (`cfgd
/// source add`, before the confirm): the manifest's own constraints are then
/// the whole answer, since there are no subscriber overrides to combine with.
pub fn effective_source_policy(
    source_spec: Option<&SourceSpec>,
    constraints: &SourceConstraints,
    allow_unsigned: bool,
) -> SourcePolicyOutput {
    let require_signed_commits = source_spec
        .map(|s| s.requires_signed_commits(constraints.require_signed_commits))
        .unwrap_or(constraints.require_signed_commits);
    SourcePolicyOutput {
        require_signed_commits,
        signed_commits_bypassed: require_signed_commits && allow_unsigned,
        scripts_allowed: source_spec.is_some_and(|s| s.subscription.allow_scripts)
            || !constraints.no_scripts,
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

/// Whether a `resolve_profile` failure means the profile is ABSENT from the
/// checkout, as opposed to present and unloadable.
///
/// Only the two not-found shapes may be reported as a source promising a
/// profile it does not ship; a parse error, an inheritance cycle and an invalid
/// secret/file/backup spec all describe a file that IS there.
fn profile_is_absent(err: &cfgd_core::errors::CfgdError) -> bool {
    matches!(
        err,
        cfgd_core::errors::CfgdError::Config(
            cfgd_core::errors::ConfigError::ProfileNotFound { .. }
                | cfgd_core::errors::ConfigError::NotFound { .. }
        )
    )
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
        manifest: None,
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
            Some(source_spec),
            &m.spec.policy.constraints,
            allow_unsigned,
        ));
        output.manifest = Some(source_manifest_output(m));
    }

    let profiles_dir = mgr.source_profiles_dir(name).ok();
    printer.emit(build_source_show_doc(
        &output,
        manifest,
        profiles_dir.as_deref(),
    ));
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
            effective_source_policy(Some(&source_spec(false, false)), &manifest_only, false)
                .require_signed_commits,
            "the manifest alone must be enough"
        );

        assert!(
            effective_source_policy(
                Some(&source_spec(false, true)),
                &SourceConstraints::default(),
                false
            )
            .require_signed_commits,
            "the subscriber alone must be enough"
        );

        assert!(
            !effective_source_policy(
                Some(&source_spec(false, false)),
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
                Some(&source_spec(false, false)),
                &SourceConstraints::default(),
                false
            )
            .scripts_allowed,
            "the default constraint (no_scripts: true) with no opt-in must disallow"
        );

        assert!(
            effective_source_policy(
                Some(&source_spec(true, false)),
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
            effective_source_policy(Some(&source_spec(false, false)), &unconstrained, false)
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
        let policy = effective_source_policy(Some(&source_spec(false, false)), &constraints, false);
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
        let bypassed =
            effective_source_policy(Some(&source_spec(false, false)), &manifest_demands, true);
        assert!(bypassed.require_signed_commits, "the demand still stands");
        assert!(
            bypassed.signed_commits_bypassed,
            "allowUnsigned must say it bypassed the demand"
        );

        let enforced =
            effective_source_policy(Some(&source_spec(false, false)), &manifest_demands, false);
        assert!(
            !enforced.signed_commits_bypassed,
            "without allowUnsigned the demand is enforced, not bypassed"
        );

        let no_demand = effective_source_policy(
            Some(&source_spec(false, false)),
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
        let policy = effective_source_policy(Some(&source_spec(false, false)), &constraints, false);
        let enc = policy
            .encryption
            .expect("encryption constraint must pass through");
        assert_eq!(enc.required_targets, vec!["secrets/**".to_string()]);
        assert_eq!(enc.backend.as_deref(), Some("sops"));
        assert_eq!(enc.mode.as_deref(), Some("Always"));
    }
}
