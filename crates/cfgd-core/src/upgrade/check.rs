//! Policy-driven self-update check.
//!
//! Splits the *decision* (pure, testable without I/O) from the *effects*
//! (network fetch + apply, injected via closures). [`run_update_check`]
//! threads an [`UpdateConfig`]'s policy, interval, and channel through the
//! existing [`check_latest`](super::check_latest) /
//! [`download_and_install`](super::download_and_install) paths and reports
//! exactly what happened in an [`UpdateCheckOutcome`].
//!
//! The dedup / skill-cascade that consumes an available update lives in a
//! later layer — this module is strictly the binary self-update check.

use std::time::Duration;

use crate::config::{UpdateConfig, UpdatePolicy};

use super::UpdateCheck;

/// What [`resolve_action`] decided to do with an *available* update, given the
/// policy and whether the session can prompt a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Drive the apply path without prompting (`Auto`, or `Prompt` confirmed).
    Apply,
    /// Surface that an update is available, but neither prompt nor apply
    /// (`Notify`, or `Prompt` degraded because the session is non-interactive).
    Surface,
    /// Take no action at all (`Manual`).
    Skip,
}

/// Outcome of [`run_update_check`].
///
/// * `checked` — whether a fresh version check was actually performed (false
///   when `Manual`, or when still inside the configured interval).
/// * `surfaced` — whether an available update was reported to the user.
/// * `applied` — whether the update was installed via the apply path.
/// * `update` — the [`UpdateCheck`] when a check ran; `None` otherwise. Its
///   `update_available` flag is the input the dedup/cascade layer consumes.
#[derive(Debug, Clone)]
pub struct UpdateCheckOutcome {
    pub checked: bool,
    pub surfaced: bool,
    pub applied: bool,
    pub update: Option<UpdateCheck>,
}

impl UpdateCheckOutcome {
    /// An outcome where no check ran (interval-gated or `Manual`).
    fn no_check() -> Self {
        Self {
            checked: false,
            surfaced: false,
            applied: false,
            update: None,
        }
    }
}

/// Parse an [`UpdateConfig::interval`] string into a [`Duration`], falling back
/// to the built-in 24h cadence when the string is malformed (a bad interval
/// must never wedge the check open every invocation, nor crash a normal
/// command — it degrades to the safe default and is surfaced via tracing).
pub fn resolved_interval(config: &UpdateConfig) -> Duration {
    match crate::parse_duration_str(&config.interval) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                interval = %config.interval,
                error = %e,
                "invalid update interval; falling back to 24h",
            );
            super::version_check_interval()
        }
    }
}

/// Pure interval/`Manual` gate: should a fresh network check run *now*?
///
/// * `Manual` → never (`false`), short-circuiting before any work.
/// * `last_checked == None` → yes (no prior check on record).
/// * otherwise → yes only once `now - last_checked >= interval`.
///
/// `now` and `last_checked` are Unix epoch seconds. `now < last_checked` (a
/// clock that moved backwards) is treated as "within interval" via saturating
/// subtraction, so a backwards clock suppresses rather than forces a check.
pub fn should_check(
    policy: UpdatePolicy,
    interval: Duration,
    now: u64,
    last_checked: Option<u64>,
) -> bool {
    if policy == UpdatePolicy::Manual {
        return false;
    }
    match last_checked {
        None => true,
        Some(last) => now.saturating_sub(last) >= interval.as_secs(),
    }
}

/// Pure policy → action mapping for an *available* update.
///
/// `Prompt` degrades to [`UpdateAction::Surface`] whenever the session cannot
/// prompt a human (`interactive == false`) or the user pre-answered yes-to-all
/// (`assume_yes == true` means "don't prompt" — but a startup check must not
/// silently auto-apply on a bare `--yes`, so it degrades to surface rather than
/// apply). `Auto` always applies; `Manual` always skips.
pub fn resolve_action(policy: UpdatePolicy, interactive: bool, assume_yes: bool) -> UpdateAction {
    match policy {
        UpdatePolicy::Auto => UpdateAction::Apply,
        UpdatePolicy::Manual => UpdateAction::Skip,
        UpdatePolicy::Notify => UpdateAction::Surface,
        UpdatePolicy::Prompt => {
            if interactive && !assume_yes {
                UpdateAction::Apply
            } else {
                UpdateAction::Surface
            }
        }
    }
}

/// Network-fetch effect: resolve the latest [`UpdateCheck`] for a release
/// channel (`None` = default stream).
pub type FetchFn<'a> =
    Box<dyn FnMut(Option<&str>) -> Result<UpdateCheck, super::UpgradeError> + 'a>;
/// Confirm/apply effect: a `&UpdateCheck` predicate returning yes/no (prompt
/// answer, or install success).
pub type CheckPredicateFn<'a> = Box<dyn FnMut(&UpdateCheck) -> bool + 'a>;
/// Surface effect: report an available `&UpdateCheck` without applying.
pub type SurfaceFn<'a> = Box<dyn FnMut(&UpdateCheck) + 'a>;
/// Persist effect: record the just-checked Unix-seconds timestamp.
pub type RecordCheckedFn<'a> = Box<dyn FnMut(u64) + 'a>;

/// Effects the orchestrator needs but the decision core must not own, injected
/// so [`run_update_check`] is drivable in tests with no network or filesystem.
pub struct UpdateCheckEffects<'a> {
    /// Whether the session can interactively prompt a human (TTY + not `--yes`).
    pub interactive: bool,
    /// Whether the user supplied `--yes` / `CFGD_YES` (suppresses the prompt).
    pub assume_yes: bool,
    /// Perform the network check for the given release channel.
    pub fetch: FetchFn<'a>,
    /// Confirm with the user before applying (only called for interactive
    /// `Prompt`). Returns the user's yes/no answer.
    pub confirm: CheckPredicateFn<'a>,
    /// Surface an available update without applying (`Notify` / degraded
    /// `Prompt`).
    pub surface: SurfaceFn<'a>,
    /// Drive the apply path for an available update; returns whether the
    /// install succeeded.
    pub apply: CheckPredicateFn<'a>,
    /// Persist the just-checked timestamp (Unix epoch seconds) so the next
    /// invocation interval-gates against it.
    pub record_checked: RecordCheckedFn<'a>,
}

/// Run a policy-driven self-update check.
///
/// 1. Interval/`Manual`-gate against `last_checked` *first* — within-interval
///    (or `Manual`) returns immediately with `checked == false` and no network
///    call, keeping the common CLI startup path cheap.
/// 2. Otherwise fetch the latest release for the configured channel and persist
///    the check timestamp.
/// 3. If an update is available, dispatch per [`resolve_action`]: apply
///    (`Auto` / confirmed `Prompt`), surface-only (`Notify` / degraded
///    `Prompt`), or skip.
///
/// A fetch error is non-fatal: `checked` stays `true` (the attempt was made and
/// the timestamp recorded so we don't hammer the API), with no surface or apply.
pub fn run_update_check(
    config: &UpdateConfig,
    now: u64,
    last_checked: Option<u64>,
    effects: &mut UpdateCheckEffects<'_>,
) -> UpdateCheckOutcome {
    let interval = resolved_interval(config);
    if !should_check(config.policy, interval, now, last_checked) {
        return UpdateCheckOutcome::no_check();
    }

    let update = match (effects.fetch)(config.channel.as_deref()) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(error = %e, "self-update check failed");
            (effects.record_checked)(now);
            return UpdateCheckOutcome {
                checked: true,
                surfaced: false,
                applied: false,
                update: None,
            };
        }
    };
    (effects.record_checked)(now);

    if !update.update_available {
        return UpdateCheckOutcome {
            checked: true,
            surfaced: false,
            applied: false,
            update: Some(update),
        };
    }

    let mut surfaced = false;
    let mut applied = false;
    match resolve_action(config.policy, effects.interactive, effects.assume_yes) {
        UpdateAction::Apply => {
            // Interactive `Prompt` confirms first; declining degrades to a
            // surface so the user still learns an update exists.
            let proceed = if config.policy == UpdatePolicy::Prompt {
                (effects.confirm)(&update)
            } else {
                true
            };
            if proceed {
                applied = (effects.apply)(&update);
                if !applied {
                    (effects.surface)(&update);
                    surfaced = true;
                }
            } else {
                (effects.surface)(&update);
                surfaced = true;
            }
        }
        UpdateAction::Surface => {
            (effects.surface)(&update);
            surfaced = true;
        }
        UpdateAction::Skip => {}
    }

    UpdateCheckOutcome {
        checked: true,
        surfaced,
        applied,
        update: Some(update),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SkillUpdateConfig;
    use semver::Version;

    const HOUR: u64 = 3600;

    fn config(policy: UpdatePolicy) -> UpdateConfig {
        UpdateConfig {
            policy,
            interval: "24h".to_string(),
            channel: None,
            skills: SkillUpdateConfig::default(),
        }
    }

    /// An [`UpdateCheck`] whose `update_available` flag is `available`.
    fn check(available: bool) -> UpdateCheck {
        let current = Version::new(1, 0, 0);
        let latest = if available {
            Version::new(2, 0, 0)
        } else {
            current.clone()
        };
        UpdateCheck {
            current,
            latest,
            update_available: available,
            release: None,
        }
    }

    /// Effects wired to canned closures with shared counters so a test can both
    /// drive the orchestrator and assert which closures fired.
    struct Spy {
        fetched_channel: std::cell::RefCell<Option<Option<String>>>,
        surfaced: std::cell::Cell<u32>,
        applied: std::cell::Cell<u32>,
        confirmed: std::cell::Cell<u32>,
        recorded: std::cell::Cell<Option<u64>>,
    }

    impl Spy {
        fn new() -> Self {
            Self {
                fetched_channel: std::cell::RefCell::new(None),
                surfaced: std::cell::Cell::new(0),
                applied: std::cell::Cell::new(0),
                confirmed: std::cell::Cell::new(0),
                recorded: std::cell::Cell::new(None),
            }
        }

        fn effects<'a>(
            &'a self,
            interactive: bool,
            assume_yes: bool,
            result: UpdateCheck,
            fetch_ok: bool,
            confirm: bool,
            apply_ok: bool,
        ) -> UpdateCheckEffects<'a> {
            UpdateCheckEffects {
                interactive,
                assume_yes,
                fetch: Box::new(move |ch| {
                    *self.fetched_channel.borrow_mut() = Some(ch.map(str::to_string));
                    if fetch_ok {
                        Ok(result.clone())
                    } else {
                        Err(super::super::UpgradeError::ApiError {
                            message: "boom".into(),
                        })
                    }
                }),
                confirm: Box::new(move |_| {
                    self.confirmed.set(self.confirmed.get() + 1);
                    confirm
                }),
                surface: Box::new(move |_| self.surfaced.set(self.surfaced.get() + 1)),
                apply: Box::new(move |_| {
                    self.applied.set(self.applied.get() + 1);
                    apply_ok
                }),
                record_checked: Box::new(move |t| self.recorded.set(Some(t))),
            }
        }
    }

    // ----- pure gate -----

    #[test]
    fn manual_policy_never_checks() {
        assert!(!should_check(
            UpdatePolicy::Manual,
            Duration::from_secs(0),
            100,
            None
        ));
    }

    #[test]
    fn no_last_checked_always_checks() {
        assert!(should_check(
            UpdatePolicy::Prompt,
            Duration::from_secs(HOUR),
            100,
            None
        ));
    }

    #[test]
    fn within_interval_does_not_check() {
        // last check 1h ago, interval 24h → suppressed.
        assert!(!should_check(
            UpdatePolicy::Notify,
            Duration::from_secs(24 * HOUR),
            10 * HOUR,
            Some(9 * HOUR),
        ));
    }

    #[test]
    fn past_interval_checks() {
        assert!(should_check(
            UpdatePolicy::Notify,
            Duration::from_secs(HOUR),
            10 * HOUR,
            Some(8 * HOUR),
        ));
    }

    #[test]
    fn backwards_clock_suppresses_rather_than_forces() {
        // now < last_checked: saturating_sub → 0 < interval → no check.
        assert!(!should_check(
            UpdatePolicy::Auto,
            Duration::from_secs(HOUR),
            5 * HOUR,
            Some(8 * HOUR),
        ));
    }

    #[test]
    fn resolved_interval_parses_valid_string() {
        let mut cfg = config(UpdatePolicy::Notify);
        cfg.interval = "1h".to_string();
        assert_eq!(
            resolved_interval(&cfg),
            Duration::from_secs(HOUR),
            "a well-formed interval must parse to its duration"
        );
    }

    #[test]
    fn resolved_interval_falls_back_to_24h_on_garbage() {
        let mut cfg = config(UpdatePolicy::Notify);
        cfg.interval = "not-a-duration".to_string();
        assert_eq!(
            resolved_interval(&cfg),
            Duration::from_secs(24 * HOUR),
            "a malformed interval must degrade to the 24h default, never crash"
        );
    }

    #[test]
    fn resolve_action_maps_each_policy() {
        assert_eq!(
            resolve_action(UpdatePolicy::Auto, true, false),
            UpdateAction::Apply
        );
        assert_eq!(
            resolve_action(UpdatePolicy::Manual, true, false),
            UpdateAction::Skip
        );
        assert_eq!(
            resolve_action(UpdatePolicy::Notify, true, false),
            UpdateAction::Surface
        );
        assert_eq!(
            resolve_action(UpdatePolicy::Prompt, true, false),
            UpdateAction::Apply
        );
        // Non-interactive Prompt degrades to Surface.
        assert_eq!(
            resolve_action(UpdatePolicy::Prompt, false, false),
            UpdateAction::Surface
        );
        // --yes degrades Prompt to Surface (no silent auto-apply).
        assert_eq!(
            resolve_action(UpdatePolicy::Prompt, true, true),
            UpdateAction::Surface
        );
    }

    // ----- orchestration -----

    #[test]
    fn manual_policy_skips_check_entirely() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), true, true, true);
        let outcome = run_update_check(&config(UpdatePolicy::Manual), 100, None, &mut effects);
        assert!(!outcome.checked);
        assert!(!outcome.surfaced);
        assert!(!outcome.applied);
        assert!(outcome.update.is_none());
        assert!(
            spy.fetched_channel.borrow().is_none(),
            "Manual must not fetch"
        );
        assert!(
            spy.recorded.get().is_none(),
            "Manual must not record a check"
        );
    }

    #[test]
    fn notify_records_available_without_applying() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), true, false, true);
        let outcome = run_update_check(&config(UpdatePolicy::Notify), 100, None, &mut effects);
        assert!(outcome.checked);
        assert!(outcome.surfaced);
        assert!(!outcome.applied);
        assert!(outcome.update.as_ref().is_some_and(|u| u.update_available));
        assert_eq!(spy.surfaced.get(), 1);
        assert_eq!(spy.applied.get(), 0);
        assert_eq!(spy.recorded.get(), Some(100));
    }

    #[test]
    fn auto_applies_without_prompting() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), true, false, true);
        let outcome = run_update_check(&config(UpdatePolicy::Auto), 100, None, &mut effects);
        assert!(outcome.checked && outcome.applied && !outcome.surfaced);
        assert_eq!(spy.applied.get(), 1);
        assert_eq!(spy.confirmed.get(), 0, "Auto must not prompt");
    }

    #[test]
    fn prompt_interactive_confirms_then_applies() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), true, true, true);
        let outcome = run_update_check(&config(UpdatePolicy::Prompt), 100, None, &mut effects);
        assert!(outcome.applied && !outcome.surfaced);
        assert_eq!(spy.confirmed.get(), 1);
        assert_eq!(spy.applied.get(), 1);
    }

    #[test]
    fn prompt_declined_degrades_to_surface() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), true, false, true);
        let outcome = run_update_check(&config(UpdatePolicy::Prompt), 100, None, &mut effects);
        assert!(!outcome.applied && outcome.surfaced);
        assert_eq!(spy.confirmed.get(), 1);
        assert_eq!(spy.surfaced.get(), 1);
        assert_eq!(spy.applied.get(), 0);
    }

    #[test]
    fn prompt_non_interactive_degrades_to_notify() {
        let spy = Spy::new();
        let mut effects = spy.effects(false, false, check(true), true, true, true);
        let outcome = run_update_check(&config(UpdatePolicy::Prompt), 100, None, &mut effects);
        assert!(!outcome.applied && outcome.surfaced);
        assert_eq!(spy.confirmed.get(), 0, "non-TTY must not block on a prompt");
        assert_eq!(spy.surfaced.get(), 1);
    }

    #[test]
    fn no_update_available_records_but_does_not_surface() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(false), true, false, true);
        let outcome = run_update_check(&config(UpdatePolicy::Auto), 100, None, &mut effects);
        assert!(outcome.checked && !outcome.surfaced && !outcome.applied);
        assert!(outcome.update.as_ref().is_some_and(|u| !u.update_available));
        assert_eq!(spy.recorded.get(), Some(100));
    }

    #[test]
    fn within_interval_short_circuits_before_fetch() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), true, true, true);
        // last check 1h ago vs default 24h interval.
        let outcome = run_update_check(
            &config(UpdatePolicy::Auto),
            10 * HOUR,
            Some(9 * HOUR),
            &mut effects,
        );
        assert!(!outcome.checked);
        assert!(spy.fetched_channel.borrow().is_none());
    }

    #[test]
    fn fetch_error_is_non_fatal_and_records_check() {
        let spy = Spy::new();
        let mut effects = spy.effects(true, false, check(true), false, true, true);
        let outcome = run_update_check(&config(UpdatePolicy::Auto), 100, None, &mut effects);
        assert!(outcome.checked);
        assert!(!outcome.surfaced && !outcome.applied);
        assert!(outcome.update.is_none());
        assert_eq!(
            spy.recorded.get(),
            Some(100),
            "a failed attempt still records the timestamp"
        );
    }

    #[test]
    fn channel_is_threaded_to_fetch() {
        let spy = Spy::new();
        let mut cfg = config(UpdatePolicy::Notify);
        cfg.channel = Some("beta".to_string());
        let mut effects = spy.effects(true, false, check(true), true, false, true);
        run_update_check(&cfg, 100, None, &mut effects);
        assert_eq!(
            spy.fetched_channel.borrow().clone(),
            Some(Some("beta".to_string())),
            "configured channel must reach the fetch closure",
        );
    }

    #[test]
    fn apply_failure_degrades_to_surface() {
        let spy = Spy::new();
        // Auto policy, apply returns false (install failed).
        let mut effects = spy.effects(true, false, check(true), true, false, false);
        let outcome = run_update_check(&config(UpdatePolicy::Auto), 100, None, &mut effects);
        assert!(
            !outcome.applied && outcome.surfaced,
            "failed apply should surface"
        );
        assert_eq!(spy.applied.get(), 1);
        assert_eq!(spy.surfaced.get(), 1);
    }
}
