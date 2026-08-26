use super::*;
use cfgd_core::output::{Doc, OwnerLabel, Printer, Role};

/// Write a captured [`SubscriptionSpec`] back over the re-homed source's entry.
///
/// The whole block is replaced rather than field-by-field, so a field added to
/// `SubscriptionSpec` later is carried by construction instead of waiting for
/// someone to remember a flag for it.
///
/// [`SubscriptionSpec`]: config::SubscriptionSpec
fn restore_subscription(
    config_path: &Path,
    name: &str,
    subscription: &config::SubscriptionSpec,
) -> anyhow::Result<()> {
    let value = serde_yaml::to_value(subscription)?;
    with_source_config(config_path, name, |entry| {
        entry
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("source '{name}' is not a mapping"))?
            .insert(serde_yaml::Value::String("subscription".into()), value);
        Ok(())
    })
}

pub fn cmd_source_replace(
    cli: &Cli,
    printer: &Printer,
    old_name: &str,
    new_url: &str,
) -> anyhow::Result<()> {
    // Resolved here as well as in `cmd_source_add` (resolution is idempotent) so
    // the success line and its structured payload report the URL that was
    // actually subscribed to, not the shorthand.
    let new_url = &*cfgd_core::resolve_repo_reference(new_url);
    printer.heading_owner_prefixed("Replace", &OwnerLabel::new("source", old_name));

    // Capture old source's profile and priority before removing
    let config_path = cli.config.clone();
    let mut old_cfg = config::load_config(&config_path)?;
    drain_config_deprecations(printer, &mut old_cfg);
    let old_source = old_cfg.spec.sources.iter().find(|s| s.name == old_name);
    let old_profile = old_source.and_then(|s| s.subscription.profile.clone());
    let old_priority = old_source.map(|s| s.subscription.priority).unwrap_or(500);
    // A re-home points one subscription at a new URL; every knob on it is a
    // decision the operator never revoked, so the WHOLE block is carried.
    // `SourceAddArgs` can express only the six knobs `cfgd source add` itself
    // exposes — `overrides` and `reject` have no flag — so the add below seeds
    // what it can and `restore_subscription` writes the rest back afterwards.
    let old_subscription = old_source
        .map(|s| s.subscription.clone())
        .unwrap_or_default();

    // Remove old source (keeping resources). Confirmation-free: a re-home
    // purges nothing, so there is no forget-my-edits question to ask, and a
    // replace must not stop mid-way to pose one.
    remove::run_source_remove(cli, printer, old_name, true, false, true, false, false)?;

    // Add new source with same name, carrying over the whole subscription
    add::run_source_add(
        cli,
        printer,
        &SourceAddArgs {
            url: new_url.to_string(),
            name: Some(old_name.to_string()),
            branch: None,
            profile: old_profile,
            accept_recommended: old_subscription.accept_recommended,
            priority: Some(old_priority),
            opt_in: old_subscription.opt_in.clone(),
            sync_interval: None,
            auto_apply: false,
            pin_version: None,
            require_signed_commits: old_subscription.require_signed_commits,
            allow_scripts: old_subscription.allow_scripts,
            yes: true,
        },
        false,
    )?;

    restore_subscription(&config_path, old_name, &old_subscription)?;

    printer.emit(
        Doc::new()
            .status(Role::Ok, format!("Replaced with {}", new_url))
            .hint(super::source_success_next_step(
                super::SourceMutation::Replaced,
            ))
            .with_data(serde_json::json!({
                "oldName": old_name,
                "newUrl": new_url,
            })),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field set to a non-default value, so nothing can survive the
    /// round trip by accident.
    fn fully_populated() -> config::SubscriptionSpec {
        config::SubscriptionSpec {
            profile: Some("dev".into()),
            priority: 700,
            accept_recommended: true,
            opt_in: vec!["tmux".into()],
            allow_scripts: true,
            require_signed_commits: true,
            overrides: serde_yaml::from_str("env:\n  EDITOR: vim\n").expect("overrides"),
            reject: serde_yaml::from_str("packages:\n  - htop\n").expect("reject"),
        }
    }

    /// Walks the SERIALIZED struct rather than a hand-written field list: a
    /// ninth field added to `SubscriptionSpec` is carried into the assertion
    /// automatically, so dropping it from the re-homed entry fails here.
    #[test]
    fn every_subscription_field_survives_the_restore() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(
            &path,
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: acme\n      origin:\n        type: Git\n        url: https://example.com/acme/dev.git\n        branch: main\n",
        )
        .expect("write seed config");

        let spec = fully_populated();
        restore_subscription(&path, "acme", &spec).expect("restore subscription");

        let written: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("parse config");
        let on_disk = written["spec"]["sources"][0]["subscription"]
            .as_mapping()
            .expect("subscription block is a mapping")
            .clone();

        let expected = serde_yaml::to_value(&spec).expect("serialize spec");
        let expected = expected.as_mapping().expect("spec serializes to a mapping");
        assert!(
            !expected.is_empty(),
            "a spec that serializes to nothing proves nothing"
        );
        for (key, value) in expected {
            assert_eq!(
                on_disk.get(key),
                Some(value),
                "field {key:?} did not survive the re-home"
            );
        }

        // And it parses back as the same spec, not merely as the same keys.
        let reloaded = config::load_config(&path).expect("reload config");
        assert_eq!(
            serde_yaml::to_value(&reloaded.spec.sources[0].subscription).expect("serialize"),
            serde_yaml::to_value(&spec).expect("serialize")
        );
    }
}
