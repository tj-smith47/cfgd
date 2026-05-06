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
