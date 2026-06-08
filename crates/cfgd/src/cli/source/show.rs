use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::config::{ConfigSourceDocument, PolicyItems};
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
    let mut doc = Doc::new()
        .heading(format!("Source: {}", output.name))
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

fn append_policy_items(mut s: SectionBuilder, items: &PolicyItems) -> SectionBuilder {
    if let Some(ref pkgs) = items.packages {
        if let Some(ref brew) = pkgs.brew {
            for f in &brew.formulae {
                s = s.status(Role::Info, format!("brew formula: {f}"));
            }
            for c in &brew.casks {
                s = s.status(Role::Info, format!("brew cask: {c}"));
            }
        }
        if let Some(ref apt) = pkgs.apt {
            for p in &apt.packages {
                s = s.status(Role::Info, format!("apt: {p}"));
            }
        }
        if let Some(ref cargo) = pkgs.cargo {
            for p in &cargo.packages {
                s = s.status(Role::Info, format!("cargo: {p}"));
            }
        }
        for p in &pkgs.pipx {
            s = s.status(Role::Info, format!("pipx: {p}"));
        }
        for p in &pkgs.dnf {
            s = s.status(Role::Info, format!("dnf: {p}"));
        }
        if let Some(ref npm) = pkgs.npm {
            for p in &npm.global {
                s = s.status(Role::Info, format!("npm: {p}"));
            }
        }
    }
    for f in &items.files {
        s = s.status(Role::Info, format!("file: {}", f.target.posix()));
    }
    for ev in &items.env {
        s = s.status(Role::Info, format!("env: {}", ev.name));
    }
    for k in items.system.keys() {
        s = s.status(Role::Info, format!("system: {k}"));
    }
    s
}

pub fn cmd_source_show(cli: &Cli, printer: &Printer, name: &str) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source_spec = match cfg.spec.sources.iter().find(|s| s.name == name) {
        Some(spec) => spec,
        None => {
            let available: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();
            return Err(build_source_not_found_error(name, &available));
        }
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_info = state.config_source_by_name(name)?;
    let resources = state.managed_resources_by_source(name)?;

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
        state: state_info.map(|s| SourceStateInfo {
            status: s.status,
            last_fetched: s.last_fetched,
            last_commit: s.last_commit,
            version: s.source_version,
        }),
        managed_resources: resources
            .iter()
            .map(|r| SourceResourceEntry {
                resource_type: r.resource_type.clone(),
                resource_id: r.resource_id.clone(),
            })
            .collect(),
        modules: Vec::new(),
    };

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    let silent_printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
    if let Err(e) = mgr.load_source(source_spec, &silent_printer) {
        printer.status_simple(
            Role::Warn,
            format!(
                "Failed to load source manifest: {}",
                cfgd_core::output::collapse_to_subject_line(&e),
            ),
        );
    }
    let manifest = mgr.get(name).map(|c| &c.manifest);

    // Modules this source DELIVERS — its manifest `provides.modules` allow-list
    // (the bodies it offers to subscribers), distinct from the policy resources
    // above. Empty when the manifest could not be loaded.
    if let Some(m) = manifest {
        output.modules = m.spec.provides.modules.clone();
    }

    printer.emit(build_source_show_doc(&output, manifest));
    Ok(())
}
