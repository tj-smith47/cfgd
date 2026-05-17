use super::*;
use cfgd_core::config::{ConfigSourceDocument, PolicyItems};
use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role, doc::SectionBuilder, renderer::Table};

const SHORT_COMMIT_LEN: usize = 12;

/// Doc for the `source not found` error path. Emitted before bailing so JSON
/// consumers see a structured error with the list of available sources.
pub fn build_source_not_found_doc(name: &str, available: &[String]) -> Doc {
    let mut doc = Doc::new().status(Role::Fail, format!("Source '{}' not found", name));
    if !available.is_empty() {
        doc = doc.hint(format!("Available sources: {}", available.join(", ")));
    }
    doc.with_data(serde_json::json!({
        "error": "not_found",
        "name": name,
        "available": available,
    }))
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
    if let Some(ref pin) = output.version_pin {
        doc = doc.kv("Version Pin", pin);
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
        s = s.status(Role::Info, format!("file: {}", f.target.display()));
    }
    for ev in &items.env {
        s = s.status(Role::Info, format!("env: {}", ev.name));
    }
    for k in items.system.keys() {
        s = s.status(Role::Info, format!("system: {k}"));
    }
    s
}

pub(crate) fn cmd_source_show(
    cli: &Cli,
    printer: &Printer,
    v2_printer: &PrinterV2,
    name: &str,
) -> anyhow::Result<()> {
    let config_path = cli.config.clone();
    let cfg = config::load_config(&config_path)?;

    let source_spec = match cfg.spec.sources.iter().find(|s| s.name == name) {
        Some(spec) => spec,
        None => {
            let available: Vec<String> = cfg.spec.sources.iter().map(|s| s.name.clone()).collect();
            v2_printer.emit(build_source_not_found_doc(name, &available));
            anyhow::bail!("Source '{}' not found", name);
        }
    };

    let state = open_state_store(cli.state_dir.as_deref())?;
    let state_info = state.config_source_by_name(name)?;
    let resources = state.managed_resources_by_source(name)?;

    let output = SourceShowOutput {
        name: name.to_string(),
        url: source_spec.origin.url.clone(),
        branch: source_spec.origin.branch.clone(),
        priority: source_spec.subscription.priority,
        accept_recommended: source_spec.subscription.accept_recommended,
        profile: source_spec.subscription.profile.clone(),
        sync_interval: source_spec.sync.interval.clone(),
        auto_apply: source_spec.sync.auto_apply,
        version_pin: source_spec.sync.pin_version.clone(),
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
    };

    let cache_dir = source_cache_dir(cli)?;
    let mut mgr = SourceManager::new(&cache_dir);
    mgr.set_allow_unsigned(cfg.spec.security.as_ref().is_some_and(|s| s.allow_unsigned));
    if let Err(e) = mgr.load_source(source_spec, printer) {
        v2_printer.status_simple(Role::Warn, format!("Failed to load source manifest: {}", e));
    }
    let manifest = mgr.get(name).map(|c| &c.manifest);

    v2_printer.emit(build_source_show_doc(&output, manifest));
    Ok(())
}
