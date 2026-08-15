//! The concurrent phase's tree, drawn while it happens instead of after it.
//!
//! A phase that dispatches its actions across lanes finishes them in an order
//! nobody chose: the lane that drains first is the one whose package manager
//! was quickest, not the one the plan listed first. The tree it renders must
//! not inherit that order — a reader watching three provisions start and then
//! seeing the second one's line appear above the first's has been told
//! something untrue about what ran when.
//!
//! So the live region IS the tree, and it is an ordered list of rows:
//!
//! ```text
//!   cfgd:managers                                   ← a group's heading
//!     ✓ refresh apt index                    (9.5s) ← settled, in place
//!     ⠹ provision brew via homebrew installer       ← running
//!         ==> Downloading and installing Homebrew…  ← its output window
//!     ○ provision pipx via apt · waiting on apt     ← blocked, in its own row
//! ```
//!
//! Four rules hold it together, and every one of them exists because the
//! alternative lies about the run:
//!
//! 1. **A row never moves.** A row is appended the first time the scheduler has
//!    something to say about its action — dispatching it, or reporting what is
//!    holding it — and from then on it changes STATE in place: pending →
//!    running (with its child's output beneath it) → settled. An action that
//!    finishes early rewrites its own row rather than printing a new line
//!    somewhere else.
//! 2. **Rows leave from the head only.** A settled row is committed to the
//!    permanent scrollback only once every row above it has been, because
//!    committing it any earlier is exactly what would make it jump: the
//!    renderer writes above the whole live region. So the scrollback reads in
//!    the same order the screen did.
//! 3. **A group's heading is written once.** Scrollback is append-only and the
//!    renderer's sections unwind in order, so a group whose rows were committed
//!    and then gained another row could only re-introduce itself by printing
//!    its own heading twice. A group therefore keeps its guard open until it
//!    can gain no more rows (`sealed`), and the next group's rows wait behind
//!    it rather than being misfiled under it.
//! 4. **The region never outgrows the terminal.** indicatif draws top down and
//!    stops at the last line, so an over-full region drops the rows at its
//!    FOOT — the work in flight. When the head row is slow, every row behind it
//!    piles up, so the region pays for itself out of the rows with nothing left
//!    to say: a settled row gives up its LINE while keeping its outcome, and a
//!    single `… N settled rows held for commit` line at the top accounts for
//!    every one of them. A running row's line is never taken.
//!
//! Off a TTY nothing here draws: rows are never built, the tree is inert, and
//! the phase's outcomes are written by `apply::emit_phase_tree` in PLAN order
//! at phase close — the shape every apply golden pins.
use crate::output::lane::LaneHandle;
use crate::output::live_row::{LiveRow, RowStatus};
use crate::output::{OwnerLabel, Printer, Role, SectionGuard};

use super::apply::{ActionOutcome, emit_action_line};
use super::format::action_display_subject;
use super::types::{Action, Owner};

/// One line the scheduler wants on screen for work it has not started.
pub(super) struct Wait<'p> {
    pub(super) owner: &'p Owner,
    /// The blocked action, or `None` for a line standing for the whole group —
    /// the tier barrier holds every action a group has, and says so once.
    pub(super) action: Option<&'p Action>,
    /// The whole sentence, composed by the scheduler that knows what is in the
    /// way.
    pub(super) subject: String,
}

/// What the scheduler is holding back, and which groups can still grow.
pub(super) struct Held<'p> {
    pub(super) waits: Vec<Wait<'p>>,
    /// Owner tokens with at least one action not yet dispatched. A group absent
    /// from this set can gain no more rows, which is what lets its heading be
    /// closed and the next group's begin.
    pub(super) pending_owners: Vec<String>,
}

enum RowState {
    /// Planned and not started — the scheduler is holding it, and the row says
    /// what for.
    Pending,
    Running,
    Settled(ActionOutcome),
    /// Committed to the scrollback; the row is gone from the live region.
    Flushed,
}

struct Row<'p> {
    /// `None` for a group's own tier-wait row, which stands for every action of
    /// the group rather than for one of them.
    action: Option<&'p Action>,
    /// The live-region line, absent off a TTY and for a pending row the region
    /// had no room to draw.
    line: Option<LiveRow<'p>>,
    state: RowState,
    /// This row has settled and the region gave its LINE up for the work still
    /// running; the elision row counts it until it commits.
    elided: bool,
}

struct Group<'p> {
    owner: &'p Owner,
    label: OwnerLabel,
    /// The heading's own row. `None` for the group whose heading the caller
    /// committed before the dispatch began (a phase with one lane owner), and
    /// once the heading has been written permanently.
    heading: Option<LiveRow<'p>>,
    rows: Vec<Row<'p>>,
    /// How many of `rows` have been committed: the prefix that has left the
    /// live region.
    flushed: usize,
    /// No further action row can join this group.
    sealed: bool,
}

impl Group<'_> {
    fn drained(&self) -> bool {
        self.flushed == self.rows.len()
    }
}

/// The group whose heading is open, and the guard writing under it.
struct OpenGroup<'g> {
    index: usize,
    /// `None` when this is the group the caller preopened, whose guard it owns.
    guard: Option<SectionGuard<'g>>,
}

pub(super) struct PhaseTree<'p, 'g> {
    printer: &'p Printer,
    /// The phase's own section, parent of every group heading opened here.
    section: Option<&'g SectionGuard<'g>>,
    /// The one group whose heading the caller already committed, with the guard
    /// it opened for it. A phase with a single lane owner names it before
    /// anything paints, because the live region draws below the last committed
    /// line and a heading held back would land under the rows it introduces.
    preopened: Option<(&'p Owner, &'g SectionGuard<'g>)>,
    /// Depth an action row renders at; a heading sits one level out.
    depth: usize,
    /// The phase's alignment column.
    width: usize,
    live: bool,
    groups: Vec<Group<'p>>,
    /// How many groups have been committed and closed.
    flushed: usize,
    open: Option<OpenGroup<'g>>,
    /// Lines the region may hold before the terminal starts dropping the ones
    /// at its foot.
    budget: usize,
    /// The region's own summary line, standing in for the settled rows whose
    /// lines it gave up. Drawn at the TOP, which is the one position an
    /// over-full region never truncates.
    elision: Option<LiveRow<'p>>,
    /// Settled rows currently holding no line.
    elided: usize,
}

impl<'p, 'g> PhaseTree<'p, 'g> {
    pub(super) fn new(
        printer: &'p Printer,
        section: Option<&'g SectionGuard<'g>>,
        preopened: Option<(&'p Owner, &'g SectionGuard<'g>)>,
        depth: usize,
        width: usize,
    ) -> Self {
        Self {
            printer,
            section,
            preopened,
            depth,
            width,
            live: printer.live_bars() && section.is_some(),
            groups: Vec::new(),
            flushed: 0,
            open: None,
            budget: printer.live_row_budget(),
            elision: None,
            elided: 0,
        }
    }

    /// Pin the region's line budget, for a test that must drive the tree past
    /// a terminal height it cannot set.
    #[cfg(test)]
    pub(super) fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget;
        self
    }

    /// Whether outcomes settle HERE, in the rows the region already shows.
    ///
    /// The negative answer is the whole off-TTY contract: the caller holds each
    /// outcome instead and `emit_phase_tree` writes the grouped tree in plan
    /// order at phase close.
    pub(super) fn is_live(&self) -> bool {
        self.live
    }

    /// Start `action`'s row and hand back the lane view its worker writes into.
    ///
    /// Reuses the row the action already has whenever it was blocked first:
    /// that row is where the reader has been watching it, and starting it
    /// somewhere else is the jump this whole module exists to prevent.
    pub(super) fn dispatched(&mut self, owner: &'p Owner, action: &'p Action) -> LaneHandle<'p> {
        let subject = action_display_subject(action).to_string();
        if !self.live {
            return self.printer.lane_at(self.depth, subject);
        }
        let (gi, ri) = self.row_for(owner, Some(action));
        self.groups[gi].rows[ri].state = RowState::Running;
        if self.groups[gi].rows[ri].line.is_none() {
            // The region had no room for this row while it was pending, so it
            // is drawn now: work that is running is never the row the budget
            // gives up.
            self.groups[gi].rows[ri].line = Some(self.draw_row(gi, ri));
        }
        // The new line may have pushed the region past the terminal's height;
        // paying for it out of rows that have nothing left to say keeps the
        // running frontier — this row included — on screen.
        self.enforce_budget();
        match &self.groups[gi].rows[ri].line {
            Some(line) => self.printer.lane_on_row(line, subject),
            // Unreachable: the row was just drawn.
            None => self.printer.lane_at(self.depth, subject),
        }
    }

    /// Bring the rows for work that has not started in step with the scheduler.
    pub(super) fn waiting(&mut self, held: &Held<'p>) {
        if !self.live {
            return;
        }
        for wait in &held.waits {
            match wait.action {
                Some(action) => {
                    let (gi, ri) = self.row_for(wait.owner, Some(action));
                    self.paint_pending(gi, ri, &wait.subject);
                }
                None => {
                    let (gi, ri) = self.row_for(wait.owner, None);
                    self.paint_pending(gi, ri, &wait.subject);
                }
            }
        }
        // A group-wide wait line is the one row that is retired rather than
        // rewritten: it stands for a barrier, and a barrier that has lifted is
        // not a state the group is in any more. An action's own row stays put —
        // it is where that action will start.
        let wanted: Vec<&Owner> = held
            .waits
            .iter()
            .filter(|w| w.action.is_none())
            .map(|w| w.owner)
            .collect();
        for group in &mut self.groups {
            let keep = wanted.contains(&group.owner);
            if !keep {
                group.rows.retain_mut(|row| {
                    if row.action.is_some() || !matches!(row.state, RowState::Pending) {
                        return true;
                    }
                    if let Some(line) = row.line.take() {
                        line.retire();
                    }
                    false
                });
            }
            group.sealed = !held
                .pending_owners
                .iter()
                .any(|token| *token == group.owner.token());
        }
        self.commit_from_head();
        self.enforce_budget();
    }

    /// Settle `action`'s row in place, and commit whatever that unblocks.
    pub(super) fn settled(&mut self, owner: &'p Owner, action: &'p Action, outcome: ActionOutcome) {
        let (gi, ri) = self.row_for(owner, Some(action));
        if let Some(line) = &self.groups[gi].rows[ri].line {
            line.set_action_status(
                &RowStatus {
                    role: outcome.role,
                    subject: &outcome.subject,
                    detail: outcome.detail.as_deref(),
                    detail_muted: outcome.detail_muted,
                    duration: outcome.duration,
                },
                self.width,
            );
        }
        self.groups[gi].rows[ri].state = RowState::Settled(outcome);
        self.commit_from_head();
        self.enforce_budget();
    }

    /// Commit everything the tree still holds and take the live region down.
    ///
    /// Every group is sealed by now — the dispatch is over — so nothing is left
    /// waiting on a group that might still grow. Rows that never settled
    /// describe work that never began; they leave no line at all, which is the
    /// same account an aborted run's shortfall gets from the rollup.
    pub(super) fn finish(mut self) {
        if !self.live {
            return;
        }
        for group in &mut self.groups {
            group.sealed = true;
            group.rows.retain_mut(|row| {
                if matches!(row.state, RowState::Settled(_) | RowState::Flushed) {
                    return true;
                }
                if let Some(line) = row.line.take() {
                    line.retire();
                }
                false
            });
        }
        self.commit_from_head();
        debug_assert!(
            self.flushed == self.groups.len(),
            "a settled row outlived the tree that was to commit it"
        );
        // A group that committed nothing never opened its heading, so nothing
        // has retired that heading's live row; and the summary line has by now
        // nothing left to summarise.
        for group in &mut self.groups {
            if let Some(heading) = group.heading.take() {
                heading.retire();
            }
        }
        if let Some(elision) = self.elision.take() {
            elision.retire();
        }
    }

    /// Commit the settled rows at the head of the tree, in order, stopping at
    /// the first row that is not settled.
    fn commit_from_head(&mut self) {
        while self.flushed < self.groups.len() {
            let index = self.flushed;
            while self.groups[index].flushed < self.groups[index].rows.len() {
                if !matches!(
                    self.groups[index].rows[self.groups[index].flushed].state,
                    RowState::Settled(_)
                ) {
                    return;
                }
                // The heading is earned by the line about to be written under
                // it, and asked for no earlier: a group whose every row was
                // retired unsettled — the abort path — must produce nothing at
                // all, which is the account `emit_phase_tree` gives it off a
                // TTY and the account the rollup's shortfall gives it here.
                if !self.open_group(index) {
                    return;
                }
                self.commit_head_row(index);
                self.paint_elision();
            }
            // The group is drained, but a group that can still gain a row must
            // keep its heading open for it — see rule 3.
            if !self.groups[index].sealed {
                return;
            }
            self.flushed += 1;
        }
    }

    /// Keep the live region inside the terminal's height.
    ///
    /// indicatif draws a `MultiProgress` top down and stops at the terminal's
    /// last line, so an over-full region drops the rows at its FOOT — the work
    /// that started most recently, which is exactly the work a reader is
    /// waiting on. The region therefore pays for its own overflow out of the
    /// rows that have nothing left to say.
    fn enforce_budget(&mut self) {
        while self.drawn_lines() > self.budget && self.free_one_line() {}
    }

    /// Give up ONE line, cheapest first, or answer `false` when every line left
    /// is one the region must keep.
    ///
    /// A settled row goes first: its outcome is held in the tree and still
    /// commits, in order, from the head, so the only cost is that a finished
    /// line leaves the screen and returns in the scrollback. Then a pending
    /// row, which describes work that has not started and is redrawn when it
    /// does. A RUNNING row's line is never taken — a region showing finished
    /// work while hiding what is in flight is the reading this whole tree
    /// exists to prevent.
    fn free_one_line(&mut self) -> bool {
        for gi in 0..self.groups.len() {
            for ri in 0..self.groups[gi].rows.len() {
                let row = &self.groups[gi].rows[ri];
                if matches!(row.state, RowState::Settled(_)) && row.line.is_some() {
                    self.elide(gi, ri);
                    return true;
                }
            }
        }
        // Taken from the foot: the furthest a pending row can be from the head
        // is the longest before anything needs to be read about it.
        for gi in (0..self.groups.len()).rev() {
            for ri in (0..self.groups[gi].rows.len()).rev() {
                if matches!(self.groups[gi].rows[ri].state, RowState::Pending)
                    && let Some(line) = self.groups[gi].rows[ri].line.take()
                {
                    line.retire();
                    return true;
                }
            }
        }
        false
    }

    /// Take a settled row's line, keeping the outcome the commit still owes.
    fn elide(&mut self, gi: usize, ri: usize) {
        if let Some(line) = self.groups[gi].rows[ri].line.take() {
            line.retire();
        }
        self.groups[gi].rows[ri].elided = true;
        self.elided += 1;
        self.paint_elision();
    }

    /// Keep the summary line in step with what it stands for, drawing it when
    /// the region first gives a line up and retiring it once every row it
    /// counted has been committed.
    ///
    /// It is never allowed to be silent: a dispatched action is on screen as a
    /// row, or committed to the scrollback, or counted here.
    fn paint_elision(&mut self) {
        if self.elided == 0 {
            if let Some(elision) = self.elision.take() {
                elision.retire();
            }
            return;
        }
        if self.elision.is_none() {
            self.elision = Some(self.printer.live_row_first(self.depth.saturating_sub(1)));
        }
        if let Some(elision) = &self.elision {
            elision.set_note(&format!(
                "{} held for commit",
                crate::pluralize(self.elided, "settled row")
            ));
        }
    }

    /// Write the group's first uncommitted row permanently and take it out of
    /// the live region.
    fn commit_head_row(&mut self, index: usize) {
        let Self {
            printer,
            groups,
            open,
            preopened,
            elided,
            ..
        } = self;
        let Some(target) = open
            .as_ref()
            .and_then(|open| open.guard.as_ref())
            .or_else(|| preopened.map(|(_, guard)| guard))
        else {
            // Unreachable: `open_group` returned true, so one of the two is set.
            debug_assert!(false, "a row was committed with no group heading open");
            return;
        };
        let group = &mut groups[index];
        let row = &mut group.rows[group.flushed];
        group.flushed += 1;
        if std::mem::take(&mut row.elided) {
            *elided = elided.saturating_sub(1);
        }
        let RowState::Settled(outcome) = std::mem::replace(&mut row.state, RowState::Flushed)
        else {
            debug_assert!(false, "an unsettled row reached the commit");
            return;
        };
        emit_action_line(printer, target, &outcome);
        if let Some(line) = row.line.take() {
            line.retire();
        }
    }

    /// Make `index` the group whose heading is open, closing the previous one
    /// if it is finished with. `false` when it cannot be opened yet.
    fn open_group(&mut self, index: usize) -> bool {
        if self.open.as_ref().is_some_and(|open| open.index == index) {
            return true;
        }
        if let Some(open) = &self.open {
            let previous = &self.groups[open.index];
            if !(previous.sealed && previous.drained()) {
                return false;
            }
            // Closes the guard, which unwinds the renderer's section stack
            // before the next one opens it again.
            self.open = None;
        }
        let group = &mut self.groups[index];
        let guard = match self.preopened {
            Some((owner, _)) if owner == group.owner => None,
            _ => {
                let Some(section) = self.section else {
                    // Unreachable: a tree with no section is never live, and a
                    // tree that is not live never reaches a flush.
                    debug_assert!(false, "a group heading was opened with no phase section");
                    return false;
                };
                let opened = section.section_owner(&group.label);
                opened.live_column(self.width);
                opened.commit_header();
                Some(opened)
            }
        };
        // The heading is on screen permanently now, so its live row is spent.
        if let Some(heading) = group.heading.take() {
            heading.retire();
        }
        self.open = Some(OpenGroup { index, guard });
        true
    }

    /// The row for `action` (or the group's own wait row when `action` is
    /// `None`), appended if it does not exist yet.
    ///
    /// Identity is the POINTER: every caller reads its `&'p Action` out of the
    /// plan's own `OwnerGroup::actions` storage, which outlives the phase, so
    /// the wait path and the dispatch path name the same address for the same
    /// action. A clone introduced between the scheduler and here would compare
    /// unequal and mint a SECOND row for one action — silently, since nothing
    /// in the type system says the reference must be the plan's own.
    /// `apply.rs`'s `action_key` rests on the same invariant.
    fn row_for(&mut self, owner: &'p Owner, action: Option<&'p Action>) -> (usize, usize) {
        let gi = self.group_for(owner);
        let existing = self.groups[gi].rows.iter().position(|row| match action {
            Some(action) => row.action.is_some_and(|held| std::ptr::eq(held, action)),
            None => row.action.is_none(),
        });
        if let Some(ri) = existing {
            return (gi, ri);
        }
        self.groups[gi].rows.push(Row {
            action,
            line: None,
            state: RowState::Pending,
            elided: false,
        });
        (gi, self.groups[gi].rows.len() - 1)
    }

    fn group_for(&mut self, owner: &'p Owner) -> usize {
        if let Some(index) = self.groups.iter().position(|group| group.owner == owner) {
            return index;
        }
        let label = OwnerLabel::new(owner.kind.as_str(), owner.name.as_str());
        let heading = match self.preopened {
            // Already on screen, above the whole live region.
            Some((preopened, _)) if preopened == owner => None,
            _ => {
                let line = self.printer.live_row_at(self.depth.saturating_sub(1));
                line.set_owner_label(&label);
                Some(line)
            }
        };
        self.groups.push(Group {
            owner,
            label,
            heading,
            rows: Vec::new(),
            flushed: 0,
            sealed: false,
        });
        self.groups.len() - 1
    }

    /// Draw a row that has none yet, at the foot of its own group.
    fn draw_row(&self, gi: usize, ri: usize) -> LiveRow<'p> {
        // Below the last line the group already owns, so a group that is not
        // contiguous in dispatch order still reads as one block. Its own
        // heading is that line for a group's first row.
        let after = self.groups[gi].rows[..ri]
            .iter()
            .rev()
            .find_map(|row| row.line.as_ref())
            .or(self.groups[gi].heading.as_ref());
        match after {
            Some(after) => self.printer.live_row_after(self.depth, after),
            None => self.printer.live_row_at(self.depth),
        }
    }

    /// Draw a pending row, if the live region has room for one.
    fn paint_pending(&mut self, gi: usize, ri: usize, subject: &str) {
        if !matches!(self.groups[gi].rows[ri].state, RowState::Pending) {
            return;
        }
        if self.groups[gi].rows[ri].line.is_none() {
            // A row describing work that has not started is the one row the
            // tree can choose not to draw, and an over-full live region hides
            // the rows at its FOOT — the work actually running. So the budget
            // is spent on what is happening before what is not.
            if self.drawn_lines() >= self.budget {
                return;
            }
            self.groups[gi].rows[ri].line = Some(self.draw_row(gi, ri));
        }
        if let Some(line) = &self.groups[gi].rows[ri].line {
            line.set_action_status(
                &RowStatus {
                    role: Role::Pending,
                    subject,
                    detail: None,
                    detail_muted: false,
                    duration: None,
                },
                self.width,
            );
        }
    }

    /// Every line the region currently holds, top down — the live region's own
    /// state, for a test that has to tell a row being REWRITTEN from a second
    /// row being added. The recording buffer shows both as one more line
    /// drawn, so it cannot answer that question and this can.
    #[cfg(test)]
    pub(super) fn drawn_rows(&self) -> Vec<String> {
        self.elision
            .iter()
            .map(super::super::output::live_row::LiveRow::text)
            .chain(self.groups.iter().flat_map(|group| {
                group
                    .heading
                    .iter()
                    .chain(group.rows.iter().filter_map(|row| row.line.as_ref()))
                    .map(super::super::output::live_row::LiveRow::text)
            }))
            .collect()
    }

    /// Lines the live region currently holds — the summary line included, since
    /// it occupies one like any other.
    fn drawn_lines(&self) -> usize {
        usize::from(self.elision.is_some())
            + self
                .groups
                .iter()
                .map(|group| {
                    usize::from(group.heading.is_some())
                        + group.rows.iter().filter(|row| row.line.is_some()).count()
                })
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::output::Verbosity;
    use crate::output::strip_ansi;
    use crate::providers::PackageAction;
    use crate::reconciler::types::PhaseName;

    fn install(manager: &str, package: &str) -> Action {
        Action::Package(PackageAction::Install {
            manager: manager.to_string(),
            packages: vec![package.to_string()],
            origin: "local".to_string(),
        })
    }

    fn done(subject: &str) -> ActionOutcome {
        ActionOutcome::for_test(subject, Duration::from_millis(1500))
    }

    fn drawn(buf: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
        strip_ansi(&buf.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Where each of `needles` was last drawn, panicking on one that never was
    /// — an ordering assertion over a missing line would pass vacuously.
    fn last_at(text: &str, needles: &[&str]) -> Vec<usize> {
        needles
            .iter()
            .map(|needle| {
                text.rfind(needle)
                    .unwrap_or_else(|| panic!("{needle:?} was never drawn: {text:?}"))
            })
            .collect()
    }

    #[test]
    fn a_row_settles_between_the_rows_it_was_dispatched_between() {
        // The invariant, at the position that can tell the two hypotheses
        // apart: the MIDDLE action finishes first. A two-row fixture cannot —
        // its settled line is last either way, whether it settled in its own
        // slot or was appended at the foot of the region. Here, an
        // append-at-the-foot writer puts `brew` BELOW `cargo`, and a reader is
        // told the second thing to start finished after the third.
        let (printer, buf) = Printer::for_test_with_live_bars();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let first = install("apt", "ripgrep");
        let middle = install("brew", "neovim");
        let last = install("cargo", "just");

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        let running_first = tree.dispatched(&managers, &first);
        let running_middle = tree.dispatched(&managers, &middle);
        let running_last = tree.dispatched(&managers, &last);

        running_middle.finish();
        tree.settled(&managers, &middle, done("brew install neovim"));

        let after = drawn(&buf);
        let order = last_at(
            &after,
            &[
                "apt install ripgrep",
                "✓ brew install neovim",
                "cargo install just",
            ],
        );
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "the settled row left the slot it was dispatched into: {after:?}"
        );
        // The same claim read off the region itself, where a row appended at
        // the foot is a FOURTH line rather than a reordered one.
        let rows = tree.drawn_rows();
        assert_eq!(
            rows.iter().filter(|l| l.contains("neovim")).count(),
            1,
            "the settled action was drawn twice: {rows:?}"
        );
        drop(running_first);
        drop(running_last);
    }

    #[test]
    fn a_settled_row_waits_for_the_rows_above_it_before_it_is_committed() {
        // Committing it early is exactly what would make it jump: the renderer
        // writes permanent lines ABOVE the whole live region, so a row
        // committed while an earlier row still runs lands above that row.
        let (printer, buf) = Printer::for_test_live_scrollback();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let slow = install("apt", "ripgrep");
        let quick = install("brew", "neovim");

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        let running = tree.dispatched(&managers, &slow);
        tree.dispatched(&managers, &quick).finish();
        tree.settled(&managers, &quick, done("brew install neovim"));

        assert!(
            !drawn(&buf).contains("brew install neovim"),
            "a settled row was committed over a row still running: {:?}",
            drawn(&buf)
        );

        running.finish();
        tree.settled(&managers, &slow, done("apt install ripgrep"));
        tree.finish();

        let scrollback = drawn(&buf);
        let order = last_at(&scrollback, &["apt install ripgrep", "brew install neovim"]);
        assert!(
            order[0] < order[1],
            "the scrollback must read in dispatch order, not completion order: {scrollback:?}"
        );
    }

    #[test]
    fn an_action_row_names_its_action_and_the_owner_is_named_once_per_group() {
        // The owner token comes off the rows because the heading carries it:
        // printed on every row it would say the same thing five times, and the
        // width it costs is width the action's own subject wanted.
        let (printer, buf) = Printer::for_test_live_scrollback();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let nvim = Owner::module("nvim");
        let tmux = Owner::module("tmux");
        let first = install("brew", "neovim");
        let second = install("apt", "ripgrep");
        let third = install("apt", "tmux");

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        for (owner, action) in [(&nvim, &first), (&tmux, &third), (&nvim, &second)] {
            tree.dispatched(owner, action).finish();
        }
        for (owner, action, subject) in [
            (&nvim, &first, "brew install neovim"),
            (&tmux, &third, "apt install tmux"),
            (&nvim, &second, "apt install ripgrep"),
        ] {
            tree.settled(owner, action, done(subject));
        }
        tree.finish();

        let scrollback = drawn(&buf);
        assert_eq!(
            scrollback.matches("module:nvim").count(),
            1,
            "an owner is named once, by its heading: {scrollback:?}"
        );
        assert_eq!(
            scrollback.matches("module:tmux").count(),
            1,
            "{scrollback:?}"
        );
        for line in scrollback.lines().filter(|l| l.contains("install")) {
            assert!(
                !line.contains("module:"),
                "an action row carried its owner token: {line:?}"
            );
        }
        // Both of `nvim`'s rows sit under its heading, and `tmux`'s under its
        // own — a group's rows stay together even though the dispatch
        // interleaved them.
        let order = last_at(
            &scrollback,
            &[
                "module:nvim",
                "brew install neovim",
                "apt install ripgrep",
                "module:tmux",
                "apt install tmux",
            ],
        );
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "a group's rows were split across another group's heading: {scrollback:?}"
        );
    }

    #[test]
    fn a_wait_row_names_the_blocked_action_and_becomes_that_actions_row() {
        // Two claims that are one claim: the row a blocked action waits in is
        // the row it starts in. If the wait were a line of its own the action
        // would appear twice, and the second copy would be below every row
        // dispatched during the wait.
        let (printer, buf) = Printer::for_test_with_live_bars();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let running = install("brew", "neovim");
        let blocked = install("brew-cask", "firefox");

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        let lane = tree.dispatched(&managers, &running);
        tree.waiting(&Held {
            waits: vec![Wait {
                owner: &managers,
                action: Some(&blocked),
                subject: "brew-cask install firefox · waiting on brew".to_string(),
            }],
            pending_owners: vec![managers.token()],
        });

        let waiting = drawn(&buf);
        assert!(
            waiting.contains("brew-cask install firefox · waiting on brew"),
            "the wait row must name what is blocked: {waiting:?}"
        );

        lane.finish();
        tree.settled(&managers, &running, done("brew install neovim"));
        let dispatched = tree.dispatched(&managers, &blocked);
        dispatched.finish();
        tree.settled(&managers, &blocked, done("brew-cask install firefox"));
        tree.finish();

        let after = drawn(&buf);
        let order = last_at(
            &after,
            &["brew install neovim", "✓ brew-cask install firefox"],
        );
        assert!(
            order[0] < order[1],
            "the blocked action left the row it waited in: {after:?}"
        );
    }

    #[test]
    fn a_blocked_action_starts_in_the_row_it_waited_in_rather_than_a_new_one() {
        // The half an ordering assertion cannot see: a dispatch that APPENDED
        // a row would leave the wait row behind, so the action would occupy
        // two lines at once and the sentence about what is blocking it would
        // outlive the block. Read off the region, because the recording buffer
        // shows a rewrite and an append as the same one extra line.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let running = install("brew", "neovim");
        let blocked = install("brew-cask", "firefox");

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        let lane = tree.dispatched(&managers, &running);
        tree.waiting(&Held {
            waits: vec![Wait {
                owner: &managers,
                action: Some(&blocked),
                subject: "brew-cask install firefox · waiting on brew".to_string(),
            }],
            pending_owners: vec![managers.token()],
        });
        assert_eq!(
            tree.drawn_rows()
                .iter()
                .filter(|line| line.contains("firefox"))
                .count(),
            1,
            "the blocked action must hold exactly one row: {:?}",
            tree.drawn_rows()
        );

        lane.finish();
        tree.settled(&managers, &running, done("brew install neovim"));
        let dispatched = tree.dispatched(&managers, &blocked);

        let rows = tree.drawn_rows();
        let naming: Vec<&String> = rows
            .iter()
            .filter(|line| line.contains("firefox"))
            .collect();
        assert_eq!(
            naming.len(),
            1,
            "dispatch appended a second row instead of starting the one that waited: {rows:?}"
        );
        assert!(
            !naming[0].contains("waiting on"),
            "the row still says it is blocked while it runs: {naming:?}"
        );
        dispatched.finish();
    }

    /// Dispatch `count` installs into one group, then settle the rows named by
    /// `settle` — everything else stays running. Returns the tree and the
    /// still-running handles, which the caller drops when it is done reading.
    fn over_full<'a>(
        printer: &'a Printer,
        section: &'a SectionGuard<'a>,
        owner: &'a Owner,
        actions: &'a [Action],
        settle: &[usize],
        budget: usize,
    ) -> (PhaseTree<'a, 'a>, Vec<LaneHandle<'a>>) {
        let mut tree =
            PhaseTree::new(printer, Some(section), None, section.depth + 1, 30).with_budget(budget);
        let mut lanes: Vec<Option<LaneHandle<'a>>> = actions
            .iter()
            .map(|action| Some(tree.dispatched(owner, action)))
            .collect();
        for &index in settle {
            if let Some(lane) = lanes[index].take() {
                lane.finish();
            }
            tree.settled(
                owner,
                &actions[index],
                done(&format!("apt install pkg{index}")),
            );
        }
        (tree, lanes.into_iter().flatten().collect())
    }

    fn packages(count: usize) -> Vec<Action> {
        (0..count)
            .map(|i| install("apt", &format!("pkg{i}")))
            .collect()
    }

    #[test]
    fn an_over_full_region_keeps_the_running_rows_and_counts_what_it_hid() {
        // A terminal shorter than the phase is the ordinary case, not an edge:
        // the head row is a slow install and every row dispatched behind it
        // stays in the region until it clears. indicatif truncates from the
        // FOOT, so an unbounded region hides the work in flight and shows only
        // finished lines — the "is it hung?" reading this tree exists to
        // remove, arrived at from the other side. Settled rows are what the
        // region gives up, because their outcome is already held and still
        // commits in order.
        let (printer, _buf) = Printer::for_test_with_live_bars();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let actions = packages(8);

        // Rows 1..=5 finish while row 0 — the head — is still running, so
        // none of them can be committed yet.
        let (tree, running) =
            over_full(&printer, &section, &managers, &actions, &[1, 2, 3, 4, 5], 5);

        let rows = tree.drawn_rows();
        assert!(
            rows.len() <= 5,
            "the region outgrew the terminal it was told it had: {rows:?}"
        );
        for still_running in ["pkg0", "pkg6", "pkg7"] {
            assert!(
                rows.iter().any(|line| line.contains(still_running)),
                "{still_running} is running and left the screen: {rows:?}"
            );
        }
        assert!(
            rows.iter()
                .any(|line| line.contains("5 settled rows held for commit")),
            "the region hid five rows without accounting for them: {rows:?}"
        );
        for hidden in ["pkg1", "pkg2", "pkg3", "pkg4", "pkg5"] {
            assert!(
                !rows.iter().any(|line| line.contains(hidden)),
                "{hidden} settled but was not given up: {rows:?}"
            );
        }
        drop(running);
    }

    #[test]
    fn a_region_that_gave_up_lines_still_commits_every_row_once_in_order() {
        // The line leaves the screen; the outcome does not leave the tree. What
        // the reader loses is a finished line for a while, which is the whole
        // trade — and the permanent record must be byte-for-byte what it would
        // have been on a terminal tall enough to hold everything.
        let (printer, buf) = Printer::for_test_live_scrollback();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let actions = packages(8);

        let (mut tree, running) =
            over_full(&printer, &section, &managers, &actions, &[1, 2, 3, 4, 5], 5);
        assert!(
            !drawn(&buf).contains("pkg1"),
            "a row behind a running head reached the scrollback early: {:?}",
            drawn(&buf)
        );

        for lane in running {
            lane.finish();
        }
        for index in [0, 6, 7] {
            tree.settled(
                &managers,
                &actions[index],
                done(&format!("apt install pkg{index}")),
            );
        }
        tree.finish();

        let scrollback = drawn(&buf);
        let subjects: Vec<String> = (0..8).map(|i| format!("apt install pkg{i}")).collect();
        for subject in &subjects {
            assert_eq!(
                scrollback.matches(subject.as_str()).count(),
                1,
                "{subject} reached the permanent record {} times: {scrollback:?}",
                scrollback.matches(subject.as_str()).count()
            );
        }
        let order = last_at(
            &scrollback,
            &subjects.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "the scrollback left dispatch order: {scrollback:?}"
        );
    }

    #[test]
    fn a_group_that_commits_nothing_prints_no_heading() {
        // The abort path: a group whose actions were dispatched and then lost
        // when the run was cut short has nothing to say, and a heading over an
        // empty body says the opposite — that something happened here and the
        // renderer could not name it. Off a TTY `emit_phase_tree` skips such a
        // group entirely; the live path must give the same account.
        let (printer, buf) = Printer::for_test_live_scrollback();
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let nvim = Owner::module("nvim");
        let tmux = Owner::module("tmux");
        let finished = install("brew", "neovim");
        let lost = install("apt", "tmux");

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        tree.dispatched(&nvim, &finished).finish();
        let interrupted = tree.dispatched(&tmux, &lost);
        tree.settled(&nvim, &finished, done("brew install neovim"));
        // The worker never answered for it — an abort, or a lane that ended
        // under one.
        drop(interrupted);
        tree.finish();

        let scrollback = drawn(&buf);
        assert!(
            scrollback.contains("module:nvim") && scrollback.contains("brew install neovim"),
            "the group that DID commit must still be written: {scrollback:?}"
        );
        assert!(
            !scrollback.contains("module:tmux"),
            "a group that committed nothing opened a heading anyway: {scrollback:?}"
        );
        assert!(
            !scrollback.contains("(none)"),
            "an empty group body reached the transcript: {scrollback:?}"
        );
    }

    #[test]
    fn off_a_tty_the_tree_is_inert_and_draws_nothing() {
        // The off-TTY contract: no rows, no headings, and every outcome held
        // for `emit_phase_tree` to write in plan order at phase close.
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        let section = printer.section_phase(&PhaseName::Packages.section_label());
        let managers = Owner::cfgd("managers");
        let action = install("brew", "neovim");
        let before = drawn(&buf);

        let mut tree = PhaseTree::new(&printer, Some(&section), None, section.depth + 1, 30);
        assert!(
            !tree.is_live(),
            "a printer with no live region draws no rows"
        );
        tree.dispatched(&managers, &action).finish();
        tree.waiting(&Held {
            waits: vec![Wait {
                owner: &managers,
                action: Some(&action),
                subject: "brew install neovim · waiting on brew".to_string(),
            }],
            pending_owners: vec![managers.token()],
        });
        tree.finish();

        assert_eq!(
            drawn(&buf),
            before,
            "the tree wrote to a terminal that has no live region"
        );
    }
}
