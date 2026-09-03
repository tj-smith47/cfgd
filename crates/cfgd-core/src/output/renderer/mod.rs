//! The renderer is the single layout authority. It owns:
//! - indent depth (push/pop per Section)
//! - blank-line state machine (no leading, no trailing, exactly one between siblings)
//! - kv auto-batching (consecutive `kv` calls coalesce into one aligned block)
//! - glyph + style lookup via Theme
//!
//! Every other module routes terminal writes through here.
//!
//! `RenderState::{depth,push,pop}` and `indent_prefix` are reachable only
//! from tests and from inside the renderer module; the narrow `dead_code`
//! allow keeps them addressable without a workspace-wide warning.
#![allow(dead_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

use super::{Theme, Verbosity, cursor_safe};

mod glyphs;
pub mod kv;
pub mod section;
pub mod status;
pub mod table;
pub(crate) mod wrap;
pub(crate) use glyphs::{
    action_detail_is_muted, action_subject_style, finalize_painted_subject, finalize_subject,
    role_glyph,
};
pub use status::{Elapsed, StatusFields};
pub use table::Table;

/// The ONE `"  ".repeat(depth)` in the workspace. Every surface that indents
/// by depth — the renderer's own line pusher, kv blocks, and the three live
/// primitives (`Spinner`, `OutputWindow`, `LiveRow`) that cannot reach a
/// `Renderer` to call [`Renderer::indent_prefix`] — calls through here rather
/// than re-deriving the multiplication at its own site.
pub(crate) fn indent_prefix(depth: usize) -> String {
    "  ".repeat(depth)
}

/// The kind of a top-level (outside any section) group emission.
///
/// Blank lines separate GROUPS, not the lines inside one. Three rules follow
/// from that and are enforced in `open_top_group`:
///
///   - consecutive emissions of a kind whose `runs_contiguously` is true
///     (statuses, hints) are one group and render with no blank between them
///   - a heading binds to whatever it introduces, so nothing directly after a
///     top-level heading is preceded by a blank
///   - the streaming → buffered seam always separates: a streamed line and a
///     buffered `Doc`'s line never join into one group
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TopGroup {
    Heading,
    Status,
    Hint,
    Bullet,
    CodeBlock,
    Note,
    KvBlock,
    Paragraph,
    Table,
}

impl TopGroup {
    /// True for single-line kinds that read as a list when repeated — a run of
    /// `✓ Created …` lines is one block, not seven blocks of one line.
    fn runs_contiguously(self) -> bool {
        matches!(self, TopGroup::Status | TopGroup::Hint | TopGroup::Bullet)
    }
}

/// Per-Printer rendering state. Held inside `Mutex` because multiple
/// `SectionGuard`s may share the same `&Printer` and write concurrently
/// from one thread (drop ordering is single-threaded but borrow-checker
/// can't see that).
/// Where a buffered kv block belongs, captured at the moment its first row
/// was written rather than at the moment it drains.
#[derive(Clone, Copy)]
pub(crate) struct KvAnchor {
    /// The indent depth the rows were written at.
    pub(crate) depth: usize,
    /// No section was open when the rows were written, so the block is a
    /// top-level group and owes the next top-level emission a blank line.
    pub(crate) at_top_level: bool,
    /// The rows followed a top-level heading with no section open, so they
    /// bind to it: one level deeper, and no blank line between.
    pub(crate) bound_to_heading: bool,
}

pub(crate) struct RenderState {
    /// Current indent depth. Section open = +1, section close = -1.
    indent_depth: usize,
    /// True if the renderer should emit a blank line before the next non-blank
    /// emission (set by section close, cleared by next emit).
    blank_pending: bool,
    /// True until the first emission lands; suppresses leading blank.
    leading: bool,
    /// Buffered kvs awaiting a non-kv emission to flush as one aligned block.
    kv_buffer: Vec<crate::output::KvPair>,
    /// Where the buffered rows were WRITTEN, captured when the first one was
    /// buffered. The block's placement is a fact about the moment its rows
    /// were emitted, and the drain can happen arbitrarily later — after a
    /// section frame has been pushed, which moves `indent_depth` and empties
    /// the heading binding out from under rows that belong to the heading.
    kv_anchor: Option<KvAnchor>,
    pub(crate) section_stack: Vec<crate::output::renderer::section::SectionFrame>,
    /// True iff the most recent emission was a top-level heading and no other
    /// emission has happened since. Consumed by the next top-level kv_block,
    /// which re-anchors the block at depth+1 so it visually nests under the
    /// heading. Reset by any other emission (status, section header, bullet,
    /// etc.).
    pub(crate) last_was_top_heading: bool,
    /// True while a top-level heading is open and nothing has been written
    /// that leaves its scope. `last_was_top_heading` answers "was the heading
    /// the LAST thing written" — which is what decides whether the blank line
    /// after it is swallowed — and every emission clears it. Placement is the
    /// other question: a heading's prose, and the facts that follow the prose,
    /// both belong under the heading, so the INDENT is decided by this flag
    /// instead. Armed by `mark_top_heading` and re-armed by the next heading.
    pub(crate) top_heading_scope: bool,
    /// Kind of the most recent top-level group emission, or `None` when the
    /// last thing written was not one (a section body, a section close).
    /// Consumed by the next top-level emit to decide whether it continues that
    /// group or starts a new one.
    pub(crate) last_top_group: Option<TopGroup>,
    /// Nesting depth of buffered `Doc` rendering; 0 while output is streaming.
    /// A streamed line and a buffered Doc's line are never the same group even
    /// when they are the same kind — the seam between them always separates.
    doc_depth: usize,
    /// Whether the most recent top-level emission came from inside a `Doc`.
    /// Compared against the current side of the seam in `open_top_group`.
    last_top_in_doc: bool,
    /// The alignment column the WHOLE report shares, claimed once by whoever
    /// can see all of it. A section declaring a live column takes this in
    /// preference to its own, so every action row of a run — the preview's, the
    /// apply tree's, and the pseudo-phases' beside them — pads to one column
    /// instead of one per phase.
    pub(crate) report_column: Option<usize>,
    /// The subject budget the same claim settled — what THIS report's rows
    /// may occupy, widened from the printer's floor by what the report's own
    /// trailing allowance leaves — so every reader of
    /// `Printer::subject_budget` inside the claim cuts one action one way.
    pub(crate) report_subject_budget: Option<usize>,
}

impl RenderState {
    /// The anchor a kv row written right now belongs to.
    pub(crate) fn kv_anchor_here(&self) -> KvAnchor {
        let at_top_level = self.section_stack.is_empty();
        KvAnchor {
            depth: self.indent_depth,
            at_top_level,
            bound_to_heading: at_top_level && self.indent_depth == 0 && self.top_heading_scope,
        }
    }

    pub(crate) fn new() -> Self {
        Self {
            indent_depth: 0,
            blank_pending: false,
            leading: true,
            kv_buffer: Vec::new(),
            kv_anchor: None,
            section_stack: Vec::new(),
            last_was_top_heading: false,
            top_heading_scope: false,
            last_top_group: None,
            doc_depth: 0,
            last_top_in_doc: false,
            report_column: None,
            report_subject_budget: None,
        }
    }

    /// Seed a fresh top-level state that continues a PRIOR renderer's
    /// blank-line bookkeeping, for a derived `Printer` that shares the same
    /// underlying sink (`Printer::rethemed`, `Printer::at_verbosity`).
    ///
    /// `Printer::build_derived` mints a brand-new `Renderer`, and a bare
    /// `RenderState::new()` there defaults `leading: true` — which reads as
    /// "nothing has been written to this sink yet" even when the parent
    /// renderer just wrote a section and owes the next heading a blank line.
    /// `cfgd init --theme <t>` hits exactly this: `cmd_init` closes its
    /// "Initialize cfgd" section (arming `blank_pending` on the OLD renderer)
    /// and then re-themes, which swaps in a renderer that has never heard of
    /// that close and drops the blank line ahead of `Apply`. Structural state
    /// — indent depth, the section stack, the buffered kvs — does NOT carry
    /// over: the derived renderer starts at a genuine fresh top level, which
    /// is what every `build_derived` caller intends.
    pub(crate) fn continued_from(prior: &RenderState) -> Self {
        Self {
            blank_pending: prior.blank_pending,
            leading: prior.leading,
            last_top_group: prior.last_top_group,
            last_was_top_heading: prior.last_was_top_heading,
            top_heading_scope: prior.top_heading_scope,
            last_top_in_doc: prior.last_top_in_doc,
            ..Self::new()
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.indent_depth
    }

    pub(crate) fn push(&mut self) -> usize {
        self.indent_depth += 1;
        self.indent_depth
    }

    pub(crate) fn pop(&mut self) {
        debug_assert!(self.indent_depth > 0, "renderer pop at depth 0");
        if self.indent_depth > 0 {
            self.indent_depth -= 1;
        }
    }

    /// Drop the pending blank when this emission continues the previous group
    /// rather than starting a new one.
    pub(crate) fn open_top_group(&mut self, kind: TopGroup) {
        if !self.section_stack.is_empty() || !self.blank_pending {
            return;
        }
        let continues = match kind {
            // A heading introduces what follows it, so it never binds to the
            // heading above: two consecutive headings are two groups.
            TopGroup::Heading => false,
            _ => {
                self.last_was_top_heading
                    || (kind.runs_contiguously()
                        && self.last_top_group == Some(kind)
                        // Streamed lines and a buffered Doc's lines are
                        // different groups even when the kind matches: the
                        // seam between them keeps its one blank line.
                        && (self.doc_depth > 0) == self.last_top_in_doc)
            }
        };
        if continues {
            self.blank_pending = false;
        }
    }

    /// Close a top-level group emission: the next top-level emit gets one
    /// blank line unless `open_top_group` decides it continues this group.
    pub(crate) fn mark_top_level_group(&mut self, kind: TopGroup) {
        self.mark_group_written_at_top_level(kind, self.section_stack.is_empty());
    }

    /// `mark_top_level_group` for an emission whose top-levelness is a fact
    /// about when it was WRITTEN rather than about the stack as it stands now.
    /// A buffered kv block drains arbitrarily later — after the following
    /// section pushed its frame — and judged there it marks no group at all,
    /// so the section header it precedes gets no blank line of its own while a
    /// status line written in the same place gets one.
    pub(crate) fn mark_group_written_at_top_level(&mut self, kind: TopGroup, at_top_level: bool) {
        if at_top_level {
            self.blank_pending = true;
            self.last_top_group = Some(kind);
            self.last_top_in_doc = self.doc_depth > 0;
        } else {
            self.last_top_group = None;
        }
    }

    pub(crate) fn clear_blank_pending(&mut self) {
        self.blank_pending = false;
    }

    /// Arm the flag a following top-level kv block consumes to re-anchor
    /// itself one level deeper. Only a heading at the root arms it; inside a
    /// section there is nothing to nest under.
    pub(crate) fn mark_top_heading(&mut self) {
        if self.section_stack.is_empty() {
            self.last_was_top_heading = true;
            self.top_heading_scope = true;
        }
    }
}

/// The live-region bookkeeping every renderer writing ONE `MultiProgress`
/// shares, and the reason it is `Arc`-held rather than a pair of fields.
///
/// `Printer::build_derived` mints a fresh `Renderer` around the SAME
/// `MultiProgress` and the SAME sinks. A derived renderer counting its own
/// bars starts at zero, so `emit_block`'s routing gate answers "no bar is
/// live" while the parent's spinner is painting, takes the raw branch, and
/// leaves that paint on the terminal for good — the quiet library sinks
/// (`cli/sync.rs`, `cli/compliance.rs`, `daemon/sync.rs`, …) all emit through
/// exactly that shape, and their `Fail` statuses, `alert()`s and
/// `deprecation()`s survive `Verbosity::Quiet`. Sharing the count makes the
/// routing decision one decision for the whole live region instead of one per
/// renderer that happens to hold a handle to it.
///
/// The broken-terminal latch is shared for the same reason: one dead terminal
/// is dead for every renderer writing it, and a per-renderer latch would have
/// each of them re-discover the break with its own dropped emission.
pub(crate) struct LiveBarState {
    /// Bars currently drawn, maintained by `LiveBarGuard`. Bound to the bars
    /// that were actually `multi.add`ed: a hidden bar is never added, so
    /// counting one would open the routing gate over an empty multi.
    live_bars: AtomicUsize,
    /// One-way latch: set when the terminal behind the bars is judged dead.
    /// Later writes go straight to the sink, which swallows write errors
    /// exactly as every write did before the routing existed.
    bars_broken: AtomicBool,
    /// Consecutive routed writes that failed without proving the terminal
    /// dead — see [`LiveBarState::record_route_failure`].
    route_failures: AtomicUsize,
    /// The sink whose cursor the region hides while any bar is counted live
    /// and shows again when the last one drops — the stderr the bars repaint.
    /// `None` for a renderer with no bars, which never has a live region.
    cursor: Option<std::sync::Arc<dyn Writer>>,
}

/// Consecutive transient failures that stand in for a dead terminal.
///
/// One is not enough: latching on a single refusal hands the whole process
/// (every renderer AND the tracing writer share one latch) back to raw writes
/// over a live region — the strand this type exists to prevent, re-entered
/// through the failure path.
///
/// Two is not free either, and the cost is paid in both directions. A terminal
/// that dies with a kind the list below does not name costs ONE extra stranded
/// paint before the latch takes: the first refusal falls through to the raw
/// sink while bars are still counted live. And two unrelated transients with no
/// successful write between them latch routing off permanently for a terminal
/// that was never broken, which costs the region its repaints for the rest of
/// the process. Two is the smallest number that keeps a single refusal from
/// disabling the mechanism, and the run resets on the first write that lands
/// (`note_route_success`), so the second cost needs two failures inside one
/// unbroken run rather than two failures in a session.
const ROUTE_FAILURES_BEFORE_LATCH: usize = 2;

impl LiveBarState {
    fn new(cursor: Option<std::sync::Arc<dyn Writer>>) -> Self {
        Self {
            live_bars: AtomicUsize::new(0),
            bars_broken: AtomicBool::new(false),
            route_failures: AtomicUsize::new(0),
            cursor,
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.live_bars.load(Relaxed)
    }

    pub(crate) fn broken(&self) -> bool {
        self.bars_broken.load(Relaxed)
    }

    fn mark_broken(&self) {
        self.bars_broken.store(true, Relaxed);
    }

    /// A routed write landed: the region is answering, so any earlier refusal
    /// was transient and the run of failures restarts.
    pub(crate) fn note_route_success(&self) {
        // Relaxed on purpose, like every other field here: the latch decides
        // where a LINE goes, and a decision made against a value one write out
        // of date costs one line's routing, never correctness.
        if self.route_failures.load(Relaxed) != 0 {
            self.route_failures.store(0, Relaxed);
        }
    }

    /// A routed write was refused. Answers whether routing is now latched off.
    ///
    /// An error kind that cannot be transient (the far end of the terminal is
    /// gone, or was never there) latches at once, because retrying it every
    /// line buys nothing. Everything else — an interrupted write, a pipe that
    /// was momentarily full, a timeout — is a condition the next write may not
    /// meet, so it only counts toward [`ROUTE_FAILURES_BEFORE_LATCH`].
    pub(crate) fn record_route_failure(&self, err: &std::io::Error) -> bool {
        use std::io::ErrorKind::*;
        let terminal_is_gone = matches!(
            err.kind(),
            BrokenPipe | NotConnected | PermissionDenied | Unsupported | UnexpectedEof
        );
        if terminal_is_gone
            || self.route_failures.fetch_add(1, Relaxed) + 1 >= ROUTE_FAILURES_BEFORE_LATCH
        {
            self.mark_broken();
            return true;
        }
        false
    }
}

/// Renderer is created per Printer. All state lives in `RenderState` behind a
/// Mutex so the caller doesn't see interior mutability.
pub struct Renderer {
    pub(crate) theme: Theme,
    pub(crate) verbosity: Verbosity,
    pub(crate) state: Mutex<RenderState>,
    /// The Printer's MultiProgress — `Some` only when this renderer's stderr
    /// sink IS that MultiProgress's draw target, because `println` writes the
    /// multi's target rather than the sink. indicatif's handle is `Clone` and
    /// internally `Arc`-shared.
    pub(crate) bars: Option<indicatif::MultiProgress>,
    /// Live-bar count + broken latch for `bars`, shared with every renderer
    /// derived from this one — see [`LiveBarState`].
    pub(crate) live: std::sync::Arc<LiveBarState>,
    /// Non-zero while a `DepthInheritGuard` is alive.
    pub(crate) inherit_guards: AtomicUsize,
    /// Whether [`Self::render_hint`] emits anything. Settled once by
    /// `Printer::with_hints_enabled` (from `--no-hints` / `CFGD_USAGE_HINTS` /
    /// `spec.usageHints`) and then read by every renderer sharing this
    /// printer's decision — `SectionGuard` and `Doc` rendering hold their own
    /// `Arc<Renderer>` clone rather than asking the `Printer`, so the flag has
    /// to live here, at the one seam every hint producer already reaches.
    /// `AtomicBool` rather than a constructor parameter: threading a new
    /// argument through `Renderer::new`/`with_bars` would touch every one of
    /// their ~70 test call sites for a decision the CLI's one production call
    /// site ever needs to flip off (the kubectl plugin's minimal global-flag
    /// subset omits `--no-hints` entirely, so hints there always stay on).
    pub(crate) hints_enabled: AtomicBool,
}

impl Renderer {
    pub fn new(theme: Theme, verbosity: Verbosity) -> Self {
        Self {
            theme,
            verbosity,
            state: Mutex::new(RenderState::new()),
            bars: None,
            live: std::sync::Arc::new(LiveBarState::new(None)),
            inherit_guards: AtomicUsize::new(0),
            hints_enabled: AtomicBool::new(true),
        }
    }

    /// The one production wiring: a renderer whose stderr sink is `bars`'s own
    /// draw target, so lines emitted while a bar is live can be routed through
    /// it. `pub(crate)` because that invariant is not something an external
    /// caller can be trusted to hold.
    ///
    /// `cursor` is that same sink, handed over so the live region can hide
    /// and show the cursor of the terminal the bars repaint — a renderer given
    /// bars but no cursor would draw spinners beside a parked cursor block.
    pub(crate) fn with_bars(
        theme: Theme,
        verbosity: Verbosity,
        bars: indicatif::MultiProgress,
        cursor: std::sync::Arc<dyn Writer>,
    ) -> Self {
        Self {
            bars: Some(bars),
            live: std::sync::Arc::new(LiveBarState::new(Some(cursor))),
            ..Self::new(theme, verbosity)
        }
    }

    /// Same wiring as [`Self::with_bars`], but the fresh renderer's blank-line
    /// bookkeeping continues `seed` — a snapshot taken via
    /// [`Self::continuation_seed`] from the renderer this one replaces —
    /// instead of starting blank. The two renderers write the SAME sink
    /// (`Printer::build_derived` clones `sink_stderr`/`sink_stdout` rather
    /// than opening new ones), so the derived renderer's first heading must
    /// still see whatever blank-line debt the replaced renderer owed it.
    ///
    /// `live` is the replaced renderer's own [`LiveBarState`], not a fresh one:
    /// both renderers write the one live region, so both have to route their
    /// emissions through the one clear-and-repaint decision it holds.
    ///
    /// `hints_enabled` is the replaced renderer's own decision (read via
    /// `Renderer::hints_enabled`), not a fresh default: a re-themed or
    /// derived printer must not un-suppress hints the parent turned off.
    pub(crate) fn with_bars_continued(
        theme: Theme,
        verbosity: Verbosity,
        bars: indicatif::MultiProgress,
        seed: RenderState,
        live: std::sync::Arc<LiveBarState>,
        hints_enabled: bool,
    ) -> Self {
        // Spelled out rather than `..Self::new(theme, verbosity)`: the struct
        // update operand is evaluated in full, so the shorthand mints a fresh
        // `Arc<LiveBarState>` per derived printer only to drop it unread.
        Self {
            theme,
            verbosity,
            state: Mutex::new(seed),
            bars: Some(bars),
            live,
            inherit_guards: AtomicUsize::new(0),
            hints_enabled: AtomicBool::new(hints_enabled),
        }
    }

    /// Whether [`Self::render_hint`] emits anything on this renderer.
    pub(crate) fn hints_enabled(&self) -> bool {
        self.hints_enabled.load(Relaxed)
    }

    /// Flip whether [`Self::render_hint`] emits anything. Called once, right
    /// after construction, by `Printer::with_hints_enabled` — never mid-run.
    pub(crate) fn set_hints_enabled(&self, enabled: bool) {
        self.hints_enabled.store(enabled, Relaxed);
    }

    /// Snapshot the blank-line bookkeeping a derived renderer needs from this
    /// one — see [`RenderState::continued_from`] for why structural state
    /// (indent depth, section stack, buffered kvs) is deliberately left out.
    pub(crate) fn continuation_seed(&self) -> RenderState {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        RenderState::continued_from(&s)
    }

    /// Build the indent prefix for the current depth.
    pub(crate) fn indent_prefix(&self, depth: usize) -> String {
        indent_prefix(depth)
    }

    /// Called by every top-level emit before writing. Returns the depth at
    /// which the emit should actually render (clamped to current open section).
    ///
    /// A top-level emit (depth 0) reached while a `SectionGuard` is alive is
    /// a programming error. Debug builds `debug_assert!` to flag the call
    /// site loudly; release builds log a `tracing::warn!` once per process
    /// and re-route the emit to the section's current depth so the output
    /// stays readable.
    pub(crate) fn enforce_structural_top_level(&self, expected_depth: usize) -> usize {
        let actual = self.state.lock().unwrap_or_else(|e| e.into_inner()).depth();
        if expected_depth == 0 && actual > 0 {
            // Top-level emit while a section is open.
            debug_assert!(
                false,
                "top-level emit at depth 0 while section open at depth {actual}"
            );
            // Release build: warn once, render at the section's depth.
            // Process-global: test runs observe at most one warning across the entire suite.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "cfgd output: top-level Printer emit reached while a SectionGuard \
                     was open. The emit was re-routed to the section's depth. Fix the \
                     call site (move it inside or outside the section)."
                );
            });
            actual
        } else {
            expected_depth
        }
    }

    /// Depth for an emit that is correct wherever it is reached — the advisory
    /// channel (`Printer::alert`).
    ///
    /// No assert, in any mode, and deliberately not `inherit_depth`: an alert is
    /// emitted at the site that DISCOVERS the effect it describes (a source
    /// constraint bypassed while composing), and that site does not know whether
    /// its caller opened a section, cannot open a `DepthInheritGuard` on the
    /// caller's behalf, and must not be the reason a debug build panics — the
    /// message is the one the user has to see. Rendering at the open section's
    /// depth is the readable shape anyway, which is exactly what the release
    /// re-route of `enforce_structural_top_level` already did.
    pub(crate) fn advisory_depth(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).depth()
    }

    /// Depth for a NON-structural emit (status / hint / note / spinner / run).
    ///
    /// With inheritance off this is exactly `enforce_structural_top_level(0)`,
    /// assert and all — the guard stays armed for the hundreds of call sites
    /// that have nothing to do with a run tree. With it on, the innermost open
    /// section's depth is returned silently, so library code keeps its
    /// `&Printer` signature and renders inside the group its caller opened.
    pub(crate) fn inherit_depth(&self) -> usize {
        if self.inherit_guards.load(Relaxed) == 0 {
            return self.enforce_structural_top_level(0);
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).depth()
    }

    /// The ONE exit from the renderer to a terminal. Lines are emitted as one
    /// block per logical emission: with bars live, indicatif clears and redraws
    /// once around the whole block rather than once per line.
    fn emit_block(&self, w: &dyn Writer, lines: &[String]) {
        // An emission that produced no line has nothing to route. The plain
        // path already writes nothing, but `println` takes a STRING and joining
        // nothing yields `""` — one blank row painted into the live region,
        // between the lines around it, for every emission the renderer merely
        // buffered (a status held for column alignment, a deferred header).
        if lines.is_empty() {
            return;
        }
        let plain = || {
            for line in lines {
                w.write_line(line);
            }
        };
        // ONE match with a guard: a second match on `self.bars` is how the
        // latch arm below loses its fallback without anyone noticing.
        match &self.bars {
            Some(mp) if !self.live.broken() && self.live.count() > 0 && !mp.is_hidden() => {
                // `println` splits on '\n' itself, so one call is one
                // clear/redraw cycle for the whole emission.
                match mp.println(lines.join("\n")) {
                    Ok(()) => self.live.note_route_success(),
                    Err(e) => {
                        // Fall through whether or not this refusal latched: the
                        // line still has to reach the user, and the emission
                        // that discovers a broken terminal is the one most
                        // likely to explain it.
                        self.live.record_route_failure(&e);
                        plain();
                    }
                }
            }
            _ => plain(),
        }
    }

    /// Emit a pre-built block (diff, syntax-highlighted code) at `depth`.
    /// Flushes any deferred section header first and indents every line to
    /// `depth` — a raw block nests under whatever section opened it exactly
    /// like any other emission — but never word-wraps a line: each entry is
    /// already one complete rendered row, and wrapping it mid-token would no
    /// longer be the diff/syntax content it was built from.
    pub(crate) fn emit_raw_block(&self, w: &dyn Writer, depth: usize, lines: &[String]) {
        self.emit_with(w, |e| e.push_raw_block(depth, lines));
    }

    /// Run one logical emission: take the state lock once, build every line of
    /// the emission through collectors that can reach neither the lock nor the
    /// sink, then flush them as a single block while still holding the lock.
    pub(crate) fn emit_with(&self, w: &dyn Writer, build: impl FnOnce(&mut Emitting<'_>)) {
        // Read from the sink BEFORE the guard is taken.
        let wrap_cols = w.wrap_columns();
        let mut lines = Vec::new();
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut emitting = Emitting {
                theme: &self.theme,
                verbosity: self.verbosity,
                state: &mut s,
                wrap_cols,
                out: &mut lines,
            };
            build(&mut emitting);
        }
        self.emit_block(w, &lines);
    }
}

/// Increments the renderer's live-bar count on construction and decrements it
/// on drop. Held by the `Spinner` / `ProgressBar` wrapper, so the count tracks
/// the bar's real lifetime with no paired decrement to forget.
///
/// The cursor rides the same count: hidden as the FIRST bar goes up, shown as
/// the LAST one drops, and never touched by the bars in between. RAII is what
/// makes it hold across an early `?` and a panic alike — the guard drops on
/// every exit path the bar's wrapper has, so there is no "show" for a call
/// site to forget. Ctrl-C is the one exit no drop runs on; `cursor.rs`'s
/// signal hook covers it.
pub(crate) struct LiveBarGuard(std::sync::Arc<LiveBarState>);

impl LiveBarGuard {
    pub(crate) fn acquire(renderer: &std::sync::Arc<Renderer>) -> Self {
        let live = &renderer.live;
        if live.live_bars.fetch_add(1, Relaxed) == 0
            && let Some(cursor) = &live.cursor
        {
            cursor.set_cursor_visible(false);
        }
        Self(live.clone())
    }
}

impl Drop for LiveBarGuard {
    fn drop(&mut self) {
        if self.0.live_bars.fetch_sub(1, Relaxed) == 1
            && let Some(cursor) = &self.0.cursor
        {
            cursor.set_cursor_visible(true);
        }
    }
}

/// Everything a line builder may read, and nothing it may reach the terminal
/// or the state lock through. It holds `&mut RenderState` — the guard the
/// entry function already took — so a collector that tried to lock would need
/// a `&Renderer` it does not have, and one that tried to write would need a
/// sink it does not have.
pub(crate) struct Emitting<'a> {
    pub(crate) theme: &'a Theme,
    pub(crate) verbosity: Verbosity,
    pub(crate) state: &'a mut RenderState,
    pub(crate) wrap_cols: Option<usize>,
    pub(crate) out: &'a mut Vec<String>,
}

impl Emitting<'_> {
    /// Collect one physical line at the given depth, honoring blank-pending.
    ///
    /// Drains both deferred buffers first — otherwise buffered kvs and the
    /// statuses a section holds back for column alignment would render *after*
    /// this line, inverting the call order.
    pub(crate) fn push_line(&mut self, depth: usize, body: &str) {
        self.push_line_with_trailer(depth, body, None, None);
    }

    /// Same as [`Self::push_line`], but with `trailer` — a status line's
    /// duration suffix — landed on the LAST physical line a wrap produces
    /// (`wrap::wrap_body_with_trailer`) at `trailer_column`, the group's
    /// settled alignment column measured from the glyph, instead of flowing
    /// inline with the rest of the wrapped body. `None` is a group with no
    /// column, where the trailer glues inline as it does on every sibling.
    pub(crate) fn push_line_with_trailer(
        &mut self,
        depth: usize,
        body: &str,
        trailer: Option<&str>,
        trailer_column: Option<usize>,
    ) {
        self.drain_buffers();
        self.push_line_undrained(depth, body, trailer, trailer_column);
    }

    /// [`Self::push_line`] with no wrap: ONE physical line, however wide.
    ///
    /// A hint's `$ ` block line is the exact text the reader copies, so a wrap
    /// splicing a newline and a hang indent into the middle of it hands them
    /// something else. A terminal soft-wraps the line instead, which costs a
    /// visual row and keeps the command whole. Only content whose BYTES are
    /// the promise belongs here; ordinary prose wraps.
    pub(crate) fn push_line_unwrapped(&mut self, depth: usize, body: &str) {
        // Drained under the real width: a kv row emptied out of the buffer on
        // the way past is still ordinary content and still wraps.
        self.drain_buffers();
        let wrap_cols = self.wrap_cols.take();
        self.push_line_undrained(depth, body, None, None);
        self.wrap_cols = wrap_cols;
    }

    /// Empty both deferred buffers, oldest content first.
    ///
    /// A section's pending statuses are always older than whatever is still in
    /// the kv buffer: a status buffered while kv rows were waiting empties them
    /// on the way in ([`super::printer::Printer::status`]'s buffered route), so at most one of
    /// the two ever holds content that predates the other. Draining them the
    /// other way round would invert exactly the call order this exists to keep.
    pub(crate) fn drain_buffers(&mut self) {
        self.drain_pending_statuses();
        self.drain_kv_buffer();
    }

    /// [`Self::push_line_with_trailer`] without the drain, for the drains
    /// themselves: a status flushed out of a section's pending buffer must not
    /// re-enter the drain, or a kv block written after it slips in first.
    pub(crate) fn push_line_undrained(
        &mut self,
        depth: usize,
        body: &str,
        trailer: Option<&str>,
        trailer_column: Option<usize>,
    ) {
        // The sink appends its own trailing newline per line, so a trailing
        // newline already in `body` would smuggle a physical line break past
        // the blank-line accounting (a Status subject ending with `\n` would
        // produce a stray blank between this emission and the next, breaking
        // the one-blank-between-siblings invariant). Internal newlines are a
        // supported shape — a brew caveat is genuinely two sentences — and
        // `wrap_body` lays them out as continuations of this line rather than
        // as unmarked lines of their own.
        let trimmed = body.trim_end_matches(['\n', '\r']);
        if self.state.leading {
            self.state.leading = false;
            self.state.blank_pending = false;
        } else if self.state.blank_pending {
            self.out.push(String::new());
            self.state.blank_pending = false;
        }
        // Any emission resets the heading-just-emitted flag. Heading itself
        // sets the flag back true after this call returns. A ROW is the
        // command reporting on itself rather than more of the heading's own
        // introduction, so it also ends the heading's scope: the closing facts
        // block a verb leaves after its result rows is its own top-level
        // block, not the header's continuation.
        self.state.last_was_top_heading = false;
        self.state.top_heading_scope = false;
        let prefix = indent_prefix(depth);
        // The group measures its column from the glyph; the wrap measures a
        // row from the margin.
        let column = trailer_column.map(|c| prefix.len() + c);
        for physical in
            wrap::wrap_body_with_trailer(trimmed, &prefix, self.wrap_cols, trailer, column)
        {
            self.out.push(physical);
        }
    }

    /// Collect a prose paragraph at `depth`: the folded text, wrapped, with
    /// every continuation flush to the same column the first line starts at.
    ///
    /// Laid out through `wrap_segment` with the SAME prefix on both sides,
    /// never through `wrap_body`: `wrap_body` reads a marker column off the
    /// first word, so a sentence opening with a one-column word ("A reusable
    /// unit of…") would hang its continuations two columns in as though the
    /// `A` were a glyph.
    ///
    /// Wrapped first and painted after, one physical line at a time (the
    /// shape `render_note` uses): every row then opens and closes its own
    /// style run, instead of leaving one run hanging open across the rows
    /// between the first and the last.
    pub(crate) fn render_paragraph(&mut self, depth: usize, text: &str) {
        let folded = cursor_safe(text);
        if folded.trim().is_empty() {
            return;
        }
        let prefix = self.open_aligned_block(depth, None);
        // Every row `wrap_segment` returns opens with exactly this prefix, and
        // it is ASCII spaces, so the split is by byte length.
        let indent = prefix.len();
        for logical in folded.split('\n') {
            // A blank line separates paragraphs; indenting it would leave
            // trailing whitespace with nothing under it.
            if logical.trim().is_empty() {
                self.out.push(String::new());
                continue;
            }
            for physical in wrap::wrap_segment(logical, &prefix, &prefix, self.wrap_cols) {
                let (lead, body) = physical.split_at(indent);
                self.out
                    .push(format!("{lead}{}", self.theme.muted.apply_to(body)));
            }
        }
        self.mark_top_level_group(TopGroup::Paragraph);
    }

    /// Collect a pre-built raw block (diff, syntax-highlighted code) at
    /// `depth`. Each entry in `lines` is already ONE complete rendered line —
    /// unlike `push_line`, nothing here word-wraps it: a diff row or a
    /// highlighted source line broken mid-token by `wrap::wrap_body` is no
    /// longer the content it was built from, which is the exemption
    /// `emit_raw_block`'s own doc names. Indentation is not part of that
    /// exemption — a raw block still nests under whatever section opened it,
    /// so it flushes any deferred section header first (same as every other
    /// emission) and prepends `depth`'s indent to each line.
    pub(crate) fn push_raw_block(&mut self, depth: usize, lines: &[String]) {
        self.flush_section_headers();
        self.drain_buffers();
        if self.state.leading {
            self.state.leading = false;
            self.state.blank_pending = false;
        } else if self.state.blank_pending {
            self.out.push(String::new());
            self.state.blank_pending = false;
        }
        self.state.last_was_top_heading = false;
        self.state.top_heading_scope = false;
        let prefix = indent_prefix(depth);
        for line in lines {
            self.out.push(format!("{prefix}{line}"));
        }
        self.mark_top_level_group(TopGroup::CodeBlock);
    }

    /// Render the buffered kvs, if any, as one aligned block.
    ///
    /// The `std::mem::take` runs BEFORE rendering, which is what terminates the
    /// recursion back through `render_kv_block_anchored` →
    /// `open_aligned_block` → `flush_section_headers`: the re-entered drain
    /// finds an empty buffer, and the frames that call marked `header_emitted`
    /// have no header left to collect either.
    ///
    /// Where the block lands RELATIVE to a deferred header is not decided here.
    /// `section::Emitting::push_deferred_headers` partitions the headers at the
    /// anchor's depth and calls this between the two halves, so rows written at
    /// the top level render above the section that opened after them while rows
    /// written inside a section render under its header.
    pub(crate) fn drain_kv_buffer(&mut self) {
        if self.state.kv_buffer.is_empty() {
            return;
        }
        let pairs = std::mem::take(&mut self.state.kv_buffer);
        let anchor = self.state.kv_anchor.take();
        let depth = anchor.map_or(self.state.indent_depth, |a| a.depth);
        self.render_kv_block_anchored(depth, &pairs, anchor);
    }

    pub(crate) fn open_top_group(&mut self, kind: TopGroup) {
        self.state.open_top_group(kind);
    }

    pub(crate) fn mark_top_level_group(&mut self, kind: TopGroup) {
        self.state.mark_top_level_group(kind);
    }

    pub(crate) fn mark_group_written_at_top_level(&mut self, kind: TopGroup, at_top_level: bool) {
        self.state
            .mark_group_written_at_top_level(kind, at_top_level);
    }

    pub(crate) fn clear_blank_pending(&mut self) {
        self.state.clear_blank_pending();
    }

    pub(crate) fn mark_top_heading(&mut self) {
        self.state.mark_top_heading();
    }
}

/// Enables depth inheritance for the renderer it was taken from, for as long
/// as it lives.
#[must_use = "depth inheritance ends when the guard drops; bind it"]
pub struct DepthInheritGuard<'p> {
    renderer: std::sync::Arc<Renderer>,
    _phantom: std::marker::PhantomData<&'p ()>,
}

impl DepthInheritGuard<'_> {
    /// Both halves of the count live on the guard, so the increment cannot be
    /// written without the decrement that pairs with it.
    pub(crate) fn acquire(renderer: &std::sync::Arc<Renderer>) -> Self {
        renderer.inherit_guards.fetch_add(1, Relaxed);
        Self {
            renderer: renderer.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl Drop for DepthInheritGuard<'_> {
    fn drop(&mut self) {
        self.renderer.inherit_guards.fetch_sub(1, Relaxed);
    }
}

/// The report-wide alignment column, held for as long as the report renders.
///
/// Acquired through [`super::Printer::report_column`]. A guard that found a
/// column already claimed carries nothing and releases nothing at drop, so the
/// outermost claim — the one made by whoever could see the whole report —
/// survives every nested surface that measures only its own part of it.
pub struct ReportColumnGuard<'p> {
    renderer: Option<std::sync::Arc<Renderer>>,
    _phantom: std::marker::PhantomData<&'p ()>,
}

impl ReportColumnGuard<'_> {
    pub(crate) fn acquire(
        renderer: &std::sync::Arc<Renderer>,
        width: usize,
        budget: Option<usize>,
    ) -> Self {
        Self {
            renderer: renderer
                .claim_report_column(width, budget)
                .then(|| renderer.clone()),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl Drop for ReportColumnGuard<'_> {
    fn drop(&mut self) {
        if let Some(renderer) = &self.renderer {
            renderer.release_report_column();
        }
    }
}

/// Sink for one rendered line. Production = stderr Term; tests = string buffer.
pub trait Writer: Send + Sync {
    fn write_line(&self, text: &str);

    /// Columns at which this sink hard-wraps, or `None` when it does not wrap
    /// at all. Only a terminal answers; a buffer or a redirected stream keeps
    /// the default so its physical lines are exactly what the renderer emitted.
    fn wrap_columns(&self) -> Option<usize> {
        None
    }

    /// Hide or show the cursor of whatever this sink writes to. Only a
    /// terminal has one; a buffer or a redirected stream keeps the no-op, so a
    /// capture never carries the escape and `-o json` never sees it.
    fn set_cursor_visible(&self, _visible: bool) {}
}

impl Writer for console::Term {
    fn write_line(&self, text: &str) {
        let _ = console::Term::write_line(self, text);
    }

    fn wrap_columns(&self) -> Option<usize> {
        self.size_checked().map(|(_, cols)| cols as usize)
    }

    fn set_cursor_visible(&self, visible: bool) {
        if visible {
            super::cursor::show(self);
        } else {
            super::cursor::hide(self);
        }
    }
}

pub struct StringSink(pub std::sync::Arc<std::sync::Mutex<String>>);
impl Writer for StringSink {
    fn write_line(&self, text: &str) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.push_str(text);
        g.push('\n');
    }
}

/// A capture sink that hard-wraps, the way a terminal does. `StringSink`
/// answers `None` to `wrap_columns`, so any claim about WRAPPING is only ever
/// provable against one of these — which is also why a golden capture never
/// re-wraps on a narrower runner. Lives here rather than in one test module
/// because both surfaces that wrap (`status`, `kv`) need the same sink.
#[cfg(test)]
pub(crate) struct NarrowSink(pub(crate) StringSink, pub(crate) usize);

#[cfg(test)]
impl Writer for NarrowSink {
    fn write_line(&self, text: &str) {
        self.0.write_line(text);
    }

    fn wrap_columns(&self) -> Option<usize> {
        Some(self.1)
    }
}

impl Renderer {
    /// Emit a single physical line at the given depth, honoring blank-pending.
    /// One emission: the kv drain, the blank line and the wrapped physicals
    /// all leave under one state-lock acquisition and one `emit_block`.
    pub(crate) fn write_line(&self, w: &dyn Writer, depth: usize, body: &str) {
        self.emit_with(w, |e| e.push_line(depth, body));
    }

    /// Mark that the next non-blank emission should be preceded by exactly
    /// one blank line. Called by Section close.
    pub(crate) fn mark_blank_pending(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.blank_pending = true;
        // A section boundary always separates: whatever follows starts a new
        // group even if it is the same kind as what preceded the section.
        s.last_top_group = None;
    }

    /// Set blank-pending iff we're at the root group level (no open section).
    /// Called at the end of every top-level group emission (heading, kv_block,
    /// status, hint, note, table) so the next top-level emit gets one blank.
    /// One blank line precedes every top-level GROUP after the first —
    /// `open_top_group` decides what continues a group rather than starting one.
    pub(crate) fn mark_top_level_group(&self, kind: TopGroup) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_top_level_group(kind);
    }

    /// Enter buffered `Doc` rendering. Paired with `exit_doc`; nests because a
    /// Doc may render a nested Doc through a component.
    pub(crate) fn enter_doc(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.doc_depth += 1;
    }

    /// Leave buffered `Doc` rendering.
    pub(crate) fn exit_doc(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.doc_depth = s.doc_depth.saturating_sub(1);
    }

    /// Drop the pending blank when this emission continues the previous group
    /// rather than starting a new one. Call before writing, from every
    /// top-level emitter.
    pub(crate) fn open_top_group(&self, kind: TopGroup) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open_top_group(kind);
    }

    /// Heading: bold styled by Theme::header. No `=== ===` decoration. Always depth 0.
    pub fn render_heading(&self, w: &dyn Writer, text: &str) {
        let styled = self.theme.header.apply_to(cursor_safe(text)).to_string();
        self.render_heading_styled(w, &styled);
    }

    /// The same top-level heading slot, for a caller that already composed
    /// its own styled string — [`super::TitleLabel`]'s 3-slot `Label: value`
    /// render, where the single `theme.header` coat `render_heading` applies
    /// would repaint over the colon/value slots the caller already styled.
    pub(crate) fn render_heading_styled(&self, w: &dyn Writer, styled: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.emit_with(w, |e| {
            e.open_top_group(TopGroup::Heading);
            e.push_line(0, styled);
            // The heading-just-emitted flag is armed AFTER the line, which
            // clears it. The next top-level kv_block consumes it to re-anchor
            // itself at depth+1 so it visually nests under the heading.
            e.mark_top_heading();
            e.mark_top_level_group(TopGroup::Heading);
        });
    }

    /// Bullet: glyph `-`, then space, then text. Uncolored except for an
    /// optional leading styled `marker` (`run PreApply script: <body>`), the
    /// bullet counterpart of a status line's marker slot — `marker` composes
    /// through `finalize_subject` exactly as a status line's marker does, so
    /// a planned script's marker in the preview tree carries the same
    /// `Role::Accent` styling it gets once the script actually runs
    /// (`StatusBuilder::marker`) instead of reading as plain body text. The
    /// renderer's only bullet glyph; `+`/`~`/`>`/`*` are forbidden.
    pub fn render_bullet(
        &self,
        w: &dyn Writer,
        depth: usize,
        text: &str,
        marker: Option<&crate::output::component::StatusLabel>,
        detail: Option<&str>,
    ) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let subject = finalize_subject(&self.theme, text, marker, None, None);
        let detail = detail.map(|d| self.theme.muted.apply_to(cursor_safe(d)).to_string());
        self.emit_with(w, |e| {
            // The dash is structure, the text is content: muting it gives a run
            // of bullets a scan column instead of leaving every character on
            // the line at the terminal's default with nothing to read against.
            // A detail joins after the same em-dash a status row's does, muted
            // the way a produced count is on the apply tree — and, like a status
            // row's, padded to the column the report claimed, so a bullet's dash
            // and a status row's land at one x position.
            let padded = detail
                .as_ref()
                .and_then(|_| e.bullet_column(depth))
                .and_then(|column| status::pad_subject(&subject, column, true));
            let mut body = format!(
                "{}{}",
                e.theme.muted.apply_to("- "),
                padded.as_deref().unwrap_or(&subject)
            );
            if let Some(detail) = &detail {
                body.push_str(" — ");
                body.push_str(detail);
            }
            e.flush_section_headers();
            e.open_top_group(TopGroup::Bullet);
            e.push_line(depth, &body);
            e.mark_top_level_group(TopGroup::Bullet);
        });
    }

    /// A deploy row's per-file child: a target and its resolved method,
    /// `— method` muted after the em-dash like every other detail slot.
    ///
    /// Renders with no glyph, one depth below its parent (see
    /// `Emitting::child_row_column` for why the trailing marker still lands
    /// at the report's one claimed column), and continues the parent row's
    /// group rather than opening one of its own —
    /// the same shape [`Self::render_stream_line`] takes for a child process's
    /// captured output, because a child row belongs to the action above it,
    /// not to a block of its own.
    pub fn render_child_row(&self, w: &dyn Writer, depth: usize, target: &str, method: &str) {
        self.render_child_row_labeled(w, depth, target, method, None);
    }

    /// [`Self::render_child_row`] with the trailing styled label slot filled —
    /// the same at-end-of-subject composition a Status row's `label` takes
    /// (via `finalize_subject`), for a nested finding annotated with the
    /// source that declared it. The subject is folded and sanitized FIRST so
    /// the renderer-owned label styling survives sanitation.
    pub fn render_child_row_labeled(
        &self,
        w: &dyn Writer,
        depth: usize,
        target: &str,
        method: &str,
        label: Option<&crate::output::component::StatusLabel>,
    ) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let target = crate::fold_home_in_text(target);
        let subject = finalize_subject(&self.theme, &target, None, None, label);
        let method = self.theme.muted.apply_to(cursor_safe(method)).to_string();
        self.emit_with(w, |e| {
            let padded = e
                .child_row_column(depth)
                .and_then(|column| status::pad_subject(&subject, column, true));
            let mut body = padded.unwrap_or_else(|| subject.to_string());
            body.push_str(" — ");
            body.push_str(&method);
            e.flush_section_headers();
            e.clear_blank_pending();
            e.push_line(depth, &body);
            e.mark_top_level_group(TopGroup::Status);
        });
    }

    /// One line of live output from a child process, rendered dim and indented.
    /// Unlike a spinner message — which repaints a fixed window in place and so
    /// erases and rewrites the lines above it — this appends, letting output
    /// scroll exactly as it would in a bare terminal with the cursor resting on
    /// the last line. It is the BODY of the status group that announced the
    /// command: it opens no group of its own, so consecutive lines never get a
    /// blank between them, and it closes as that group, so the sibling block
    /// after the last line gets the one blank every top-level boundary gets.
    pub fn render_stream_line(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let body = self.theme.muted.apply_to(cursor_safe(text)).to_string();
        self.emit_with(w, |e| {
            e.flush_section_headers();
            // Streamed output continues the announcing line's group rather
            // than starting one: without this the status line's pending blank
            // lands between the announcement and the first line of its output.
            e.clear_blank_pending();
            e.push_line(depth, &body);
            // …and re-arms the boundary it just consumed. Clearing without
            // re-arming left a section header or a heading written after git's
            // last passthrough line glued to it (`Cloning into …` / `Plan`),
            // while the same header after a plain status row got its blank. The
            // announcing line's own finish (`✓ Cloned …`) still binds: a status
            // continues a status group.
            e.mark_top_level_group(TopGroup::Status);
        });
    }

    /// Hint: arrow glyph + dim text. Shown at Normal+ (NOT Quiet). The
    /// canonical "next step" surface.
    ///
    /// `commands` drops the hint's payload onto its own indented `$ ` lines
    /// (see [`crate::output::HintCommands`]) — the ONE place that indent and
    /// that prompt are spelled, so no call site hand-builds either and the
    /// home fold below reaches the block lines too. A block line is pushed
    /// UNWRAPPED, because the bytes are the promise: a hard wrap would splice
    /// a newline and a hang indent into the command the reader copies.
    ///
    /// The ONE seam every hint reaches — `Printer::hint`, `SectionGuard::hint`
    /// and the `Component::Hint` a `Doc` / `SectionBuilder` carries all render
    /// here — so a path under home folds to `~/` for the whole class at once,
    /// and a composer cannot ship a hint that spells `$HOME` differently from
    /// the rows above it. Folded HERE rather than at the composers because a
    /// `Doc` with no `with_data` serializes its `Component::Hint` text into the
    /// Doc-derived payload, which keeps the absolute path a script can `cat`.
    ///
    /// Also the ONE seam `spec.usageHints: false` / `CFGD_USAGE_HINTS=false` /
    /// `--no-hints` suppresses through: the early return below fires before
    /// `open_top_group` arms the leading blank line a hint would otherwise
    /// own, so turning hints off drops both the hint AND its blank line
    /// rather than leaving a bare blank behind. `note`/`deprecation`/`alert`
    /// are NOT hints and do not check this flag — they report what the run
    /// did or will do, not what to run next, and stay visible with hints off.
    pub fn render_hint(&self, w: &dyn Writer, depth: usize, text: &str, commands: &[String]) {
        if self.verbosity == Verbosity::Quiet || !self.hints_enabled() {
            return;
        }
        let arrow = self
            .theme
            .muted
            .apply_to(format!("{} ", self.theme.icon_arrow));
        let text = crate::fold_home_in_text(text);
        let body = format!("{}{}", arrow, self.theme.muted.apply_to(cursor_safe(&text)));
        // The prompt is scenery and the command is the payload, so only the
        // prompt is muted: what the reader copies reads at full weight.
        let block: Vec<String> = commands
            .iter()
            .map(|c| {
                let c = crate::fold_home_in_text(c);
                format!("{}{}", self.theme.muted.apply_to("$ "), cursor_safe(&c))
            })
            .collect();
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.open_top_group(TopGroup::Hint);
            e.push_line(depth, &body);
            for line in &block {
                e.push_line_unwrapped(depth + 1, line);
            }
            e.mark_top_level_group(TopGroup::Hint);
        });
    }

    /// Code block: a tight run of verbatim lines (e.g. a copy-pasteable YAML
    /// snippet). Shown at Normal+ like `hint` (NOT Verbose-only like `note`).
    /// Unlike `hint`, NO per-line glyph and NO blank line between rows — the
    /// block renders as one contiguous unit, with a single trailing blank set
    /// at the end (modeled on `render_kv_block_no_flush`). Each entry in `lines`
    /// must be newline-free; multi-line content is split by the caller so the
    /// `write_line` debug_assert holds.
    pub fn render_code_block(&self, w: &dyn Writer, depth: usize, lines: &[String]) {
        if self.verbosity == Verbosity::Quiet || lines.is_empty() {
            return;
        }
        let bodies: Vec<String> = lines
            .iter()
            .map(|l| self.theme.muted.apply_to(cursor_safe(l)).to_string())
            .collect();
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.open_top_group(TopGroup::CodeBlock);
            for body in &bodies {
                e.push_line(depth, body);
            }
            e.mark_top_level_group(TopGroup::CodeBlock);
        });
    }

    /// Paragraph: plain wrapped body text, no glyph and no coat of its own.
    /// Shown at Normal+ like `hint` (NOT Verbose-only like `note`).
    ///
    /// Placed through the same preamble an aligned block takes
    /// (`open_aligned_block`), so a paragraph written directly under a
    /// top-level heading nests one level beneath it with no blank line
    /// between: a description belongs to the thing the heading named, exactly
    /// as a kv block written there does.
    pub fn render_paragraph(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.emit_with(w, |e| e.render_paragraph(depth, text));
    }

    /// Note: multi-line prose. Suppressed at both Quiet and Normal; only Verbose.
    pub fn render_note(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity != Verbosity::Verbose {
            return;
        }
        let bodies: Vec<String> = cursor_safe(text)
            .lines()
            .map(|l| self.theme.muted.apply_to(l).to_string())
            .collect();
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.open_top_group(TopGroup::Note);
            for body in &bodies {
                e.push_line(depth, body);
            }
            e.mark_top_level_group(TopGroup::Note);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn fresh_renderer_at_depth_0() {
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        assert_eq!(r.state.lock().unwrap().depth(), 0);
    }

    #[test]
    fn push_pop_balances() {
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        let mut s = r.state.lock().unwrap();
        assert_eq!(s.push(), 1);
        assert_eq!(s.push(), 2);
        s.pop();
        s.pop();
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn indent_prefix_uses_two_spaces_per_level() {
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        assert_eq!(r.indent_prefix(0), "");
        assert_eq!(r.indent_prefix(1), "  ");
        assert_eq!(r.indent_prefix(3), "      ");
    }

    use std::sync::{Arc, Mutex};

    fn capture() -> (Renderer, StringSink, Arc<Mutex<String>>) {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        (r, sink, buf)
    }

    /// The design system is only real if every free-text emitter routes its
    /// line through a theme slot. "All output goes through `Printer`" checks
    /// routing, not visual identity — which is how a notice reached the
    /// terminal as bare default-coloured text sitting among themed output while
    /// passing that gate cleanly. Table body cells are the one deliberate
    /// exception: they carry caller data and take a `Role` per cell, opt-in.
    #[test]
    #[serial_test::serial]
    fn every_free_text_emitter_applies_a_theme_style() {
        use crate::output::Role;

        fn assert_styled(name: &str, emit: impl Fn(&Renderer, &StringSink)) {
            let buf = Arc::new(Mutex::new(String::new()));
            let sink = StringSink(buf.clone());
            // Verbose so `note`, which is Verbose-only, still emits.
            let r = Renderer::new(
                Theme::from_preset("dracula").with_colors(true),
                Verbosity::Verbose,
            );
            emit(&r, &sink);
            // raw-capture-ok: asserting a free-text emitter's raw output carries ANSI at all — captured_text would strip the escapes this test exists to check
            let out = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert!(
                out.contains('\u{1b}'),
                "{name} emitted unstyled text: {out:?}"
            );
        }

        assert_styled("heading", |r, s| r.render_heading(s, "h"));
        assert_styled("bullet", |r, s| r.render_bullet(s, 0, "b", None, None));
        assert_styled("stream_line", |r, s| r.render_stream_line(s, 0, "l"));
        assert_styled("hint", |r, s| r.render_hint(s, 0, "h", &[]));
        assert_styled("code_block", |r, s| {
            r.render_code_block(s, 0, &["c".to_string()])
        });
        assert_styled("note", |r, s| r.render_note(s, 0, "n"));
        assert_styled("paragraph", |r, s| r.render_paragraph(s, 0, "p"));
        assert_styled("status", |r, s| {
            r.render_status(
                s,
                0,
                &status::StatusFields {
                    role: Role::Info,
                    subject: "s",
                    detail: None,
                    duration: None,
                    target: None,
                    subject_style: None,
                    detail_style: None,
                    verdict: None,
                },
            )
        });
        assert_styled("advisory", |r, s| r.render_advisory(s, 0, "d"));
        assert_styled("table header", |r, s| {
            r.render_table(s, 0, &Table::new(["col"]))
        });
    }

    /// Streamed child output is the body of the announcement above it. A blank
    /// line between the two reads as the command producing nothing and some
    /// unrelated block following, which is exactly the seam a spinner used to
    /// hide.
    #[test]
    fn streamed_lines_bind_to_the_status_that_announced_them() {
        use crate::output::Role;
        let status = |role: Role, subject: &'static str| status::StatusFields {
            role,
            subject,
            detail: None,
            duration: None,
            target: None,
            subject_style: None,
            detail_style: None,
            verdict: None,
        };

        let (r, sink, buf) = capture();
        r.render_status(&sink, 0, &status(Role::Running, "running a script"));
        r.render_stream_line(&sink, 1, "first line of output");
        r.render_stream_line(&sink, 1, "second line of output");
        r.render_status(&sink, 0, &status(Role::Ok, "running a script"));

        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            !out.contains("\n\n"),
            "blank line inside a streamed block: {out:?}"
        );
    }

    /// The streamed lines are the announcing status's body, so the block
    /// after them is a new sibling and gets its blank — the same one it gets
    /// after a plain status row — while the announcing line's own finish
    /// binds to its output with none.
    #[test]
    fn a_block_after_streamed_output_gets_its_one_blank_line() {
        use crate::output::Role;
        let status = |role: Role, subject: &'static str| status::StatusFields {
            role,
            subject,
            detail: None,
            duration: None,
            target: None,
            subject_style: None,
            detail_style: None,
            verdict: None,
        };

        let (r, sink, buf) = capture();
        r.render_status(&sink, 0, &status(Role::Running, "Cloning source:acme"));
        r.render_stream_line(&sink, 1, "Cloning into 'acme'...");
        r.render_stream_line(&sink, 1, "done.");
        r.render_heading(&sink, "Plan");
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            out.ends_with("  done.\n\nPlan\n"),
            "the heading after the streamed body is a new sibling: {out:?}"
        );
        assert_eq!(
            out.matches("\n\n").count(),
            1,
            "one blank, at the boundary only: {out:?}"
        );

        let (r, sink, buf) = capture();
        r.render_status(&sink, 0, &status(Role::Running, "Fetching source:acme"));
        r.render_stream_line(&sink, 1, "From file:///acme");
        r.render_status(&sink, 0, &status(Role::Ok, "Updated"));
        let out = crate::test_helpers::captured_text(&buf);
        assert!(
            !out.contains("\n\n"),
            "the finish line binds to the streamed body: {out:?}"
        );
    }

    #[test]
    fn no_leading_blank() {
        let (r, sink, buf) = capture();
        r.mark_blank_pending(); // even if requested before first emit
        r.write_line(&sink, 0, "first");
        let s = crate::test_helpers::captured_text(&buf);
        assert_eq!(s, "first\n");
    }

    #[test]
    fn one_blank_between_siblings() {
        let (r, sink, buf) = capture();
        r.write_line(&sink, 0, "A");
        r.mark_blank_pending();
        r.mark_blank_pending(); // duplicate marks coalesce
        r.write_line(&sink, 0, "B");
        let s = crate::test_helpers::captured_text(&buf);
        assert_eq!(s, "A\n\nB\n");
    }

    #[test]
    fn indent_two_spaces_per_level() {
        let (r, sink, buf) = capture();
        r.write_line(&sink, 0, "root");
        r.write_line(&sink, 1, "child");
        r.write_line(&sink, 2, "grand");
        let s = crate::test_helpers::captured_text(&buf);
        assert_eq!(s, "root\n  child\n    grand\n");
    }

    /// A description belongs to the thing the heading named, so it nests one
    /// level under it with no blank between — the same binding a kv block
    /// written there gets. The block that FOLLOWS the description is still the
    /// heading's, so it nests too and separates as its own group: the indent
    /// is a fact about scope, the blank line a fact about adjacency. Before
    /// the two were split, `cfgd explain` printed its prose indented and its
    /// `Location` / `Docs` rows at column 0 under the same heading.
    #[test]
    fn a_paragraph_binds_to_the_heading_above_it() {
        let (r, sink, buf) = capture();
        r.render_heading(&sink, "profile.spec.packages.brew <object>");
        r.render_paragraph(&sink, 0, "Homebrew packages.");
        r.render_kv_block(&sink, 0, &[crate::output::KvPair::new("kind", "Profile")]);
        let out = crate::test_helpers::captured_text(&buf);
        assert_eq!(
            out, "profile.spec.packages.brew <object>\n  Homebrew packages.\n\n  kind  Profile\n",
            "got: {out:?}"
        );
    }

    /// Body text wraps flush to its own indent: with no marker to hang under,
    /// a continuation column of anything else would read as a nested line.
    #[test]
    fn a_wrapped_paragraph_keeps_every_line_in_one_column() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = NarrowSink(StringSink(buf.clone()), 30);
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        // A one-column first word is what `wrap_body` reads as a glyph, and
        // every production description opens with one ("A reusable unit of…").
        r.render_paragraph(&sink, 1, "A bravo charlie delta echo foxtrot golf");
        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() > 1, "expected a wrap at 30 columns: {out:?}");
        for line in &lines {
            assert!(
                line.starts_with("  ") && !line.starts_with("   "),
                "line off the paragraph's column: {line:?}"
            );
        }
    }

    /// A blank interior line is a paragraph break, and the row carrying it has
    /// no prefix to split off — the reason the layout runs per logical line.
    #[test]
    fn a_paragraph_break_survives_an_indented_paragraph() {
        let (r, sink, buf) = capture();
        r.render_paragraph(&sink, 1, "first\n\nsecond");
        let out = crate::test_helpers::captured_text(&buf);
        assert_eq!(out, "  first\n\n  second\n", "got: {out:?}");
    }

    /// The slot owns the invariant, not one builder: an empty description
    /// renders nothing rather than a line of indent.
    #[test]
    fn an_empty_paragraph_renders_nothing() {
        let (r, sink, buf) = capture();
        r.render_paragraph(&sink, 1, "   ");
        assert_eq!(crate::test_helpers::captured_text(&buf), "");
    }

    #[test]
    fn heading_renders_at_depth_zero() {
        let (r, sink, buf) = capture();
        r.render_heading(&sink, "Status");
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("Status"));
        // No `=== ===` decoration.
        assert!(!s.contains("==="));
    }

    #[test]
    fn heading_suppressed_when_quiet() {
        let (r_default, _, _) = capture();
        drop(r_default);
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_heading(&sink, "Status");
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    /// A heading introduces what follows it, for EVERY group kind — the rule
    /// `open_top_group` enforces. Each arm renders the emission a `TopGroup`
    /// variant stands for directly under a heading; a kind whose renderer
    /// forgets to open its group renders one line low and fails here.
    #[test]
    fn every_group_kind_renders_directly_under_the_heading_that_introduces_it() {
        type Emit = fn(&Renderer, &dyn Writer);
        // Exhaustive: a new `TopGroup` variant fails to compile until it names
        // the emission it stands for (or says why a heading cannot precede it).
        fn emitter(kind: TopGroup) -> Option<Emit> {
            match kind {
                // Two consecutive headings are two groups, by construction.
                TopGroup::Heading => None,
                TopGroup::Status => Some(|r, w| {
                    r.render_status(
                        w,
                        0,
                        &StatusFields {
                            role: crate::output::Role::Ok,
                            subject: "Synced",
                            detail: None,
                            duration: None,
                            target: None,
                            subject_style: None,
                            detail_style: None,
                            verdict: None,
                        },
                    )
                }),
                TopGroup::Hint => Some(|r, w| r.render_hint(w, 0, "run cfgd apply", &[])),
                TopGroup::Bullet => Some(|r, w| r.render_bullet(w, 0, "item", None, None)),
                TopGroup::CodeBlock => {
                    Some(|r, w| r.render_code_block(w, 0, &["let x = 1;".to_string()]))
                }
                TopGroup::Note => Some(|r, w| r.render_note(w, 0, "a note")),
                TopGroup::KvBlock => Some(|r, w| {
                    r.render_kv_block(w, 0, &[crate::output::KvPair::new("Config", "cfgd.yaml")])
                }),
                TopGroup::Paragraph => Some(|r, w| r.render_paragraph(w, 0, "Homebrew packages.")),
                TopGroup::Table => Some(|r, w| {
                    r.render_table(
                        w,
                        0,
                        &Table::new(vec!["Name".to_string()]).row(vec!["team".to_string()]),
                    )
                }),
            }
        }
        for kind in [
            TopGroup::Heading,
            TopGroup::Status,
            TopGroup::Hint,
            TopGroup::Bullet,
            TopGroup::CodeBlock,
            TopGroup::Note,
            TopGroup::KvBlock,
            TopGroup::Paragraph,
            TopGroup::Table,
        ] {
            let Some(emit) = emitter(kind) else {
                continue;
            };
            let (r, sink, buf) = capture();
            r.render_heading(&sink, "Sources");
            emit(&r, &sink);
            let out = crate::test_helpers::captured_text(&buf);
            assert!(
                !out.contains("\n\n"),
                "{kind:?} left a blank line under its heading: {out:?}"
            );
        }
    }

    #[test]
    fn bullet_uses_dash_glyph() {
        let (r, sink, buf) = capture();
        r.render_bullet(&sink, 1, "foo", None, None);
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("  - foo"), "got: {s:?}");
    }

    #[test]
    fn bullet_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_bullet(&sink, 1, "foo", None, None);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn hint_uses_arrow_glyph() {
        let (r, sink, buf) = capture();
        r.render_hint(&sink, 0, "run cfgd apply", &[]);
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("→"), "got: {s:?}");
        assert!(s.contains("run cfgd apply"));
    }

    /// A hint's commands drop onto their own lines, one indent below the
    /// prose, each behind a `$ ` the renderer supplies. The prose keeps the
    /// arrow; the block lines carry no glyph of their own, so the prompt
    /// column is what the eye follows down.
    #[test]
    fn a_hint_with_commands_drops_them_onto_indented_dollar_lines() {
        let (r, sink, buf) = capture();
        r.render_hint(
            &sink,
            0,
            "Make the first commit in the config directory:",
            &[
                "git add -A && git commit -m 'initial'".to_string(),
                "cfgd pull".to_string(),
            ],
        );
        let s = crate::test_helpers::captured_text(&buf);
        let rows: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            rows,
            vec![
                "→ Make the first commit in the config directory:",
                "  $ git add -A && git commit -m 'initial'",
                "  $ cfgd pull",
            ],
            "got: {s:?}"
        );
    }

    /// The home fold reaches the block lines too: a command naming a path
    /// under home must not spell it one way while the rows above spell it
    /// another.
    #[test]
    fn a_hint_command_naming_a_path_under_home_folds_it() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::with_test_home_guard(home.path());
        let (r, sink, buf) = capture();
        r.render_hint(
            &sink,
            0,
            "Stop it later, from a GUI login session:",
            &[format!(
                "launchctl bootout gui/$(id -u) {}/Library/LaunchAgents/com.cfgd.daemon.plist",
                crate::to_posix_string(home.path())
            )],
        );
        let s = crate::test_helpers::captured_text(&buf);
        assert!(
            s.contains("$ launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/"),
            "got: {s:?}"
        );
    }

    /// A block line's whole point is that what follows the `$ ` is what the
    /// reader copies, so it is the one hint line that must not hard-wrap: the
    /// 79-column `launchctl bootout` command under an 80-column terminal came
    /// back with a newline and a hang indent spliced into the middle of a path.
    /// Soft-wrapping costs a visual row and leaves the bytes alone. Only the
    /// emulated screen can answer this — it is the one capture whose sink
    /// reports a width at all.
    #[test]
    fn a_hint_command_line_is_never_hard_wrapped() {
        let cmd = "launchctl bootout gui/$(id -u) \
                   ~/Library/LaunchAgents/com.cfgd.daemon.plist";
        let (printer, screen) = crate::output::Printer::for_test_live_terminal(24, 60);
        printer.hint_commands("Stop it later, from a GUI login session:", &[cmd]);
        drop(printer);
        // Soft wrap breaks a row at the terminal's edge and resumes at column
        // zero, so the rows rejoin into the command; a hard wrap would leave
        // its hang indent behind in the middle.
        let rejoined = screen.contents().replace('\n', "");
        assert!(
            rejoined.contains(cmd),
            "the command came back broken: {:?}",
            screen.contents()
        );
    }

    /// Every hint reaches `render_hint`, so the home fold sits there rather
    /// than in each composer: a hint naming a path under home reads `~/` even
    /// when the composer interpolated an absolute one.
    #[test]
    fn a_hint_naming_a_path_under_home_folds_it() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::with_test_home_guard(home.path());
        let (r, sink, buf) = capture();
        r.render_hint(
            &sink,
            0,
            &format!(
                "chmod u+w {}/.config/cfgd",
                crate::to_posix_string(home.path())
            ),
            &[],
        );
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("chmod u+w ~/.config/cfgd"), "got: {s:?}");
    }

    #[test]
    fn code_block_renders_tight_no_arrow_no_blank_lines() {
        let (r, sink, buf) = capture();
        r.render_code_block(
            &sink,
            0,
            &[
                "spec:".to_string(),
                "  sources:".to_string(),
                "    - name: acme".to_string(),
            ],
        );
        let out = crate::test_helpers::captured_text(&buf);
        // Every YAML row present, verbatim, no `→` glyph.
        assert!(out.contains("spec:"), "got: {out:?}");
        assert!(out.contains("  sources:"), "got: {out:?}");
        assert!(out.contains("    - name: acme"), "got: {out:?}");
        assert!(
            !out.contains('→'),
            "code block must NOT prefix `→`: {out:?}"
        );
        // Tight: no blank line between rows.
        assert!(
            !out.contains("\n\n"),
            "code block rows must be contiguous: {out:?}"
        );
    }

    #[test]
    fn code_block_shown_at_normal() {
        // Unlike note (Verbose-only), a code block is visible at Normal.
        let (r, sink, buf) = capture();
        r.render_code_block(&sink, 0, &["line".to_string()]);
        assert!(
            !crate::test_helpers::captured_text(&buf).is_empty(),
            "code block visible at Normal"
        );
    }

    #[test]
    fn code_block_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_code_block(&sink, 0, &["line".to_string()]);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn note_suppressed_at_normal() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Normal);
        r.render_note(&sink, 0, "long prose");
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn note_shown_at_verbose() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Verbose);
        r.render_note(&sink, 0, "line1\nline2");
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("line1"));
        assert!(s.contains("line2"));
    }

    // --- Line routing while a bar is live ---

    /// A `TermLike` that records what indicatif drew and counts draw cycles.
    /// One `flush` is one `draw_to_term`, so the flush delta across an
    /// emission is exactly the number of `println` calls it made.
    #[derive(Debug)]
    struct RecordingTerm {
        drawn: Arc<Mutex<String>>,
        flushes: Arc<AtomicUsize>,
        /// Every write fails, standing in for a terminal that went away.
        broken: bool,
    }

    impl RecordingTerm {
        fn result(&self) -> std::io::Result<()> {
            if self.broken {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "terminal is gone",
                ))
            } else {
                Ok(())
            }
        }

        fn record(&self, s: &str) -> std::io::Result<()> {
            self.drawn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_str(s);
            self.result()
        }
    }

    impl indicatif::TermLike for RecordingTerm {
        fn width(&self) -> u16 {
            200
        }
        fn height(&self) -> u16 {
            40
        }
        fn move_cursor_up(&self, _n: usize) -> std::io::Result<()> {
            self.result()
        }
        fn move_cursor_down(&self, _n: usize) -> std::io::Result<()> {
            self.result()
        }
        fn move_cursor_right(&self, _n: usize) -> std::io::Result<()> {
            self.result()
        }
        fn move_cursor_left(&self, _n: usize) -> std::io::Result<()> {
            self.result()
        }
        fn write_line(&self, s: &str) -> std::io::Result<()> {
            self.record(s)?;
            self.record("\n")
        }
        fn write_str(&self, s: &str) -> std::io::Result<()> {
            self.record(s)
        }
        fn clear_line(&self) -> std::io::Result<()> {
            self.result()
        }
        fn flush(&self) -> std::io::Result<()> {
            self.flushes.fetch_add(1, Relaxed);
            self.result()
        }
    }

    /// A renderer wired to a MultiProgress whose draw target is a
    /// `RecordingTerm`, with one bar added and one live-bar guard held — the
    /// production shape of "a bar is on screen".
    struct BarsFixture {
        renderer: Arc<Renderer>,
        sink: StringSink,
        sink_buf: Arc<Mutex<String>>,
        drawn: Arc<Mutex<String>>,
        flushes: Arc<AtomicUsize>,
        _bar: indicatif::ProgressBar,
        live: Option<LiveBarGuard>,
    }

    impl BarsFixture {
        fn new(broken: bool) -> Self {
            let drawn = Arc::new(Mutex::new(String::new()));
            let flushes = Arc::new(AtomicUsize::new(0));
            let term = RecordingTerm {
                drawn: drawn.clone(),
                flushes: flushes.clone(),
                broken,
            };
            let multi = indicatif::MultiProgress::with_draw_target(
                indicatif::ProgressDrawTarget::term_like(Box::new(term)),
            );
            let sink_buf = Arc::new(Mutex::new(String::new()));
            let renderer = Arc::new(Renderer::with_bars(
                Theme::default(),
                Verbosity::Normal,
                multi.clone(),
                Arc::new(StringSink(sink_buf.clone())),
            ));
            // A real bar, added the way `build_spinner` adds one, so the
            // multi has something to redraw around each routed emission.
            let bar = multi.add(indicatif::ProgressBar::new_spinner());
            let live = LiveBarGuard::acquire(&renderer);
            Self {
                renderer,
                sink: StringSink(sink_buf.clone()),
                sink_buf,
                drawn,
                flushes,
                _bar: bar,
                live: Some(live),
            }
        }

        fn drawn(&self) -> String {
            strip_ansi(&self.drawn.lock().unwrap_or_else(|e| e.into_inner()))
        }

        fn sunk(&self) -> String {
            crate::test_helpers::captured_text(&self.sink_buf)
        }

        fn cycles(&self) -> usize {
            self.flushes.load(Relaxed)
        }
    }

    #[test]
    fn live_bar_routes_lines_through_the_multi() {
        let f = BarsFixture::new(false);
        f.renderer.write_line(&f.sink, 0, "routed line");
        assert!(
            f.drawn().contains("routed line"),
            "line did not reach the multi: {:?}",
            f.drawn()
        );
        assert!(
            !f.sunk().contains("routed line"),
            "line also went straight to the sink, so it would print twice: {:?}",
            f.sunk()
        );
    }

    #[test]
    fn no_bars_means_no_routing() {
        // No MultiProgress at all: the sink is the only path.
        let (r, sink, buf) = capture();
        r.write_line(&sink, 0, "plain");
        assert_eq!(crate::test_helpers::captured_text(&buf), "plain\n");

        // A MultiProgress with no LIVE bar is the same case — nothing is
        // drawn over, so there is nothing to redraw around.
        let mut f = BarsFixture::new(false);
        f.live = None;
        f.renderer.write_line(&f.sink, 0, "no bars live");
        assert!(
            f.sunk().contains("no bars live"),
            "line missed the sink: {:?}",
            f.sunk()
        );
        assert!(
            !f.drawn().contains("no bars live"),
            "line was routed with no live bar: {:?}",
            f.drawn()
        );
    }

    #[test]
    fn broken_terminal_latches_and_does_not_panic() {
        let f = BarsFixture::new(true);
        f.renderer.write_line(&f.sink, 0, "first");
        assert!(
            f.renderer.live.broken(),
            "an io error must latch the routing off"
        );
        assert!(
            f.sunk().contains("first"),
            "the emission that DISCOVERED the break was dropped: {:?}",
            f.sunk()
        );

        let before = f.cycles();
        f.renderer.write_line(&f.sink, 0, "second");
        assert!(
            f.sunk().contains("second"),
            "later emission lost: {:?}",
            f.sunk()
        );
        assert_eq!(
            f.cycles(),
            before,
            "the latch must stop later emissions from retrying the dead terminal"
        );
    }

    #[test]
    fn emit_block_is_one_println_per_emission() {
        let f = BarsFixture::new(false);

        let before = f.cycles();
        f.renderer.render_code_block(
            &f.sink,
            0,
            &["one".to_string(), "two".to_string(), "three".to_string()],
        );
        let three_lines = f.cycles() - before;

        let before = f.cycles();
        f.renderer.write_line(&f.sink, 0, "single");
        let one_line = f.cycles() - before;

        assert_eq!(
            three_lines, 1,
            "a 3-line emission drew {three_lines} times; it must clear and redraw once"
        );
        assert_eq!(one_line, 1, "a 1-line emission drew {one_line} times");
        let drawn = f.drawn();
        for line in ["one", "two", "three", "single"] {
            assert!(drawn.contains(line), "{line:?} missing from: {drawn:?}");
        }
    }

    /// An emission the renderer only BUFFERED — a status a live section holds
    /// back for column alignment, a section header deferred until its first
    /// child — produces no line, and must therefore draw nothing. Routed, an
    /// empty block joined to `""` and indicatif printed one blank row into the
    /// middle of the live region: every owner-group boundary in an apply's
    /// phase tree carried a blank line no other boundary had.
    #[test]
    fn an_emission_that_produced_no_line_draws_nothing() {
        let f = BarsFixture::new(false);
        let before = f.cycles();
        f.renderer.emit_with(&f.sink, |_| {});
        assert_eq!(
            f.cycles(),
            before,
            "a buffered emission drew: {:?}",
            f.drawn()
        );
        assert!(
            f.drawn().is_empty() && f.sunk().is_empty(),
            "a buffered emission wrote a line: drawn {:?}, sunk {:?}",
            f.drawn(),
            f.sunk()
        );
    }

    /// The latch is shared by every renderer AND by the tracing writer, so
    /// latching on a single refusal hands the whole process back to raw writes
    /// over a live region — the strand the latch exists to survive, re-entered
    /// through the failure path.
    #[test]
    fn a_transient_refusal_alone_does_not_latch_routing_off() {
        let transient = || std::io::Error::from(std::io::ErrorKind::Interrupted);
        let live = LiveBarState::new(None);

        assert!(!live.record_route_failure(&transient()));
        assert!(!live.broken(), "one refusal is not a dead terminal");

        // The region answered again, so the run of failures restarts and the
        // NEXT lone refusal must not latch either.
        live.note_route_success();
        assert!(!live.record_route_failure(&transient()));
        assert!(!live.broken(), "a success in between restarts the run");

        assert!(
            live.record_route_failure(&transient()),
            "consecutive refusals stand in for a terminal that stopped answering"
        );
        assert!(live.broken());
    }

    /// A kind that says the far end is gone will say it again on every later
    /// line, so re-probing it per line buys nothing.
    #[test]
    fn a_terminal_that_is_gone_latches_on_its_first_refusal() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::NotConnected,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let live = LiveBarState::new(None);
            assert!(
                live.record_route_failure(&std::io::Error::from(kind)),
                "{kind:?} must latch at once"
            );
            assert!(live.broken(), "{kind:?} left routing on");
        }
    }

    #[test]
    fn hidden_bars_are_not_counted() {
        // Quiet / non-TTY yields a hidden bar, which is never `multi.add`ed.
        // Counting one would open the routing gate over a multi that draws
        // nothing, and every line would vanish.
        let p = crate::output::Printer::with_format(
            Verbosity::Quiet,
            None,
            crate::output::OutputFormat::Table,
            crate::output::ColorChoice::Auto,
        );
        let sp = p.spinner("hidden");
        assert!(sp.bar.is_hidden(), "Quiet must yield a hidden bar");
        assert!(
            sp._live.is_none(),
            "a hidden bar must not hold a live-bar guard"
        );
    }

    /// The cursor rides the live-bar count: two overlapping bars hide it once,
    /// and it comes back only when the LAST of them drops. A per-bar toggle
    /// would flash the cursor back mid-region as the first bar settled.
    #[test]
    fn the_cursor_hides_on_the_first_bar_and_shows_when_the_last_drops() {
        struct CursorLog(Mutex<Vec<bool>>);
        impl Writer for CursorLog {
            fn write_line(&self, _: &str) {}
            fn set_cursor_visible(&self, visible: bool) {
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(visible);
            }
        }
        let log = Arc::new(CursorLog(Mutex::new(Vec::new())));
        let renderer = Arc::new(Renderer::with_bars(
            Theme::default(),
            Verbosity::Normal,
            indicatif::MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden()),
            log.clone(),
        ));
        let toggles = || log.0.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let first = LiveBarGuard::acquire(&renderer);
        let second = LiveBarGuard::acquire(&renderer);
        assert_eq!(toggles(), vec![false], "hidden once, on the first bar");
        drop(first);
        assert_eq!(toggles(), vec![false], "still hidden while a bar is live");
        drop(second);
        assert_eq!(
            toggles(),
            vec![false, true],
            "shown once, as the last bar drops"
        );

        // A renderer without bars has no live region and never touches it.
        let bare = Arc::new(Renderer::new(Theme::default(), Verbosity::Normal));
        drop(LiveBarGuard::acquire(&bare));
        assert_eq!(toggles(), vec![false, true]);
    }

    #[test]
    fn live_bar_count_returns_to_zero() {
        let mut f = BarsFixture::new(false);
        assert_eq!(f.renderer.live.count(), 1);
        let second = LiveBarGuard::acquire(&f.renderer);
        assert_eq!(f.renderer.live.count(), 2);
        drop(second);
        assert_eq!(f.renderer.live.count(), 1);
        f.live = None;
        assert_eq!(
            f.renderer.live.count(),
            0,
            "the guard's Drop is the only decrement, so it must fire"
        );
    }

    #[test]
    fn kv_block_travels_with_the_line_it_precedes() {
        let f = BarsFixture::new(false);
        f.renderer.render_section_open("Config", true);
        f.renderer.render_kv("Profile", "work");
        f.renderer.render_kv("Host", "jarvis");

        // The bullet is what drains the kvs and what flushes the still-
        // deferred section header. All of it is ONE emission.
        let before = f.cycles();
        f.renderer.render_bullet(&f.sink, 1, "applied", None, None);
        assert_eq!(
            f.cycles() - before,
            1,
            "header + kv block + bullet must leave as one block"
        );

        let drawn = f.drawn();
        for text in ["Config", "Profile", "Host", "applied"] {
            assert!(drawn.contains(text), "{text:?} missing from: {drawn:?}");
        }
        // The line the block precedes is last, which is what makes it one
        // block rather than a kv emission followed by a separate line.
        let bullet = drawn.find("applied").expect("bullet missing");
        for text in ["Config", "Profile", "Host"] {
            let at = drawn.find(text).expect("member missing");
            assert!(
                at < bullet,
                "{text:?} left after the line it precedes: {drawn:?}"
            );
        }
        f.renderer.render_section_close(&f.sink);
    }

    #[test]
    fn deferred_header_and_kv_emit_without_reentry() {
        // Same shape against a plain sink: the recursion
        // `flush_section_headers -> push_deferred_headers -> drain_kv_buffer ->
        // render_kv_block -> flush_section_headers` must terminate — the outer
        // call marked every frame `header_emitted` before it pushed anything,
        // so the inner one collects nothing — and land the lines in call order.
        let (r, sink, buf) = capture();
        r.render_section_open("Section", true);
        r.render_kv("Key", "value");
        r.render_bullet(&sink, 1, "child", None, None);
        r.render_section_close(&sink);

        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        // The rows were written INSIDE the section, so they belong under its
        // header — the anchor's depth is what says so, and the header flush
        // splits at it. Rows written before the section opened take the other
        // side of that split and still render above the header.
        assert_eq!(
            lines,
            vec!["Section", "  Key  value", "  - child"],
            "got: {out:?}"
        );
    }
}
