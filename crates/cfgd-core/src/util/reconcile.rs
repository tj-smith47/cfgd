use crate::config;

/// Fully resolved reconcile settings for a single entity (no Options).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EffectiveReconcile {
    pub interval: String,
    pub auto_apply: bool,
    pub drift_policy: config::DriftPolicy,
}

/// Resolve effective reconcile settings for a module given the profile
/// inheritance chain and any patches in the global reconcile config.
///
/// Precedence (most specific wins):
///   Named Module patch > Kind-wide Module patch > Named Profile patch >
///   Kind-wide Profile patch > Global reconcile settings
///
/// `profile_chain` is ancestors-first, leaf-last (e.g., `["base", "work"]`).
/// Within each level, patches apply in list order (last wins for duplicates).
pub fn resolve_effective_reconcile(
    module_name: &str,
    profile_chain: &[&str],
    reconcile: &config::ReconcileConfig,
) -> EffectiveReconcile {
    let mut effective = EffectiveReconcile {
        interval: reconcile.interval.clone(),
        auto_apply: reconcile.auto_apply,
        drift_policy: reconcile.drift_policy.clone(),
    };

    // 1. Kind-wide Profile patch (no name = applies to all profiles)
    if let Some(patch) = reconcile
        .patches
        .iter()
        .rev()
        .find(|p| p.kind == config::ReconcilePatchKind::Profile && p.name.is_none())
    {
        overlay_reconcile_patch(&mut effective, patch);
    }

    // 2. Named Profile patches in inheritance order (leaf last = leaf wins)
    for profile_name in profile_chain {
        if let Some(patch) = reconcile.patches.iter().rev().find(|p| {
            p.kind == config::ReconcilePatchKind::Profile && p.name.as_deref() == Some(profile_name)
        }) {
            overlay_reconcile_patch(&mut effective, patch);
        }
    }

    // 3. Kind-wide Module patch (no name = applies to all modules)
    if let Some(patch) = reconcile
        .patches
        .iter()
        .rev()
        .find(|p| p.kind == config::ReconcilePatchKind::Module && p.name.is_none())
    {
        overlay_reconcile_patch(&mut effective, patch);
    }

    // 4. Named Module patch (highest priority) — last matching entry wins
    if let Some(patch) = reconcile.patches.iter().rev().find(|p| {
        p.kind == config::ReconcilePatchKind::Module && p.name.as_deref() == Some(module_name)
    }) {
        overlay_reconcile_patch(&mut effective, patch);
    }

    effective
}

/// Overlay a patch's `Some` fields onto an effective reconcile struct.
fn overlay_reconcile_patch(base: &mut EffectiveReconcile, patch: &config::ReconcilePatch) {
    if let Some(ref interval) = patch.interval {
        base.interval = interval.clone();
    }
    if let Some(auto_apply) = patch.auto_apply {
        base.auto_apply = auto_apply;
    }
    if let Some(ref dp) = patch.drift_policy {
        base.drift_policy = dp.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_reconcile_config(patches: Vec<config::ReconcilePatch>) -> config::ReconcileConfig {
        config::ReconcileConfig {
            interval: "5m".into(),
            on_change: false,
            auto_apply: false,
            policy: None,
            drift_policy: config::DriftPolicy::NotifyOnly,
            patches,
        }
    }

    #[test]
    fn no_patches_returns_global_defaults() {
        let rc = make_reconcile_config(vec![]);
        let eff = resolve_effective_reconcile("docker", &["base"], &rc);
        assert_eq!(eff.interval, "5m");
        assert!(!eff.auto_apply);
        assert_eq!(eff.drift_policy, config::DriftPolicy::NotifyOnly);
    }

    #[test]
    fn named_module_patch_overrides_global() {
        let rc = make_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("docker".into()),
            interval: Some("1m".into()),
            auto_apply: Some(true),
            drift_policy: Some(config::DriftPolicy::Auto),
        }]);
        let eff = resolve_effective_reconcile("docker", &["base"], &rc);
        assert_eq!(eff.interval, "1m");
        assert!(eff.auto_apply);
        assert_eq!(eff.drift_policy, config::DriftPolicy::Auto);
    }

    #[test]
    fn named_module_patch_does_not_affect_other_modules() {
        let rc = make_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("docker".into()),
            interval: Some("1m".into()),
            auto_apply: None,
            drift_policy: None,
        }]);
        let eff = resolve_effective_reconcile("kubernetes", &["base"], &rc);
        assert_eq!(eff.interval, "5m");
    }

    #[test]
    fn kind_wide_module_patch_applies_to_all() {
        let rc = make_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: None,
            interval: Some("2m".into()),
            auto_apply: None,
            drift_policy: None,
        }]);
        let eff = resolve_effective_reconcile("anything", &["base"], &rc);
        assert_eq!(eff.interval, "2m");
    }

    #[test]
    fn named_profile_patch_applies_when_in_chain() {
        let rc = make_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: Some("work".into()),
            interval: Some("10m".into()),
            auto_apply: Some(true),
            drift_policy: None,
        }]);
        let eff = resolve_effective_reconcile("docker", &["base", "work"], &rc);
        assert_eq!(eff.interval, "10m");
        assert!(eff.auto_apply);
    }

    #[test]
    fn named_module_beats_named_profile() {
        let rc = make_reconcile_config(vec![
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Profile,
                name: Some("work".into()),
                interval: Some("10m".into()),
                auto_apply: None,
                drift_policy: None,
            },
            config::ReconcilePatch {
                kind: config::ReconcilePatchKind::Module,
                name: Some("docker".into()),
                interval: Some("30s".into()),
                auto_apply: None,
                drift_policy: None,
            },
        ]);
        let eff = resolve_effective_reconcile("docker", &["base", "work"], &rc);
        assert_eq!(eff.interval, "30s");
    }

    #[test]
    fn kind_wide_profile_patch_applies() {
        let rc = make_reconcile_config(vec![config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: None,
            interval: None,
            auto_apply: Some(true),
            drift_policy: Some(config::DriftPolicy::Auto),
        }]);
        let eff = resolve_effective_reconcile("docker", &["base"], &rc);
        assert!(eff.auto_apply);
        assert_eq!(eff.drift_policy, config::DriftPolicy::Auto);
    }
}
