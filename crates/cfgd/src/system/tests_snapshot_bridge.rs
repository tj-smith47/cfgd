//! The shared capture behind every per-configurator snapshot bridge.
//!
//! A configurator's narration is not free-floating output: the reconciler
//! settles ONE action line per `apply` call and renders the narration attached
//! under it, inside the phase → owner → action tree. A bridge that drove `apply`
//! through a discarded sink would pin the shape a configurator emits when
//! nobody is listening — which no production path produces — so the goldens
//! would keep passing while the shape users actually see drifted.
//!
//! So the capture is assembled from the REAL derivations: the action subject
//! from [`action_display_subject`], the notes it produced from
//! [`render_caveats`] — the run-wide `Caveats` section production renders
//! after the closing summary, not attached under the action line. Nothing
//! here formats a status line of its own, and a configurator that reached the
//! printer directly would show up in the golden above its own action line
//! rather than under it.

use cfgd_core::output::test_capture::strip_ansi;
use cfgd_core::output::{Doc, OwnerLabel, Printer, Role};
use cfgd_core::providers::{NoteSink, SystemConfigurator, SystemContext};
use cfgd_core::reconciler::{
    Action, Owner, PhaseName, SystemAction, action_display_subject, render_caveats,
};

/// One configurator apply, described the way the plan would describe it.
///
/// `key`/`current`/`target` are fixture data in the same sense `desired` is:
/// they name a drift THIS configurator's own `diff` produces for this fixture,
/// spelled exactly as `diff` spells it. They are not read back from `diff` at
/// run time on purpose — several configurators probe the host (systemd asks
/// `systemctl is-enabled`), so a golden built from a live diff would say
/// something different on a machine without that daemon.
pub(crate) struct BridgeApply<'a> {
    pub configurator: &'a dyn SystemConfigurator,
    pub desired: &'a serde_yaml::Value,
    /// The `SystemDrift::key` this apply settles.
    pub key: &'a str,
    /// The before/after the action line renders as `current → target`.
    pub current: &'a str,
    pub target: &'a str,
    /// The command's own closing summary, emitted after the phase closes — the
    /// seam the bridge exists to check.
    pub summary_role: Role,
    pub summary: &'a str,
}

/// Apply under a collecting sink and return the human render, ANSI stripped.
///
/// The caller still normalizes host-specific paths and asserts the seam; only
/// the production-shaped assembly is shared.
pub(crate) fn capture_attached_apply<D: serde::Serialize>(
    apply: &BridgeApply<'_>,
    data: &D,
) -> String {
    let notes = NoteSink::default();
    let (printer, cap) = Printer::for_test_doc();
    let owner = Owner::profile("snapshot");

    {
        let phase = printer.section_phase(&PhaseName::System.section_label());
        let owner_section = phase.section_owner(&OwnerLabel::new("profile", "snapshot"));

        apply
            .configurator
            .apply(apply.desired, &SystemContext::with_notes(&printer, &notes))
            .expect("a bridge fixture applies cleanly; its warnings travel as notes");

        let action = Action::System(SystemAction::SetValue {
            configurator: apply.configurator.name().to_string(),
            key: apply.key.to_string(),
            desired: apply.target.to_string(),
            current: apply.current.to_string(),
            origin: String::new(),
        });
        drop(owner_section.action_status(Role::Ok, action_display_subject(&action).to_string()));
    }

    printer.emit(
        Doc::new()
            .status(apply.summary_role, apply.summary)
            .with_data(data),
    );
    render_caveats(&printer, &[(owner, notes.take())]);
    drop(printer);

    strip_ansi(&cap.human())
}

/// Assert the one blank line separating the phase tree from the closing
/// summary — the invariant every bridge in this crate shares.
pub(crate) fn assert_single_seam(name: &str, captured: &str) {
    assert!(
        captured.contains("\n\n"),
        "{name} missing blank line at seam:\n{captured}"
    );
    assert!(
        !captured.contains("\n\n\n"),
        "{name} has duplicate blank line:\n{captured}"
    );
}
