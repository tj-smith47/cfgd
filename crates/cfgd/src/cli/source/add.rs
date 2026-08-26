use super::*;
use cfgd_core::output::{Doc, OwnerLabel, Printer, Role};

pub fn cmd_source_add(cli: &Cli, printer: &Printer, args: &SourceAddArgs) -> anyhow::Result<()> {
    add_source(cli, printer, args, true)
}

/// The body of `cfgd source add`. `closing` is whether this add is the whole
/// command: a `source replace` runs one inside its own report and closes on its
/// own verdict, so the next-step hint belongs to the caller's last line, not to
/// a `Subscribed` row mid-screen.
pub(super) fn add_source(
    cli: &Cli,
    printer: &Printer,
    args: &SourceAddArgs,
    closing: bool,
) -> anyhow::Result<()> {
    // Resolve the reference before anything reads the URL, so the inferred name,
    // the clone, and the persisted `spec.sources[].origin` all carry one string.
    // An existing local path stays itself (and is then refused by `load_source`
    // as a source origin, which is a truthful answer to what the user named);
    // a GitHub `owner/repo` shorthand expands; any other shape passes through.
    let resolved_url = cfgd_core::resolve_repo_reference(&args.url);
    let url = &*resolved_url;
    let name = args.name.as_deref();
    let branch = args.branch.as_deref();
    let profile = args.profile.as_deref();
    let accept_recommended = args.accept_recommended;
    let priority = args.priority;
    let opt_in = &args.opt_in;
    let sync_interval = args.sync_interval.as_deref();
    let auto_apply = args.auto_apply;
    let pin_version = args.pin_version.as_deref();
    // Infer name from URL if not provided
    let source_name = name
        .map(|s| s.to_string())
        .unwrap_or_else(|| infer_source_name(url));
    printer.heading_owner_prefixed("Add", &OwnerLabel::new("source", &source_name));

    // A pin selects its own git ref (tag or commit), so an explicit branch is
    // meaningless and contradictory — reject the combination before any clone.
    if args.branch.is_some() && args.pin_version.is_some() {
        return Err(crate::cli::cli_error(
            &source_name,
            "branch_pin_conflict",
            "--branch and --pin-version are mutually exclusive; a pin selects its own ref",
            serde_json::json!({}),
        ));
    }

    // Argument-injection guard: a `-`-leading pin would be parsed as a git flag
    // by the downstream `git fetch`/`checkout`. Reject early with a clear error.
    if pin_version.is_some_and(|p| p.trim_start().starts_with('-')) {
        return Err(crate::cli::cli_error(
            &source_name,
            "invalid_pin_version",
            "--pin-version must not start with '-' (a leading dash is reserved for git flags)",
            serde_json::json!({}),
        ));
    }

    // Check if source already exists in config
    let config_path = cli.config.clone();
    if config_path.exists() {
        let mut cfg = config::load_config(&config_path)?;
        drain_config_deprecations(printer, &mut cfg);
        if cfg.spec.sources.iter().any(|s| s.name == source_name) {
            return Err(crate::cli::cli_error(
                &source_name,
                "already_exists",
                format!(
                    "Source '{}' already exists. Use `cfgd source update` to refresh.",
                    source_name
                ),
                serde_json::json!({}),
            ));
        }
    }

    // Clone and parse the source
    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    let allow_unsigned = config_path.exists()
        && config::load_config(&config_path)
            .is_ok_and(|c| c.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    mgr.set_allow_unsigned(allow_unsigned);
    let mut spec = SourceManager::build_source_spec(&source_name, url, profile);
    if let Some(b) = branch {
        spec.origin.branch = b.to_string();
    }
    // Apply the pin at clone time so resolution selects the pinned ref now,
    // not just when it is later persisted to config.
    if let Some(pin) = pin_version {
        spec.sync.pin_version = Some(pin.to_string());
    }
    // Set BEFORE the clone, unlike `source update`'s counterpart: there is no
    // prior fetch this demand could be read as describing, so a subscription
    // that demands a signature is verified by the very fetch that establishes
    // it rather than accepting an unsigned HEAD once and refusing it later.
    spec.subscription.require_signed_commits = args.require_signed_commits;
    spec.subscription.allow_scripts = args.allow_scripts;
    // Surface lib-side load failure with the same {"error": "load_failed", ...}
    // structured shape as the "Ok-but-no-cache-entry" fallback below, so both
    // load-failure paths look identical to structured consumers.
    // The clone is the wait. It retires silently on both arms because the
    // failure below is already worded as its own line.
    let load = printer.narrate_silent(format!("Fetching source:{source_name}"), |_| {
        mgr.load_source(&spec, printer)
    });
    if let Err(e) = load {
        return Err(crate::cli::cli_error(
            &source_name,
            "load_failed",
            // The cause, not the whole sentence: this line already names the
            // source, and the wrapped error names it again.
            format!(
                "Failed to load source '{}': {}",
                source_name,
                super::source_failure_detail(&e)
            ),
            serde_json::json!({ "url": url }),
        ));
    }

    let cached = match mgr.get(&source_name) {
        Some(c) => c,
        None => {
            return Err(crate::cli::cli_error(
                &source_name,
                "load_failed",
                format!("Failed to load source '{}'", source_name),
                serde_json::json!({ "url": url }),
            ));
        }
    };

    // What the source IS, through the same composer `cfgd source show` renders
    // afterwards: the policy is EFFECTIVE for the subscription about to be
    // written, since `spec` already carries the two subscriber knobs
    // (`--require-signed-commits`, `--allow-scripts`) that combine with the
    // manifest's constraints.
    let manifest = &cached.manifest;
    let provided_profiles = cfgd_core::config::source_profile_names(&manifest.spec.provides);
    let profiles_dir = mgr.source_profiles_dir(&source_name).ok();
    let policy = super::show::effective_source_policy(
        Some(&spec),
        &manifest.spec.policy.constraints,
        allow_unsigned,
    );
    printer.emit(super::show::source_manifest_doc_sections(
        Doc::new(),
        manifest,
        Some(&policy),
        profiles_dir.as_deref(),
    ));

    // Profile selection: explicit flag > platform auto-detect > single profile > interactive
    let auto_detected_profile =
        if profile.is_none() && !manifest.spec.provides.platform_profiles.is_empty() {
            let platform = cfgd_core::config::detect_platform();
            cfgd_core::config::match_platform_profile(
                &platform,
                &manifest.spec.provides.platform_profiles,
            )
            .inspect(|matched| {
                printer.status_simple(
                    Role::Ok,
                    format!(
                        "Auto-selected profile '{}' for platform {}",
                        matched,
                        platform.distro.as_deref().unwrap_or(&platform.os)
                    ),
                );
            })
        } else {
            None
        };

    let selected_profile: Option<String> = match resolve_non_interactive_profile(
        profile,
        auto_detected_profile.as_deref(),
        &provided_profiles,
    ) {
        Some(p) => Some(p),
        None if provided_profiles.is_empty() => None,
        None => {
            let selection =
                printer.prompt_select("Select a profile to subscribe to:", &provided_profiles)?;
            Some(selection.clone())
        }
    };

    // Interactive priority prompt (when --priority not specified on command line)
    let resolved_priority = if let Some(p) = priority {
        cfgd_core::config::validate_source_priority(p).map_err(|m| anyhow::anyhow!(m))?
    } else if args.yes {
        DEFAULT_NONINTERACTIVE_PRIORITY
    } else {
        let input = printer.prompt_text("Set priority", "500")?;
        parse_priority_input(&input)?
    };

    // Conflict preview: check for conflicts with current config before subscribing
    if config_path.exists()
        && let Ok(cfg) = config::load_config(&config_path)
    {
        let pdir = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("profiles");
        let profile_name = cli.profile.as_deref().or(cfg.spec.profile.as_deref());

        if let Some(pn) = profile_name
            && let Ok(local_resolved) = config::resolve_profile(pn, &pdir)
        {
            let mut preview_layers = Vec::new();
            if let Some(ref pn) = selected_profile
                && let Ok(src_profiles_dir) = mgr.source_profiles_dir(&source_name)
                && src_profiles_dir.exists()
                && let Ok(r) = config::resolve_profile(pn, &src_profiles_dir)
            {
                preview_layers = r.layers;
            }

            let preview_input = build_subscription_preview_input(
                &source_name,
                resolved_priority,
                &manifest.spec.policy,
                accept_recommended,
                opt_in,
                preview_layers,
            );

            match composition::compose(
                &local_resolved,
                &[preview_input],
                composition::ConstraintMode::Enforce,
            ) {
                Ok(result) => {
                    let lines = format_conflict_preview_lines(&result.conflicts);
                    if lines.is_empty() {
                        // Role::Ok marks the conflict-check step as having passed cleanly
                        // (consistent with other clean-state preview steps).
                        // verdict-row-ok: a comparison verdict, not an act cfgd performed
                        printer.status_simple(Role::Ok, "No conflicts with current config");
                    } else {
                        let conflicts_sec = printer.section("Conflicts with Current Config");
                        for line in &lines {
                            conflicts_sec.status_simple(Role::Warn, line.clone());
                        }
                    }
                }
                Err(e) => {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "Failed to preview conflicts: {}",
                            cfgd_core::output::collapse_to_subject_line(&e),
                        ),
                    );
                }
            }
        }
    }

    // Confirm subscription
    if !args.yes && !printer.prompt_confirm("Subscribe to this source?")? {
        printer.emit(
            Doc::new()
                .status(Role::Info, "Cancelled")
                .with_data(serde_json::json!({
                    "name": source_name,
                    "url": url,
                    "cancelled": true,
                })),
        );
        return Ok(());
    }

    // Build the source spec with user choices
    let mut source_spec =
        SourceManager::build_source_spec(&source_name, url, selected_profile.as_deref());
    if let Some(b) = branch {
        source_spec.origin.branch = b.to_string();
    }
    source_spec.subscription.accept_recommended = accept_recommended;
    source_spec.subscription.priority = resolved_priority;
    source_spec.subscription.require_signed_commits = args.require_signed_commits;
    source_spec.subscription.allow_scripts = args.allow_scripts;
    if !opt_in.is_empty() {
        source_spec.subscription.opt_in = opt_in.to_vec();
    }
    if let Some(interval) = sync_interval {
        source_spec.sync.interval = interval.to_string();
    }
    if auto_apply {
        source_spec.sync.auto_apply = true;
    }
    if let Some(pin) = pin_version {
        source_spec.sync.pin_version = Some(pin.to_string());
    }

    // Update cfgd.yaml
    add_source_to_config(&config_path, &source_spec)?;

    // Update state store
    let state = open_state_store(cli.state_dir.as_deref(), cli.scope())?;
    state.upsert_config_source(&cfgd_core::state::ConfigSourceUpsert {
        name: &source_name,
        origin_url: url,
        origin_branch: &spec.origin.branch,
        last_commit: cached.last_commit.as_deref(),
        source_version: manifest.metadata.version.as_deref(),
        pinned_version: None,
        last_commit_signed: cached.head_signed,
    })?;

    // Record the resolved commit SHA in the sources lockfile so composition
    // is bit-reproducible across machines.
    if let Some(ref commit) = cached.last_commit {
        let lock_entry = cfgd_core::config::SourceLockEntry {
            name: source_name.clone(),
            url: url.to_string(),
            pin_version: pin_version.map(|s| s.to_string()),
            resolved_ref: cached.resolved_ref.clone(),
            resolved_commit: commit.clone(),
            locked_at: cfgd_core::utc_now_iso8601(),
        };
        let cfg_dir = config_dir(cli);
        if let Err(e) = cfgd_core::update_source_lock_entry(&cfg_dir, lock_entry) {
            printer.status_simple(
                cfgd_core::output::Role::Warn,
                super::sources_lock_update_warning(&e),
            );
        }
    }

    // No `Profile` row: the manifest render above already showed the profile
    // this subscription activates, under its own `profile:<name>` owner, and a
    // headingless key/value pair restating it cannot be read on its own. The
    // payload below still carries the field.
    let mut doc = Doc::new().status(Role::Ok, "Subscribed");
    if closing {
        doc = doc.hint(super::source_success_next_step(
            super::SourceMutation::Subscribed,
        ));
    }
    let doc = doc.with_data(serde_json::json!({
        "name": source_name,
        "url": url,
        "branch": source_spec.origin.branch,
        "commit": cached.last_commit.clone().unwrap_or_default(),
        "profile": selected_profile,
        "priority": resolved_priority,
        // Additive: the same manifest object `source show` carries, so a
        // consumer scripting a subscription reads what it subscribed TO
        // without a second `source show` call.
        "manifest": super::show::source_manifest_output(manifest),
    }));
    printer.emit(doc);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::output::OutputFormat;

    fn base_args(url: &str) -> SourceAddArgs {
        SourceAddArgs {
            url: url.to_string(),
            name: None,
            branch: None,
            profile: None,
            accept_recommended: false,
            priority: None,
            opt_in: Vec::new(),
            sync_interval: None,
            auto_apply: false,
            pin_version: None,
            yes: true,
            require_signed_commits: false,
            allow_scripts: false,
        }
    }

    fn cli_for(config: PathBuf) -> Cli {
        Cli {
            config,
            config_explicit: false,
            profile: None,
            verbose: 0,
            quiet: true,
            no_color: true,
            color: crate::cli::ColorWhen::Auto,
            output: crate::cli::OutputFormatArg(OutputFormat::Table),
            list_envelope: false,
            theme: None,
            jsonpath: None,
            yes: false,
            state_dir: None,
            config_dir: None,
            cache_dir: None,
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: None,
        }
    }

    fn meta_of(err: &anyhow::Error) -> &crate::cli::CliErrorMeta {
        err.downcast_ref::<crate::cli::CliErrorMeta>()
            .expect("source add handler must return CliErrorMeta")
    }

    #[test]
    fn add_rejects_branch_and_pin_version_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.branch = Some("main".into());
        args.pin_version = Some("v1.0.0".into());

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("branch + pin must be rejected before any clone");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "branch_pin_conflict",
            "expected branch_pin_conflict, got: {meta:?}"
        );
        assert_eq!(
            meta.name, "dev",
            "error name must be the source name inferred from the URL's final path segment"
        );
    }

    #[test]
    fn add_rejects_pin_version_with_leading_dash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.pin_version = Some("--upload-pack=evil".into());

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("dash-leading pin is an argument-injection risk and must be rejected");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "invalid_pin_version",
            "expected invalid_pin_version, got: {meta:?}"
        );
    }

    #[test]
    fn add_rejects_pin_version_with_leading_whitespace_dash() {
        // The guard trims leading whitespace before the dash check, so a
        // " -flag" pin must also be rejected.
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = cli_for(dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.pin_version = Some("  -x".into());

        let err = cmd_source_add(&cli, &printer, &args).expect_err("whitespace+dash pin rejected");
        drop(printer);

        assert_eq!(meta_of(&err).error_kind, "invalid_pin_version");
    }

    #[test]
    fn add_rejects_duplicate_source_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("cfgd.yaml");
        // Seed a config that already subscribes to a source named "dev"
        // (the name inferred from the URL below's final path segment).
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: dev\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n",
        )
        .expect("write seed config");
        let cli = cli_for(config_path);
        let (printer, _cap) = Printer::for_test_doc();

        let args = base_args("https://example.com/acme/dev.git");

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("re-adding an existing source name must error before clone");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "already_exists",
            "expected already_exists, got: {meta:?}"
        );
        assert_eq!(meta.name, "dev");
        assert!(
            meta.message.contains("cfgd source update"),
            "already_exists message must point to 'source update', got: {}",
            meta.message
        );
    }

    #[test]
    fn add_names_a_github_shorthand_the_same_as_its_full_url() {
        // `cfgd source add acme/dev` and `cfgd source add https://github.com/acme/dev`
        // must land on ONE subscription, which is a claim about the inferred
        // name only: `infer_source_name` reads the last path segment, so it
        // answers `dev` either way and this test would pass with no expansion
        // at all. What the expansion itself does to the URL is pinned by
        // `add_hands_the_expanded_shorthand_to_the_source_load` below.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: dev\n      origin:\n        type: Git\n        url: https://github.com/acme/dev.git\n        branch: main\n",
        )
        .expect("write seed config");
        let cli = cli_for(config_path);
        let (printer, _cap) = Printer::for_test_doc();

        let err = cmd_source_add(&cli, &printer, &base_args("acme/dev"))
            .expect_err("shorthand for an already-subscribed repo must error before clone");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(
            meta.error_kind, "already_exists",
            "expected already_exists, got: {meta:?}"
        );
        assert_eq!(meta.name, "dev");
    }

    /// A `cli` whose source cache cannot be created, so `load_source` fails at
    /// its first filesystem step — before any clone, and with no network. That
    /// is the earliest point at which the URL the command resolved becomes
    /// observable: it is carried verbatim in the `load_failed` payload.
    fn cli_with_unusable_cache(dir: &std::path::Path, config: PathBuf) -> Cli {
        std::fs::write(dir.join("blocker"), "not a directory").expect("write blocker file");
        let mut cli = cli_for(config);
        cli.cache_dir = Some(dir.join("blocker").join("cache"));
        cli
    }

    fn load_failed_url(err: &anyhow::Error) -> String {
        let meta = meta_of(err);
        assert_eq!(
            meta.error_kind, "load_failed",
            "expected the load to fail offline, got: {meta:?}"
        );
        meta.extras
            .get("url")
            .and_then(|v| v.as_str())
            .expect("load_failed payload carries the resolved url")
            .to_string()
    }

    #[test]
    #[serial_test::serial]
    fn add_hands_the_expanded_shorthand_to_the_source_load() {
        // The shorthand has to be expanded before `build_source_spec`, or the
        // subscription is recorded against a string git cannot clone. The
        // `load_failed` payload reports the URL that reached the load, so
        // dropping the expansion flips this assertion.
        //
        // `acme/dev` is resolved against the process CWD, which is global and
        // which its negative sibling below moves into a directory holding a
        // real `acme/dev`. Unserialized, the two overlap and this test reads
        // the other one's world: the shorthand resolves to a local path, no
        // expansion happens, and a correct build goes red. Both the guard and
        // the `serial` are the fix — a guard alone only excludes other serial
        // tests, and `serial` alone leaves the answer to whatever the suite was
        // started from.
        let dir = tempfile::tempdir().expect("tempdir");
        let _cwd = cfgd_core::test_helpers::CwdGuard::set(dir.path()).expect("cwd guard");
        let cli = cli_with_unusable_cache(dir.path(), dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("acme/dev");
        args.name = Some("shorthand".into());

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("an uncreatable source cache fails the load before any clone");
        drop(printer);

        assert_eq!(
            load_failed_url(&err),
            "https://github.com/acme/dev.git",
            "the shorthand must reach the source load already expanded"
        );
    }

    #[test]
    #[serial_test::serial]
    fn add_keeps_an_existing_relative_path_out_of_github() {
        // The same value, with a directory of that name on disk: it means the
        // directory, and `source add` says so instead of silently subscribing
        // to a stranger's github.com/acme/dev.
        let dir = tempfile::tempdir().expect("tempdir");
        let _cwd = cfgd_core::test_helpers::CwdGuard::set(dir.path()).expect("cwd guard");
        std::fs::create_dir_all(dir.path().join("acme").join("dev")).expect("create local repo");
        let cli = cli_with_unusable_cache(dir.path(), dir.path().join("cfgd.yaml"));
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("acme/dev");
        args.name = Some("local".into());

        let err = cmd_source_add(&cli, &printer, &args)
            .expect_err("a local path is not a valid source origin");
        drop(printer);

        assert_eq!(
            load_failed_url(&err),
            "acme/dev",
            "an existing relative path must never be expanded into a GitHub URL"
        );
        assert!(
            meta_of(&err).message.contains("local path"),
            "the failure must name what the user actually pointed at, got: {}",
            meta_of(&err).message
        );
    }

    #[test]
    fn add_explicit_name_overrides_inferred_for_duplicate_check() {
        // With --name set, the duplicate check keys off the explicit name, not
        // the URL-inferred one.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &config_path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: custom\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n",
        )
        .expect("write seed config");
        let cli = cli_for(config_path);
        let (printer, _cap) = Printer::for_test_doc();

        let mut args = base_args("https://example.com/acme/dev.git");
        args.name = Some("custom".into());

        let err =
            cmd_source_add(&cli, &printer, &args).expect_err("duplicate explicit name must error");
        drop(printer);

        let meta = meta_of(&err);
        assert_eq!(meta.error_kind, "already_exists");
        assert_eq!(
            meta.name, "custom",
            "duplicate check must use the explicit --name"
        );
    }
}
