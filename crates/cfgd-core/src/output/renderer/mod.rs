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
pub(crate) use glyphs::{finalize_subject, role_glyph};
pub use status::StatusFields;
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
}

impl RenderState {
    /// The anchor a kv row written right now belongs to.
    pub(crate) fn kv_anchor_here(&self) -> KvAnchor {
        let at_top_level = self.section_stack.is_empty();
        KvAnchor {
            depth: self.indent_depth,
            at_top_level,
            bound_to_heading: at_top_level && self.indent_depth == 0 && self.last_was_top_heading,
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
            last_top_group: None,
            doc_depth: 0,
            last_top_in_doc: false,
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
    fn new() -> Self {
        Self {
            live_bars: AtomicUsize::new(0),
            bars_broken: AtomicBool::new(false),
            route_failures: AtomicUsize::new(0),
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
}

impl Renderer {
    pub fn new(theme: Theme, verbosity: Verbosity) -> Self {
        Self {
            theme,
            verbosity,
            state: Mutex::new(RenderState::new()),
            bars: None,
            live: std::sync::Arc::new(LiveBarState::new()),
            inherit_guards: AtomicUsize::new(0),
        }
    }

    /// The one production wiring: a renderer whose stderr sink is `bars`'s own
    /// draw target, so lines emitted while a bar is live can be routed through
    /// it. `pub(crate)` because that invariant is not something an external
    /// caller can be trusted to hold.
    pub(crate) fn with_bars(
        theme: Theme,
        verbosity: Verbosity,
        bars: indicatif::MultiProgress,
    ) -> Self {
        Self {
            bars: Some(bars),
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
    pub(crate) fn with_bars_continued(
        theme: Theme,
        verbosity: Verbosity,
        bars: indicatif::MultiProgress,
        seed: RenderState,
        live: std::sync::Arc<LiveBarState>,
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
        }
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
pub(crate) struct LiveBarGuard(std::sync::Arc<LiveBarState>);

impl LiveBarGuard {
    pub(crate) fn acquire(renderer: &std::sync::Arc<Renderer>) -> Self {
        renderer.live.live_bars.fetch_add(1, Relaxed);
        Self(renderer.live.clone())
    }
}

impl Drop for LiveBarGuard {
    fn drop(&mut self) {
        self.0.live_bars.fetch_sub(1, Relaxed);
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
        self.push_line_with_trailer(depth, body, None);
    }

    /// Same as [`Self::push_line`], but with `trailer` — a status line's
    /// duration suffix — anchored to the shared duration column on the LAST
    /// physical line a wrap produces (`wrap::wrap_body_with_trailer`)
    /// instead of flowing inline with the rest of the wrapped body, where a
    /// long enough subject strands it off the column every other row in the
    /// section pads its own duration to.
    pub(crate) fn push_line_with_trailer(
        &mut self,
        depth: usize,
        body: &str,
        trailer: Option<&str>,
    ) {
        self.drain_buffers();
        self.push_line_undrained(depth, body, trailer);
    }

    /// Empty both deferred buffers, oldest content first.
    ///
    /// A section's pending statuses are always older than whatever is still in
    /// the kv buffer: a status buffered while kv rows were waiting empties them
    /// on the way in ([`super::status`]'s buffered route), so at most one of
    /// the two ever holds content that predates the other. Draining them the
    /// other way round would invert exactly the call order this exists to keep.
    pub(crate) fn drain_buffers(&mut self) {
        self.drain_pending_statuses();
        self.drain_kv_buffer();
    }

    /// [`Self::push_line_with_trailer`] without the drain, for the drains
    /// themselves: a status flushed out of a section's pending buffer must not
    /// re-enter the drain, or a kv block written after it slips in first.
    pub(crate) fn push_line_undrained(&mut self, depth: usize, body: &str, trailer: Option<&str>) {
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
        // sets the flag back true after this call returns.
        self.state.last_was_top_heading = false;
        let prefix = indent_prefix(depth);
        for physical in wrap::wrap_body_with_trailer(trimmed, &prefix, self.wrap_cols, trailer) {
            self.out.push(physical);
        }
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
        let prefix = indent_prefix(depth);
        for line in lines {
            self.out.push(format!("{prefix}{line}"));
        }
        self.mark_top_level_group(TopGroup::CodeBlock);
    }

    /// Render the buffered kvs, if any, as one aligned block.
    ///
    /// The `std::mem::take` runs BEFORE rendering, which is what terminates
    /// the recursion through `flush_section_headers` → `push_line` and what
    /// keeps a pending kv block rendering above a deferred section header —
    /// the shape a block written at the top level takes when a section opens
    /// before the next emission drains it. Rows written INSIDE a section never
    /// reach that shape: `render_section_close` drains before it pops the
    /// frame, so they render under their own section's header.
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

/// Sink for one rendered line. Production = stderr Term; tests = string buffer.
pub trait Writer: Send + Sync {
    fn write_line(&self, text: &str);

    /// Columns at which this sink hard-wraps, or `None` when it does not wrap
    /// at all. Only a terminal answers; a buffer or a redirected stream keeps
    /// the default so its physical lines are exactly what the renderer emitted.
    fn wrap_columns(&self) -> Option<usize> {
        None
    }
}

impl Writer for console::Term {
    fn write_line(&self, text: &str) {
        let _ = console::Term::write_line(self, text);
    }

    fn wrap_columns(&self) -> Option<usize> {
        self.size_checked().map(|(_, cols)| cols as usize)
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
    /// through [`finalize_subject`] exactly as a status line's marker does, so
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
    ) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let subject = finalize_subject(&self.theme, text, marker, None, None);
        // The dash is structure, the text is content: muting it gives a run of
        // bullets a scan column instead of leaving every character on the
        // line at the terminal's default with nothing to read against.
        let body = format!("{}{}", self.theme.muted.apply_to("- "), subject);
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.open_top_group(TopGroup::Bullet);
            e.push_line(depth, &body);
            e.mark_top_level_group(TopGroup::Bullet);
        });
    }

    /// One line of live output from a child process, rendered dim and indented.
    /// Unlike a spinner message — which repaints a fixed window in place and so
    /// erases and rewrites the lines above it — this appends, letting output
    /// scroll exactly as it would in a bare terminal with the cursor resting on
    /// the last line. Claims no `TopGroup`: interleaving child output must not
    /// insert group-boundary blank lines between consecutive lines.
    pub fn render_stream_line(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let body = self.theme.muted.apply_to(cursor_safe(text)).to_string();
        self.emit_with(w, |e| {
            e.flush_section_headers();
            // Streamed output is the body of the line that just announced the
            // command, so it continues that group rather than starting one:
            // without this the status line's pending blank lands between the
            // announcement and the first line of its own output.
            e.clear_blank_pending();
            e.push_line(depth, &body);
        });
    }

    /// Hint: arrow glyph + dim text. Shown at Normal+ (NOT Quiet). The
    /// canonical "next step" surface.
    pub fn render_hint(&self, w: &dyn Writer, depth: usize, text: &str) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let arrow = self
            .theme
            .muted
            .apply_to(format!("{} ", self.theme.icon_arrow));
        let body = format!("{}{}", arrow, self.theme.muted.apply_to(cursor_safe(text)));
        self.emit_with(w, |e| {
            e.flush_section_headers();
            e.open_top_group(TopGroup::Hint);
            e.push_line(depth, &body);
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
        assert_styled("bullet", |r, s| r.render_bullet(s, 0, "b", None));
        assert_styled("stream_line", |r, s| r.render_stream_line(s, 0, "l"));
        assert_styled("hint", |r, s| r.render_hint(s, 0, "h"));
        assert_styled("code_block", |r, s| {
            r.render_code_block(s, 0, &["c".to_string()])
        });
        assert_styled("note", |r, s| r.render_note(s, 0, "n"));
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

    #[test]
    fn bullet_uses_dash_glyph() {
        let (r, sink, buf) = capture();
        r.render_bullet(&sink, 1, "foo", None);
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("  - foo"), "got: {s:?}");
    }

    #[test]
    fn bullet_quiet_suppressed() {
        let buf = Arc::new(Mutex::new(String::new()));
        let sink = StringSink(buf.clone());
        let r = Renderer::new(Theme::default(), Verbosity::Quiet);
        r.render_bullet(&sink, 1, "foo", None);
        assert!(crate::test_helpers::captured_text(&buf).is_empty());
    }

    #[test]
    fn hint_uses_arrow_glyph() {
        let (r, sink, buf) = capture();
        r.render_hint(&sink, 0, "run cfgd apply");
        let s = crate::test_helpers::captured_text(&buf);
        assert!(s.contains("→"), "got: {s:?}");
        assert!(s.contains("run cfgd apply"));
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
            let renderer = Arc::new(Renderer::with_bars(
                Theme::default(),
                Verbosity::Normal,
                multi.clone(),
            ));
            // A real bar, added the way `build_spinner` adds one, so the
            // multi has something to redraw around each routed emission.
            let bar = multi.add(indicatif::ProgressBar::new_spinner());
            let live = LiveBarGuard::acquire(&renderer);
            let sink_buf = Arc::new(Mutex::new(String::new()));
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

    /// The latch is shared by every renderer AND by the tracing writer, so
    /// latching on a single refusal hands the whole process back to raw writes
    /// over a live region — the strand the latch exists to survive, re-entered
    /// through the failure path.
    #[test]
    fn a_transient_refusal_alone_does_not_latch_routing_off() {
        let transient = || std::io::Error::from(std::io::ErrorKind::Interrupted);
        let live = LiveBarState::new();

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
            let live = LiveBarState::new();
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
        f.renderer.render_bullet(&f.sink, 1, "applied", None);
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
        // `push_line -> drain_kv_buffer -> render_kv_block ->
        // flush_section_headers -> push_line` must terminate and land the
        // lines in call order.
        let (r, sink, buf) = capture();
        r.render_section_open("Section", true);
        r.render_kv("Key", "value");
        r.render_bullet(&sink, 1, "child", None);
        r.render_section_close(&sink);

        let out = crate::test_helpers::captured_text(&buf);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        // Byte-identical to the sequence the scoped acquisitions produced: the
        // drain at the top of `push_line` renders the pending block before the
        // header line that triggered the drain.
        assert_eq!(
            lines,
            vec!["  Key  value", "Section", "  - child"],
            "got: {out:?}"
        );
    }
}
