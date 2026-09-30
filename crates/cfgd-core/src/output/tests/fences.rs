//! Source-shaped fences: invariants that no runtime assertion can hold,
//! because each is about code that must not exist.

use std::path::{Path, PathBuf};

use crate::test_helpers::{
    KNOWN_GOLDEN_ROOTS, blank_string_literals, carries_hatch, folded_literal_lines,
    is_plain_line_comment, item_keyword, opens_function, rust_sources_under, snapshot_golden_roots,
    snapshot_goldens, snapshot_root_files, walked_file_body, workspace_root,
};

/// Every `.rs` file under every crate's `src/`.
fn workspace_rust_files() -> Vec<PathBuf> {
    rust_sources_under(&workspace_root().join("crates"))
}

/// `MultiProgress::suspend` and `ProgressBar::suspend` both `unwrap()` an
/// `io::Result` internally, so a terminal that goes away during a suspended
/// write aborts the process. `emit_block`'s `println` route returns its error
/// instead, which is why the latch can exist at all. The one API that could
/// have re-introduced the call from outside `output/` — `Printer::multi_progress()`
/// — was deleted with this fence.
#[test]
fn suspend_is_never_called() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: a call anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        for (i, line) in body.lines().enumerate() {
            if line.contains(".suspend(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "indicatif's suspend() unwraps io errors; route through the renderer's \
         emit_block instead:\n{}",
        offenders.join("\n")
    );
}

/// The marker that exempts one wiring from the fence below, with the reason
/// written after it.
const HATCH: &str = "unfolded-writer-ok:";

/// A subscriber's writer is the whole of its sanitation: an event reaches the
/// terminal through it and through no renderer, so a writer that does not fold
/// puts a module name, a device hostname or a remote's error text on screen
/// with its control bytes live — and one written straight at the stream a live
/// region repaints also strands that region's last paint there forever.
/// `output::LiveTracingWriter` is the writer that answers both: it routes every
/// event through the printer's `MultiProgress` and folds it on the way.
///
/// The predicate is POSITIVE, and that is the whole of the design. It asks
/// whether a wiring routes through the folding writer and refuses everything
/// else, rather than listing the streams to refuse: a refusal list grown one
/// name at a time still passed `.with_writer(std::io::stdout)`, which is
/// precisely the writer a `fmt::Layer` takes when none is named. Three
/// offenses fall out of that one question — a writer that is not the folding
/// one, a construction that names no writer at all, and a folding wiring that
/// leaves the formatter's colours on (the fold strips ANSI, so they are eaten,
/// and left on they paint SGR into a redirected stream the formatter never
/// asked was a terminal).
///
/// Hatch, read like every sibling gate's (`tracing-ok:`, `native-ok:`,
/// `spawn-blocking-ok:`): mark the construction line, the writer's own line,
/// or the line above either with `// unfolded-writer-ok: <why>`. The shapes
/// that need it are a writer that is no terminal at all (a log file, a test
/// capture) and a formatter whose own serializer is the sanitizer — a JSON log
/// line, where folding would emit `\xNN` inside a string and cost every
/// consumer a parseable payload.
#[test]
fn every_subscriber_writes_through_a_folding_writer() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: a subscriber anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        for (line_no, why) in unfolded_subscriber_offenders(&body) {
            let line = body.lines().nth(line_no).unwrap_or_default();
            offenders.push(format!(
                "{}:{}: {why}: {}",
                path.display(),
                line_no + 1,
                line.trim()
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "every tracing subscriber writes through output::LiveTracingWriter with \
         the formatter's colours off (or carries `// {HATCH} <why>`):\n{}",
        offenders.join("\n")
    );
}

/// The marker that exempts one wiring from the fence below.
const UNSTAMPED_HATCH: &str = "unstamped-log-ok:";

/// A log line with no clock on it cannot answer the question a log exists to
/// answer.
///
/// Both cfgd entry points used to drop the stamp — the justification being that
/// a one-shot command's warning is read the instant it appears. But the same
/// subscriber serves `cfgd daemon run >> daemon.log`, whose whole subject is a
/// cadence: a reconcile every 30s and a sync every 5s, in a file where no
/// elapsed time was representable and a completed tick could not be told from a
/// hung one. The two are one wiring, so the stamp is not optional on either;
/// [`super::super::LocalTimeOfDay`] is the one both take.
///
/// The fence is on `.without_time()` rather than on the presence of a timer:
/// every `tracing_subscriber::fmt` wiring stamps by default, so dropping the
/// stamp takes a deliberate call, and that call is the whole population. Hatch
/// with `// unstamped-log-ok: <why>` for a sink whose own envelope already
/// carries the time.
#[test]
fn no_subscriber_drops_its_timestamp() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: a subscriber anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if code_half(line).contains("without_time(") && !hatched(&lines, i, UNSTAMPED_HATCH) {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a subscriber that drops its timestamp leaves a log that cannot date its \
         own events; take output::LocalTimeOfDay (or carry `// {UNSTAMPED_HATCH} <why>`):\n{}",
        offenders.join("\n")
    );
}

/// Whether `code` opens a `tracing_subscriber::fmt` subscriber or layer. Lists
/// every construction spelling `tracing-subscriber`'s public API and this
/// workspace's own audits have turned up so far — not a claim of exhaustive
/// coverage, since a spelling this list has not seen is still caught by the
/// writer it names: the argument pass below reads every `with_writer(` and
/// `map_writer(` wherever either stands, independent of whether this function
/// recognized the construction in front of it. Turbofish generics are
/// stripped first (`strip_turbofish`), so `fmt::Layer::<S>::default()` reaches
/// the same arm as `fmt::Layer::default()`.
fn opens_subscriber(code: &str) -> bool {
    const SPELLINGS: [&str; 11] = [
        "tracing_subscriber::fmt(",
        // Bare, so an imported `fmt` module reaches the same arm as the
        // fully-qualified path that contains it.
        "fmt::layer(",
        "fmt::init(",
        "fmt::try_init(",
        "fmt::fmt(",
        "fmt::Layer::new(",
        "fmt::Layer::default(",
        "fmt::Subscriber::new(",
        "fmt::Subscriber::default(",
        "fmt::Subscriber::builder(",
        "FmtSubscriber::builder(",
    ];
    let code = strip_turbofish(code);
    SPELLINGS.iter().any(|spelling| code.contains(spelling))
}

/// Remove every `::<…>` turbofish from `code`, collapsing `fmt::Layer::<S>::default(`
/// to `fmt::Layer::default(` so a generic parameter cannot hide a construction
/// spelling from the substring list above. Brace-balanced rather than a single
/// close-angle search, so a turbofish nesting another generic
/// (`::<Foo<Bar>>`) still collapses to its outer close.
fn strip_turbofish(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c == ':' && chars.peek().map(|&(_, c)| c) == Some(':') {
            let mut lookahead = chars.clone();
            lookahead.next();
            if lookahead.peek().map(|&(_, c)| c) == Some('<') {
                lookahead.next();
                let mut depth = 1i32;
                for (_, c) in lookahead.by_ref() {
                    match c {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                // Nothing pushed for the turbofish itself: the `::` that
                // follows its close (already the next thing `lookahead`
                // points at) is the real path separator and survives on its
                // own in the next iteration.
                chars = lookahead;
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Whether the `with_writer`/`map_writer` argument `arg`, called on
/// `lines[at]`, names the folding writer: the type itself, or a binding whose
/// initializer names it — resolved through the LAST matching `let` AT OR
/// BEFORE `at`, not any matching `let` anywhere in the file, so a later
/// binding that shadows an earlier folding one is not mistaken for it (the
/// two CLI entry points hand the subscriber a `tracing_writer.clone()` bound
/// earlier, and accepting that identifier on its NAME alone would accept any
/// writer somebody later bound to it, shadow included).
fn names_folding_writer(lines: &[&str], at: usize, arg: &str) -> bool {
    let squashed: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
    if squashed.contains("LiveTracingWriter") {
        return true;
    }
    let root = squashed
        .split(['.', '(', ')', ',', '&'])
        .find(|part| !part.is_empty())
        .unwrap_or_default();
    if root.is_empty() {
        return false;
    }
    let bindings = [format!("let {root} "), format!("let mut {root} ")];
    lines[..=at]
        .iter()
        .rev()
        .find_map(|line| {
            let code = code_half(line);
            bindings
                .iter()
                .any(|b| code.contains(b.as_str()))
                .then(|| code.contains("LiveTracingWriter"))
        })
        .unwrap_or(false)
}

/// Line numbers (0-based) of every wiring in `source` that does not put its
/// events through the folding writer, each paired with what it gets wrong.
///
/// Two passes, because either half alone leaves a way through. The first reads
/// every `with_writer(` and `map_writer(` wherever either stands — `map_writer`
/// can swap an otherwise-folding wiring's writer for something else after
/// `with_writer` already named the folding one — so a construction spelling
/// this file does not recognize is still judged by the writer it names. The
/// second reads each recognized construction to its terminating `;` —
/// rustfmt splits a builder chain across lines, and a line-scoped read would
/// judge the constructor alone — and catches the wiring that names no writer
/// at all, plus the folding wiring that left the formatter's colours on.
fn unfolded_subscriber_offenders(source: &str) -> Vec<(usize, &'static str)> {
    const MAX_LINES: usize = 16;
    let lines: Vec<&str> = source.lines().collect();
    let mut offenders = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let code = code_half(line);
        for needle in ["with_writer(", "map_writer("] {
            let Some(pos) = code.find(needle) else {
                continue;
            };
            let arg = writer_argument(&lines, i, pos + needle.len());
            if !names_folding_writer(&lines, i, &arg) && !hatched(&lines, i, HATCH) {
                offenders.push((i, "the writer named here does not fold"));
            }
        }
    }

    for (i, line) in lines.iter().enumerate() {
        if !opens_subscriber(&code_half(line)) {
            continue;
        }
        let mut writer: Option<bool> = None;
        let mut writer_line = i;
        let mut test_writer = false;
        let mut colours_off = false;
        for (offset, candidate) in lines[i..].iter().take(MAX_LINES).enumerate() {
            let chain = code_half(candidate);
            test_writer |= chain.contains("with_test_writer(");
            colours_off |= chain.contains("with_ansi(false)");
            if let Some(pos) = chain.find("with_writer(") {
                let arg = writer_argument(&lines, i + offset, pos + "with_writer(".len());
                writer = Some(names_folding_writer(&lines, i + offset, &arg));
                writer_line = i + offset;
            }
            if chain.contains(';') {
                break;
            }
        }
        // `with_test_writer` is the harness's own capture, named by the crate
        // rather than by an expression this file would have to interpret.
        if test_writer {
            continue;
        }
        let excused = hatched(&lines, i, HATCH) || hatched(&lines, writer_line, HATCH);
        match writer {
            None if !excused => {
                offenders.push((i, "no writer named, so the raw stream carries it"))
            }
            // Not hatchable: with the fold in front of it, a formatter's colour
            // has no destination to reach, so there is no wiring that wants it.
            Some(true) if !colours_off => {
                offenders.push((i, "the folding writer needs `.with_ansi(false)`"))
            }
            _ => {}
        }
    }
    offenders
}

/// The fence is only as wide as its predicate, and a predicate that recognizes
/// one spelling of a mistake is a fence somebody walks around without
/// noticing. Every way the workspace could reach a terminal unfolded is pinned
/// here, together with the wirings that are correct and the hatch. The first
/// four offenders are the ones a stream-name refusal list passed.
#[test]
fn the_folding_writer_fence_recognizes_every_spelling() {
    for offender in [
        // The raw stream, named. `Layer::default` uses stdout, so the stream a
        // refusal list forgot is the one a forgotten wiring lands on.
        "        .with_writer(std::io::stdout)",
        "        .with_writer(|| io::stdout())",
        "        .with_writer(std::io::stderr)",
        // A writer that is neither the stream nor the folding one.
        "        .with_writer(RawTerminal::new())",
        // Constructions that name no writer at all, in every spelling.
        "    tracing_subscriber::fmt().json().init();",
        "    tracing_subscriber::fmt::init();",
        "    fmt::init();",
        "    let l = tracing_subscriber::fmt::layer();",
        "    let l = fmt::layer();",
        "    let l = fmt::Layer::new();",
        "    let s = FmtSubscriber::builder().finish();",
        "    let s = fmt::Subscriber::builder().finish();",
        "    tracing_subscriber::fmt::try_init();",
        "    fmt::fmt().init();",
        "    fmt::Subscriber::new();",
        "    fmt::Subscriber::default();",
        // A turbofish on the construction cannot hide it from the spelling
        // list — `strip_turbofish` collapses it before matching.
        "    fmt::Layer::<S>::default();",
        // A chain split over lines, with the writer named nowhere in it.
        "    tracing_subscriber::fmt()\n        .with_target(false)\n        .without_time()\n        .init();",
        // A later statement's writer does not cover this one.
        "    tracing_subscriber::fmt().init();\n    other.with_writer(x);",
        // The folding writer with the formatter's colours left on.
        "    let w = LiveTracingWriter::new();\n    tracing_subscriber::fmt().with_writer(w.clone()).init();",
        // A binding that merely CARRIES the expected name is not the writer.
        "    let tracing_writer = std::io::stdout;\n    fmt::layer().with_writer(tracing_writer.clone());",
        // A shadowing `let` of the same name after the real binding is not
        // the writer either — resolution takes the LAST matching `let` at or
        // before the call, not the first one found anywhere in the file.
        "    let tracing_writer = LiveTracingWriter::new();\n    let tracing_writer = std::io::stdout;\n    fmt::layer().with_ansi(false).with_writer(tracing_writer.clone());",
        // `map_writer` can swap an otherwise-folding wiring's writer for
        // something else after `with_writer` already named the folding one.
        "    tracing_subscriber::fmt()\n        .with_ansi(false)\n        .with_writer(LiveTracingWriter::new())\n        .map_writer(|_| std::io::stdout());",
        // A marker with no reason after it is not a hatch.
        "    // unfolded-writer-ok:\n    tracing_subscriber::fmt().init();",
        // The marker inside a string literal is not a hatch either.
        "        .with_writer(io::stderr).named(\"// unfolded-writer-ok: no\")",
        // A `//` inside a string literal is not a comment: neither a URL
        // before the call nor one inside its arguments hides the wiring.
        "        connect(\"https://x.test//hook\").with_writer(io::stderr)",
        "        .with_writer(tee(\"https://x.test//hook\", io::stdout))",
    ] {
        assert!(
            !unfolded_subscriber_offenders(offender).is_empty(),
            "the fence must recognize this wiring: {offender:?}"
        );
    }
    for allowed in [
        "    tracing_subscriber::fmt()\n        .with_ansi(false)\n        .with_writer(LiveTracingWriter::new())\n        .init();",
        // The binding both CLI entry points use, resolved through its
        // initializer rather than through its name.
        "    let tracing_writer = cfgd_core::output::LiveTracingWriter::new();\n    tracing_subscriber::fmt()\n        .with_ansi(false)\n        .with_writer(tracing_writer.clone())\n        .init();",
        "    tracing_subscriber::fmt().with_test_writer().finish();",
        // The hatch, on the construction line, on the writer's line, and on
        // the line above either.
        "    // unfolded-writer-ok: the JSON serializer is the sanitizer here\n    tracing_subscriber::fmt().json().init();",
        "    fmt::layer()\n        .with_writer(Mutex::new(file)) // unfolded-writer-ok: a log file, not a terminal",
        "    fmt::layer()\n        // unfolded-writer-ok: a log file, not a terminal\n        .with_writer(Mutex::new(file));",
        // The constructor named inside a string literal is a name, not a call.
        "    let s = \"tracing_subscriber::fmt()\";",
        // A `with_writer` this file never sees the construction of is still
        // judged by its argument, and this one is the folding writer.
        "        .with_writer(LiveTracingWriter::new())",
    ] {
        assert!(
            unfolded_subscriber_offenders(allowed).is_empty(),
            "the fence must not flag this: {allowed:?}"
        );
    }
}

/// Whether this code line DECLARES a type: the keyword
/// [`crate::test_helpers::item_keyword`] reads off it is `struct`, `enum`,
/// `union`, `trait` or `impl`.
///
/// A roster needle naming a fixture TYPE matches its `struct` line and every
/// `impl` block written for it as readily as the fixtures that construct it,
/// and those lines sit outside every function, where no attribute can be hung
/// on them. Skipping them is what lets a needle be spelled as the type is
/// really written. A needle narrowed until it dodges them misses a
/// construction with it.
fn declares_a_type(code: &str) -> bool {
    matches!(
        crate::test_helpers::item_keyword(code),
        "struct" | "enum" | "union" | "trait" | "impl"
    )
}

/// The code half of a line: what is left once its comments are gone, judged
/// on the literal-blanked line so a `//` inside a string (a URL in an
/// argument) cannot truncate the code half, and so parens or the word
/// `stderr` inside a literal cannot join a call's argument text.
///
/// A `//` cuts the line; a `/* … */` span is BLANKED byte-for-byte instead,
/// because code follows a block comment on the same line — the crate spells
/// its inline argument names that way (`render_section_open(name,
/// /*keep_when_empty=*/ true)`). Blanking is also what keeps every byte
/// position found on this rendering indexing the raw line exactly, which the
/// argument scan relies on, and what keeps a brace between the delimiters from
/// moving [`function_source`]'s depth.
fn code_half(line: &str) -> String {
    let mut bytes = blank_string_literals(line).into_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'/', b'/') => {
                bytes.truncate(i);
                break;
            }
            (b'/', b'*') => {
                // A span with no close on this line OPENS a multi-line
                // comment, whose remaining lines [`LineMask`] masks: blanking
                // to the end of the line is the same answer for the part of
                // that comment the mask never sees.
                let close = bytes[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(bytes.len(), |at| i + 2 + at + 2);
                bytes[i..close].fill(b' ');
                i = close;
            }
            _ => i += 1,
        }
    }
    // Every replaced byte is an ASCII space and every delimiter is ASCII, so
    // the buffer is valid UTF-8 by construction.
    String::from_utf8(bytes).unwrap_or_else(|_| line.to_string())
}

/// Whether the construction on `lines[at]` is exempted by a `// <marker> <why>`
/// comment on its own line or the line above, with a reason written after it.
/// The code span's length IS the offset the comment opens at, and it answers
/// `line.len()` where there is no comment at all, so the marker is read from
/// the true comment and a line cannot claim the hatch by carrying the marker
/// inside a string literal.
fn hatched(lines: &[&str], at: usize, marker: &str) -> bool {
    let marked = |line: &str| {
        carries_hatch(line, marker)
            && line
                .get(crate::test_helpers::code_span(line).len() + 2..)
                .and_then(|comment| comment.split_once(marker))
                .is_some_and(|(_, why)| !why.trim().is_empty())
    };
    marked(lines[at]) || (at > 0 && marked(lines[at - 1]))
}

/// The index of the `]` closing the bracket `text` opens on, or `None` while it
/// is still open, so an accumulation runs to the BALANCED close: a value holding
/// a bracket of its own (`class[0].1`) ends the scan at the first `]` character
/// and leaves every later entry unread.
fn balanced_close(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (at, ch) in text.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The argument text of the call whose `(` sits just before `from` on
/// `lines[at]`, up to its matching close paren, rows below included: rustfmt
/// splits a long call, and a line-scoped read would see `with_writer(` and
/// `std::io::stderr` as two unrelated lines.
///
/// The text comes back RAW, because a rule may turn on a string literal the
/// blanked rendering would have emptied; a caller whose tells are identifiers
/// takes [`writer_argument`] instead. Parens are counted on the blanked
/// rendering either way, which is byte-for-byte, so one inside a literal
/// cannot close the call early. Bounded at a few rows, so an unbalanced paren
/// cannot swallow the rest of the file and pair the call with an unrelated
/// `stderr` far below it.
///
/// The raw text is the ceiling as well: a `/* … */` span inside the argument
/// is cut like code but pushed verbatim, so a tell written inside a block
/// comment reads as a tell. No current call site writes one.
fn call_argument(lines: &[&str], at: usize, from: usize) -> String {
    const MAX_LINES: usize = 6;
    let mut depth = 1usize;
    let mut arg = String::new();
    for (offset, line) in lines[at..].iter().take(MAX_LINES).enumerate() {
        let masked = code_half(line);
        let start = if offset == 0 {
            from.min(masked.len())
        } else {
            0
        };
        let mut close = None;
        for (pos, byte) in masked.as_bytes().iter().enumerate().skip(start) {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(pos);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = close.unwrap_or(masked.len());
        arg.push_str(&line[start..end]);
        if close.is_some() {
            break;
        }
        arg.push(' ');
    }
    arg
}

/// That same argument with its literals blanked, so a `stderr` written inside
/// one is prose to a walk whose tells are identifiers.
fn writer_argument(lines: &[&str], at: usize, from: usize) -> String {
    blank_string_literals(&call_argument(lines, at, from))
}

/// Extract the body of every `struct Emitting` / `impl … Emitting` region in
/// `source`, by brace matching from the region's opening `{`.
///
/// An `impl` header may wrap across lines (rustfmt does that once the generics
/// grow), so the header is matched against the text from `impl` up to the
/// first `{` rather than against one physical line.
fn emitting_regions(source: &str) -> Vec<String> {
    let mut regions = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    for (start, line) in lines.iter().enumerate() {
        let opens_region = line.contains("struct Emitting")
            || (line.trim_start().starts_with("impl") && impl_header(&lines[start..]));
        if !opens_region {
            continue;
        }
        let mut depth = 0usize;
        let mut seen_open = false;
        let mut body = String::new();
        for line in &lines[start..] {
            body.push_str(line);
            body.push('\n');
            for c in line.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        seen_open = true;
                    }
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if seen_open && depth == 0 {
                break;
            }
        }
        regions.push(body);
    }
    regions
}

/// The header of the `impl` block starting at `lines[0]`, up to its opening
/// brace, names `Emitting`.
fn impl_header(lines: &[&str]) -> bool {
    let mut header = String::new();
    for line in lines {
        match line.split_once('{') {
            Some((head, _)) => {
                header.push_str(head);
                break;
            }
            None => {
                header.push_str(line);
                header.push(' ');
            }
        }
    }
    header.contains("Emitting")
}

/// The collector split is what makes the deferred-header flush and the kv
/// drain unable to re-enter `write_line` — they hold `&mut RenderState` alone,
/// with no sink and no lock. A collector that regained either would deadlock or
/// emit out of band, and neither failure is visible in a diff.
#[test]
fn emit_collectors_take_no_sink() {
    // The whole tree, not a file list: an `impl Emitting` added in a file
    // nobody remembered to list would otherwise be silently unfenced.
    let files: Vec<PathBuf> = workspace_rust_files()
        .into_iter()
        .filter(|p| p.components().any(|c| c.as_os_str() == "output"))
        .filter(|p| !p.ends_with(Path::new("output/tests/fences.rs")))
        .collect();
    let banned = ["Writer", "self.state.lock(", "write_line("];
    let mut regions = Vec::new();
    let mut offenders = Vec::new();
    for path in files {
        let body = crate::test_helpers::production_slice_of(&path);
        for region in emitting_regions(&body) {
            for needle in banned {
                if region.contains(needle) {
                    offenders.push(format!(
                        "{}: collector region mentions {needle}",
                        path.display()
                    ));
                }
            }
            regions.push(region);
        }
    }
    // The struct plus its three impls (mod.rs, kv.rs, section.rs). A lower
    // count means the extractor stopped matching and the fence proves nothing.
    assert!(
        regions.len() >= 4,
        "matched only {} Emitting regions",
        regions.len()
    );
    assert!(
        regions
            .iter()
            .any(|r| r.contains("fn push_line") && r.contains("fn drain_kv_buffer")),
        "the extracted regions are truncated — the collector bodies are missing"
    );
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

/// `PackageContext` gained a `notes` field, and a struct literal is how a call
/// site opts out of the sink without saying so — it compiles, collects nothing,
/// and the manager's post-install notes vanish. The constructors
/// (`PackageContext::new` / `::with_notes`) are the only supported spelling, so
/// the literal must not reappear outside the constructors themselves.
#[test]
fn package_context_is_only_built_through_its_constructors() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("providers/mod.rs"))
            || path.ends_with(Path::new("output/tests/fences.rs"))
        {
            continue;
        }
        // unfloored-slice-ok: a literal anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        for (i, line) in body.lines().enumerate() {
            if line.contains("PackageContext {") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "build a PackageContext with ::new(printer, state) or \
         ::with_notes(printer, state, notes); a literal silently drops \
         post-install notes:\n{}",
        offenders.join("\n")
    );
}

/// The files that RENDER a pending-decision row. The fence below refuses a
/// `.summary` read anywhere in them, which is the point: a decision's stored
/// summary restates its own coordinates, and a row that reads one is a screen
/// describing an item by its label rather than by what it would put on the
/// machine. [`DecisionContents::decision_row`] is the ONE composer, and the summary
/// reaches a row only through the version-conflict annotation it derives.
///
/// [`DecisionContents::decision_row`]: crate::reconciler::DecisionContents::decision_row
const DECISION_ROW_RENDERERS: &[&str] = &[
    "cfgd-core/src/reconciler/run.rs",
    "cfgd/src/cli/source/helpers.rs",
    "cfgd/src/cli/decide.rs",
    "cfgd/src/cli/status.rs",
];

/// A pending decision's stored `summary` restates its own coordinates. Every
/// surface that LISTS a decision — `cfgd decide`, `cfgd status`, the run
/// header's withheld rows — renders through [`DecisionContents::decision_row`]
/// instead, so three screens naming one item cannot describe it three ways.
///
/// The predicate is STRUCTURAL rather than a list of binding names: any
/// `.summary` field read inside a row-rendering file is refused, whatever the
/// binding is called. The earlier name list (`item.summary`, `row.summary`, …)
/// went green the moment a renderer bound the decision to any other name, which
/// is a fence that guards a spelling instead of a rule. A read that is
/// genuinely about something else (an `applies` record's summary column, a
/// `-o json` serialization of the stored row) carries
/// `// decision-summary-ok: <why>` on its own line or the line above it.
///
/// [`DecisionContents::decision_row`]: crate::reconciler::DecisionContents::decision_row
#[test]
fn no_decision_row_renderer_reads_the_stored_summary() {
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    for path in workspace_rust_files() {
        let posix = crate::to_posix_string(&path);
        if !DECISION_ROW_RENDERERS.iter().any(|f| posix.ends_with(f)) {
            continue;
        }
        scanned += 1;
        // unfloored-slice-ok: the listed renderers are judged whole, which can only add offenders.
        let body = walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !reads_a_summary_field(line) {
                continue;
            }
            let hatched = carries_hatch(line, "decision-summary-ok:")
                || i.checked_sub(1)
                    .is_some_and(|p| carries_hatch(lines[p], "decision-summary-ok:"));
            if !hatched {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, trimmed));
            }
        }
    }
    assert_eq!(
        scanned,
        DECISION_ROW_RENDERERS.len(),
        "every listed row renderer must exist and be scanned; a renamed file \
         silently empties this fence"
    );
    assert!(
        offenders.is_empty(),
        "render a decision through DecisionContents::decision_row; the stored summary \
         belongs to reconciler/pending.rs:\n{}",
        offenders.join("\n")
    );
}

/// `<expr>.summary` — a field read, never `.summary()` (the accessor on a
/// minted decision, which lives in `pending.rs` and is not what a row reads)
/// and never `foo_summary` (a script summary, a diff summary).
fn reads_a_summary_field(line: &str) -> bool {
    line.match_indices(".summary").any(|(at, _)| {
        // A following `(` is the accessor method; a following identifier
        // character is a longer field name (`.summary_counts`). The dot in the
        // pattern already excludes `script_summary` and its siblings.
        let after = line[at + ".summary".len()..].chars().next();
        !matches!(after, Some(c) if c == '(' || c.is_alphanumeric() || c == '_')
    })
}

/// The matcher itself, so the fence above cannot pass because it never matches
/// anything. `pending.rs` is where the summary legitimately lives.
#[test]
fn the_summary_matcher_finds_the_reads_that_do_exist() {
    assert!(reads_a_summary_field("let a = decision.summary;"));
    assert!(reads_a_summary_field("f.detail(&self.summary)"));
    assert!(!reads_a_summary_field("refreshed.summary()"));
    assert!(!reads_a_summary_field("d.script_summary.clone()"));
    assert!(!reads_a_summary_field("snapshot.summary_counts"));

    let pending = workspace_rust_files()
        .into_iter()
        .find(|p| crate::to_posix_string(p).ends_with("reconciler/pending.rs"))
        .unwrap_or_else(|| panic!("reconciler/pending.rs must exist"));
    // unfloored-slice-ok: one file, asked only whether the reads exist in it.
    let body = walked_file_body(&pending);
    assert!(
        body.lines().any(reads_a_summary_field),
        "the fallback's own file must contain the reads this fence refuses \
         elsewhere; if it does not, the fence guards nothing"
    );
}

/// The subsystems a daemon log line may name. Every info-level event on the
/// daemon's stream opens with one of these, so a reader scanning a journal can
/// tell at a glance which of the daemon's four concurrent concerns is speaking.
const DAEMON_SUBSYSTEMS: &[&str] = &["daemon: ", "sync: ", "reconcile: ", "watch: "];

/// The daemon's log IS its output — under systemd or launchd it is the only
/// surface a running daemon has — so the stream is held to the same dialect a
/// terminal render is: `HH:MM:SS  INFO <subsystem>: <sentence>`.
///
/// Two halves, and the second is the one that keeps being re-broken. A
/// subsystem prefix makes a journal scannable; `key=value` fields do not belong
/// on an info line at all. `sync: pulled new changes from remote from=9777c7d
/// to=95f300a` is a sentence that stops mid-thought and then repeats itself in
/// a second grammar — the value the reader wants is in the tail, in the
/// notation, unpunctuated. Fields are a debugging detail, and `debug!` is where
/// a debugging detail goes; the info line spells its operands into the
/// sentence.
///
/// The third half is what may NOT speak: a `Printer` heading or status line
/// from inside the loop lands on the same journal without the timestamp, the
/// level or the subsystem every neighbouring line carries. `cfgd daemon run`
/// opened with a bare `Daemon` heading and `Starting cfgd daemon` above a
/// stream of `HH:MM:SS  INFO daemon: …`, which is two dialects in one log.
/// `service/` is exempt: installing the unit is a one-shot command the user is
/// watching, and its report belongs on the terminal. Inside the loop's own
/// directory, `// terminal-row-ok: <why>` on the call's line or the one above
/// exempts a row addressed to a terminal the code has just established there
/// is one of — the startup banner's stop key, printed under `can_prompt()`.
///
/// Scoped to `daemon/`, because that is exactly the directory `audit.sh`
/// exempts from the workspace-wide `tracing::info!` ban.
#[test]
fn every_daemon_info_event_names_its_subsystem() {
    const PRINTER_LINES: &[&str] = &[
        "printer.heading(",
        "printer.status_simple(",
        "printer.status(",
        "printer.status_with(",
    ];
    const TERMINAL_ROW_HATCH: &str = "terminal-row-ok:";
    let mut offenders = Vec::new();
    let mut seen = 0usize;
    for path in workspace_rust_files() {
        if !path.components().any(|c| c.as_os_str() == "daemon")
            || crate::test_helpers::is_test_source(&path)
        {
            continue;
        }
        let body = crate::test_helpers::production_slice_of(&path);
        // `service/` installs and uninstalls the unit from a one-shot command
        // the user is watching, so those DO report through the printer. The
        // loop itself has no terminal to report to.
        if !path.components().any(|c| c.as_os_str() == "service") {
            let lines: Vec<&str> = body.lines().collect();
            for (n, line) in lines.iter().enumerate() {
                if let Some(call) = PRINTER_LINES.iter().find(|c| line.contains(**c)) {
                    if hatched(&lines, n, TERMINAL_ROW_HATCH) {
                        continue;
                    }
                    offenders.push(format!(
                        "{}:{}: `{call}` — the reconcile loop speaks through \
                         `tracing`, whose events carry the timestamp and level \
                         a journal reader reads, so a row addressed to a \
                         terminal carries `// terminal-row-ok: <why>`",
                        path.display(),
                        n + 1
                    ));
                }
            }
        }
        for (line_no, args) in macro_invocations(&body, "tracing::info!(") {
            seen += 1;
            let where_ = format!("{}:{}", path.display(), line_no);
            // tracing puts fields before the format string, so an info call
            // whose first argument is not the literal is carrying fields.
            if !args.starts_with('"') {
                offenders.push(format!("{where_}: fields precede the message: {args}"));
                continue;
            }
            match first_string_literal(&args) {
                None => offenders.push(format!("{where_}: no message literal: {args}")),
                Some(message) if !DAEMON_SUBSYSTEMS.iter().any(|p| message.starts_with(p)) => {
                    offenders.push(format!("{where_}: unprefixed message {message:?}"));
                }
                Some(_) => {}
            }
        }
    }
    assert!(seen > 20, "the walk found only {seen} daemon info events");
    assert!(
        offenders.is_empty(),
        "a daemon info line is `<subsystem>: <sentence>` with its operands spelled \
         into the sentence — one of {DAEMON_SUBSYSTEMS:?}, and no `key = value` \
         fields (move those to a `debug!` beside it):\n{}",
        offenders.join("\n")
    );
}

/// Every invocation of `name` in `body`, as `(1-based line, argument text)`.
///
/// Paren-matched across lines and literal-aware, because rustfmt splits a long
/// macro call over five lines and a line-scoped read would see the name and its
/// message as unrelated.
fn macro_invocations(body: &str, name: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = body[search_from..].find(name) {
        let start = search_from + rel + name.len();
        search_from = start;
        // The line number is the count of newlines before the call.
        let line = body[..start].matches('\n').count() + 1;
        let mut depth = 1usize;
        let mut arg = String::new();
        let mut in_str = false;
        let mut escaped = false;
        for ch in body[start..].chars() {
            if in_str {
                arg.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_str = false;
                }
                continue;
            }
            match ch {
                '"' => {
                    in_str = true;
                    arg.push(ch);
                }
                '(' => {
                    depth += 1;
                    arg.push(ch);
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    arg.push(ch);
                }
                _ => arg.push(ch),
            }
        }
        out.push((line, arg.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    out
}

/// The first double-quoted literal in `args`, unescaped only enough to read its
/// opening words.
fn first_string_literal(args: &str) -> Option<String> {
    let start = args.find('"')? + 1;
    let mut out = String::new();
    let mut escaped = false;
    for ch in args[start..].chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

/// The parser behind the fence above, so it cannot pass by never matching. A
/// fielded call and a message-first call are the two shapes it has to tell
/// apart, and a rustfmt-split call is the shape a line-scoped read gets wrong.
#[test]
fn the_daemon_log_dialect_matcher_reads_both_call_shapes() {
    let body = "fn f() {\n    tracing::info!(\n        \"sync: pulled {} {}\",\n        \
                a,\n        b\n    );\n    tracing::info!(from = %x, \"pulled\");\n}\n";
    let calls = macro_invocations(body, "tracing::info!(");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].0, 2,
        "the split call is reported at its opening line"
    );
    assert!(calls[0].1.starts_with('"'), "{:?}", calls[0].1);
    assert_eq!(
        first_string_literal(&calls[0].1).as_deref(),
        Some("sync: pulled {} {}")
    );
    assert!(
        !calls[1].1.starts_with('"'),
        "a fielded call is what the fence refuses: {:?}",
        calls[1].1
    );
    assert_eq!(first_string_literal(&calls[1].1).as_deref(), Some("pulled"));
}

/// Whether `func` mentions `ident` as a whole identifier — flanked by no
/// `[A-Za-z0-9_]` on either side.
///
/// A producer tell EXEMPTS a function, so a bare substring test widens every
/// exemption to any identifier containing a producer's name:
/// `recorded_managed_env_files` is a state-store query, not a producer, and a
/// fixture whose only producer-shaped mention is that call is hand-spelling.
/// The cost runs the other way instead — a producer whose name extends
/// another's (`primary_env_file_path` over `primary_env_file`) needs its own
/// entry in the tell set — which fails loud when the shorter entry stops
/// matching, where the substring's failure was a silent exemption.
fn names_identifier(func: &str, ident: &str) -> bool {
    let is_word = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_';
    let bytes = func.as_bytes();
    func.match_indices(ident).any(|(at, _)| {
        (at == 0 || !is_word(&bytes[at - 1]))
            && bytes.get(at + ident.len()).is_none_or(|b| !is_word(b))
    })
}

use crate::test_helpers::LineMask;

impl LineMask {
    /// Whether the NEXT line begins inside a literal or comment.
    fn masked(&self) -> bool {
        self.raw_hashes.is_some() || self.in_plain || self.comment_depth > 0
    }

    /// The source half of one line, advancing across it: everything from the
    /// point the line stopped being masked, less its own literal bodies and
    /// trailing comment.
    ///
    /// A line that CLOSES a multi-line literal or block comment is source
    /// AFTER the closing delimiter. Skipping such a line whole loses a brace
    /// that really does move the depth — a `"#;` followed by the block's own
    /// `}` is the shape — and a lost close is a slice that ends somewhere
    /// other than where the code does, in the direction nothing reports.
    fn source_code(&mut self, line: &str) -> String {
        code_half(self.source_remainder(line))
    }

    /// The same span before its literal bodies and trailing comment are
    /// dropped, for a caller reading the line's SHAPE rather than its braces.
    fn source_remainder<'a>(&mut self, line: &'a str) -> &'a str {
        let began_masked = self.masked();
        self.advance(line);
        let from = if began_masked {
            self.resumed_at.unwrap_or(line.len())
        } else {
            0
        };
        line.get(from..).unwrap_or("")
    }
}

/// The label a scanner names its source by, held in a module of its own so
/// the inner string is unreachable from outside it.
///
/// A `&str` parameter accepted every spelling of a path, so the rule that a
/// walk names one file one way was the CALLER's to remember and a scanner
/// handed `path.display()` compiled. With the field private, [`source_label`]
/// and [`FIXTURE_SOURCE`] are the only ways to hold a [`SourceLabel`] at all,
/// and the offending shape stops compiling.
mod label {
    use std::borrow::Cow;
    use std::fmt;
    use std::path::Path;

    /// The label a file walk names a path by: workspace-relative, `/`-folded.
    ///
    /// `workspace_root` is a `..`-joined absolute path and `PathBuf` never
    /// normalizes one away, so the absolute spelling names a file as
    /// `…/crates/cfgd-core/../../crates/cfgd-core/src/…` here and with `\`
    /// between the components on Windows — three renderings of one file, none
    /// of which a reader can paste. Stripping the root leaves the spelling a
    /// CI log, a Windows run and this box all print identically.
    pub(super) struct SourceLabel(Cow<'static, str>);

    impl fmt::Display for SourceLabel {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    /// The label a scan over a literal fixture names, where a file walk names
    /// the path it read.
    pub(super) static FIXTURE_SOURCE: SourceLabel = SourceLabel(Cow::Borrowed("<fixture>"));

    /// Fold `path` into the one label every walk over it names it by.
    pub(super) fn source_label(path: &Path) -> SourceLabel {
        SourceLabel(Cow::Owned(crate::to_posix_string(
            path.strip_prefix(crate::test_helpers::workspace_root())
                .unwrap_or(path),
        )))
    }
}

use label::{FIXTURE_SOURCE, SourceLabel, source_label};

/// The functions a source's `fn` lines cut it into, each paired with its
/// opening line number.
///
/// A line is an opener only OUTSIDE every string literal and comment: a
/// fixture spelling `fn f() {` on a line of a multi-line literal — raw,
/// `\`-continued, or a plain `"…"` spanning lines — would otherwise split the
/// enclosing function around it, and a split severs a folded tell from its
/// opening line or a tell from the producer token that exempts it, the
/// silent direction. [`LineMask`] holds that state, and a declaration sharing
/// its physical line with the CLOSE of a literal or comment still opens a
/// slice: the mask hands back the line's remainder, so the reverse mistake —
/// a real declaration the walk never sees — is not traded for the first.
///
/// A slice ends at its declaration's OWN closing brace, found by
/// [`function_source`]'s brace scan — the same scanner, reached with a whole
/// body rather than one open. Cut at the next `fn` instead, every file-scope
/// item between two declarations lands in the first one's slice: the `const`
/// holding a YAML fixture read as the body of the test above it, which made
/// that test a member of populations it never joined and judged it on text it
/// does not contain.
///
/// PARTIAL, not total: a declaration whose braces never balance before the
/// input ends panics out of [`function_source`], which names `source`, the
/// line the declaration opened on and that line's text. Every caller here
/// reads whole files it does not author, and the alternative — a slice cut at
/// the end of the input — is a body the walk believes it read and did not, so
/// a desynced scan stops the test rather than answering from a slice it
/// invented. `source` is what makes that answer actionable: a walk over the
/// workspace reads hundreds of files, and a line number alone names none of
/// them. `source` is a [`SourceLabel`], whose only constructors are
/// [`source_label`] and [`FIXTURE_SOURCE`], so a caller cannot hand a scanner
/// a second spelling of a file it already labelled.
/// `an_unbalanced_declaration_stops_the_walk` holds the contract.
fn source_functions(source: &SourceLabel, body: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut mask = LineMask::default();
    // Each open carries the SPAN the declaration starts at, not just its line:
    // on a literal's closing line the span begins past the close, and a brace
    // scan handed the whole line would read that literal's own closing quote as
    // a fresh string opener and desync over the rest of the body.
    let mut opens: Vec<(usize, &str)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let began_masked = mask.masked();
        let rest = mask.source_remainder(line);
        // On the line that CLOSES a literal, what precedes the declaration is
        // the tail of the statement the literal belonged to (`"#;`), so the
        // declaration does not start the span. Only a closing line looks past
        // anything at all.
        //
        // Which terminator ends that tail cannot be counted off: the line may
        // carry further statements of its own before the declaration, so it is
        // the EARLIEST terminator a declaration follows. Taking the first
        // outright stops at `"#; let s = 1; fn f() {}`'s first `;`; taking the
        // last lets a declaration's own body (`fn f() { let x = 1; }`) swallow
        // it. Candidates are read off the literal-blanked remainder, which is
        // byte-length preserving, so a `;` inside a literal of the tail's own
        // (`let s = "a;b";`) is never one.
        //
        // `;` is the only terminator the scan names, which is its ceiling: a
        // declaration the tail reaches through a `}` (`"#; } fn f() {}`) opens
        // no slice. Every other boundary is a brace whose meaning depends on
        // the nesting above this line, and reading it would take a parser,
        // where a statement boundary states itself.
        let code = code_half(rest);
        let head = if began_masked && code.contains(';') {
            code.match_indices(';')
                .map(|(at, _)| rest.get(at + 1..).unwrap_or(""))
                .find(|tail| opens_function(tail))
        } else {
            opens_function(rest).then_some(rest)
        };
        if let Some(head) = head {
            opens.push((i, head));
        }
    }
    opens
        .into_iter()
        .map(|(at, head)| (at + 1, function_source(source, &lines, at, head)))
        .collect()
}

/// Whether a line's code half opens a `const` or `static` ITEM rather than a
/// `const fn`, whatever visibility stands in front of it.
fn opens_const_item(code: &str) -> bool {
    !opens_function(code) && matches!(item_keyword(code), "const" | "static")
}

/// The `const` and `static` items a source declares outside every function
/// body, each paired with its opening line number.
///
/// [`source_functions`]' complement over the same file: an item written
/// between two declarations belongs to no slice, so it is the text every
/// function-scoped walk here reads not at all. An item runs from its
/// declaration to the `;` that closes it, counted on
/// [`LineMask::source_code`] so a `;` inside a literal or a comment ends none,
/// and only once the bracket depth the declaration opened has come back down —
/// a `[u8; 4]` in the type would otherwise end the item on its own first line
/// and hide the initializer this exists to read.
fn const_items_outside_functions(source: &SourceLabel, body: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let in_a_function: Vec<std::ops::RangeInclusive<usize>> = source_functions(source, body)
        .iter()
        // Both ends are 0-based indices of `lines`, off a 1-based opening line
        // and a slice that always holds at least the declaration itself; the
        // additions come first, so a one-line declaration at line 1 does not
        // take the subtraction below zero.
        .map(|(open, slice)| (open - 1)..=(open + slice.lines().count() - 2))
        .collect();
    let mut mask = LineMask::default();
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    let mut depth = 0i32;
    for (i, line) in lines.iter().enumerate() {
        let code = mask.source_code(line);
        if open.is_none()
            && opens_const_item(&code)
            && !in_a_function.iter().any(|r| r.contains(&i))
        {
            open = Some(i);
            depth = 0;
        }
        let Some(at) = open else {
            continue;
        };
        depth += code.matches(['(', '[', '{']).count() as i32
            - code.matches([')', ']', '}']).count() as i32;
        if depth <= 0 && code.contains(';') {
            out.push((at + 1, lines[at..=i].join("\n")));
            open = None;
        }
    }
    out
}

/// No cfgd-core fixture hand-spells the generated env file's name or its
/// dialect.
///
/// The twin of `cli/tests.rs`'s
/// `no_env_file_fixture_hardcodes_the_primary_env_files_name_or_dialect`, for
/// the crate the generator lives in. Three tests that had never executed on
/// windows-latest failed there the day they first ran, because the primary
/// managed env file is `~/.cfgd.env` on POSIX and `~/.cfgd-env.ps1` on
/// Windows, and a declared entry renders as bash `export EDITOR="vim"
/// # module:m` there and PowerShell `$env:EDITOR = 'vim' # module:m` here — a
/// fixture hardcoding either wrote its file where nothing reads, or a line the
/// check can never match, and the ones asserting an ABSENCE passed anyway.
///
/// Exempt where a literal is correct and no per-site hatch should be needed:
/// a fixture exercising the generator NAMES it (an explicit `EnvPlatform`, a
/// `generate_*` call, `env_targets`), and one naming a path production writes
/// VERBATIM names that too (`WriteEnvFile`, `plan_env_with_home`,
/// `ScriptShell` — where the shell, not the host, picks the dialect). A unit
/// that spells a tell and names none of them is hand-spelling.
///
/// The population is BOTH halves of every file: the [`source_functions`]
/// slices, and the [`const_items_outside_functions`] items between them, under
/// one judgement so a fixture cannot escape it by being written at file scope.
/// A `const` holding a hand-spelled env-file body is the same regression as a
/// function holding one — the generator still never ran, and the ps1/POSIX
/// split still decides which host reads the file. The items half holds no
/// offender today, and the item count it reads has a floor: a minimum the scan
/// must keep, so file-scope items cannot drop out of it silently, and a count
/// above it passes. The same scan for the mutation tells is floored by
/// [`no_item_outside_a_function_body_mutates_the_process_environment`].
#[test]
fn no_core_env_file_fixture_hardcodes_the_primary_env_files_name_or_dialect() {
    let joins = [
        format!("join(\"{}\")", ".cfgd.env"),
        format!("join(\"{}\")", ".cfgd-env.ps1"),
    ];
    let owner_comments = [
        format!("# {}:", "module"),
        format!("# {}:", "profile"),
        format!("# {}:", "manager"),
    ];
    let dialects = ["export ", "$env:"];
    // A PATH line carries no owner comment (`comment(owner)` is only called
    // for a declared env var), so `spells_a_line` above never sees it — a
    // fixture hand-spelling the PATH export line itself needed no owner tell
    // beside it to slip through. These four are hits on their own.
    let dialect_tells_alone = ["export PATH=", "$env:PATH =", "set -gx PATH"];
    const ENVIRONMENT_D_TELL: &str = "environment.d";
    const ENVIRONMENT_D_PATH_TELL: &str = "PATH=";
    // Every producer is named in FULL and matched as a WHOLE identifier: the
    // crate also holds `generate_` functions for systemd units and SLSA
    // provenance, and `recorded_managed_env_files` is a state-store query —
    // a prefix or bare substring would exempt any fixture that mentions such
    // a lookalike beside a hand-spelled tell. A producer whose name extends
    // another's (`primary_env_file_path`) is its own entry.
    let names_a_producer = [
        "EnvPlatform",
        "primary_env_file",
        "primary_env_file_path",
        "managed_env_files",
        "env_targets",
        "generate_env_file_content",
        "generate_fish_env_content",
        "generate_powershell_env_content",
        "generate_environment_d_content",
        "WriteEnvFile",
        "plan_env_with_home",
        "ScriptShell",
    ];
    // The dialect-alone tells above earn a NARROWER hatch than a fixture's
    // name/owner-comment tells do: `apply_does_not_reorder_the_env_file_…`
    // names `primary_env_file` for an unrelated existence check and, under
    // the wider list, that alone used to excuse a hand-spelled `export PATH=`
    // literal it never derived from anything. Only a function that calls the
    // dialect-emitting generator itself may spell the dialect it just called.
    let generator_calls = [
        "env_targets",
        // The dialect ITSELF: `Dialect`'s arms are where each generator's
        // assignment syntax lives, so the methods spelling it are the
        // generator for this purpose.
        "Dialect",
        "generate_env_file_content",
        "generate_fish_env_content",
        "generate_powershell_env_content",
        "generate_environment_d_content",
    ];
    let core_src = workspace_root().join("crates/cfgd-core/src");
    let mut offenders = Vec::new();
    let mut checked = 0usize;
    let mut items = 0usize;
    for path in workspace_rust_files() {
        // This file spells every tell in order to hunt for it.
        if !path.starts_with(&core_src) || path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: the env-file fixtures judged here live in test regions.
        let body = walked_file_body(&path);
        // The dialect-alone tells' one path-based hatch: the file that OWNS
        // the dialect (`env_engine.rs`, home of `path_line`/`fold_path_line`)
        // pins the raw assignment syntax through helpers with no `generate_*`
        // name of their own to match on.
        let is_env_engine_owner = path.ends_with(Path::new("reconciler/env_engine.rs"));
        let shown = source_label(&path);
        // A file's two halves, judged by ONE block: an exemption or a tell
        // means the same thing whether the text was written inside a
        // declaration or between two of them, and a second judgement is how
        // the halves start disagreeing.
        let functions = source_functions(&shown, &body);
        let file_scope = const_items_outside_functions(&shown, &body);
        checked += functions.len();
        items += file_scope.len();
        for (open, func) in functions.iter().chain(file_scope.iter()) {
            let (open, func) = (*open, func.as_str());
            let names_producer = names_a_producer.iter().any(|n| names_identifier(func, n));
            let calls_generator =
                is_env_engine_owner || generator_calls.iter().any(|n| names_identifier(func, n));
            if names_producer && calls_generator {
                continue;
            }
            let folded = crate::test_helpers::logical_source_lines(func);
            let spells_a_name = !names_producer
                && folded
                    .iter()
                    .any(|(_, l)| joins.iter().any(|j| l.contains(j.as_str())));
            let spells_a_line = !names_producer
                && folded.iter().any(|(_, l)| {
                    dialects.iter().any(|d| l.contains(d))
                        && owner_comments.iter().any(|c| l.contains(c.as_str()))
                });
            let spells_a_dialect_alone = !calls_generator
                && folded
                    .iter()
                    .any(|(_, l)| dialect_tells_alone.iter().any(|d| l.contains(d)));
            let spells_environment_d_path = !calls_generator
                && func.contains(ENVIRONMENT_D_TELL)
                && folded
                    .iter()
                    .any(|(_, l)| l.contains(ENVIRONMENT_D_PATH_TELL));
            if spells_a_name || spells_a_line || spells_a_dialect_alone || spells_environment_d_path
            {
                offenders.push(format!(
                    "{shown}:{open}: {}",
                    body.lines().nth(open - 1).unwrap_or("").trim()
                ));
            }
        }
    }
    assert!(
        checked > 2000,
        "the walk no longer reaches cfgd-core's functions — it read {checked}"
    );
    // The item count is a minimum the scan must keep, so an item cannot drop
    // out of it silently, and a count above it passes. The items half holds no
    // offender, and an empty offender list reads the same whether the scan saw
    // every file-scope item or none of them.
    assert!(
        items >= 315,
        "the walk read {items} items outside a function body in cfgd-core; it \
         has stopped seeing the crate's file-scope declarations"
    );
    assert!(
        offenders.is_empty(),
        "a fixture must take the env file's path from \
         `cfgd_core::reconciler::primary_env_file` and its body from the \
         generator (`MergedEnvItems::managed_env_files`, or a `generate_*` \
         call under an explicit `EnvPlatform`) — both halves are the running \
         platform's:\n{}",
        offenders.join("\n")
    );
}

/// Every source under the crate roots `floors` names, tests and integration
/// tests included, each paired with the root it sits under. The cross-OS
/// fixture fences below read the two crates the Windows test leg builds.
fn cross_os_fence_sources(floors: &[(&'static str, usize)]) -> Vec<(&'static str, PathBuf)> {
    let crates = workspace_root().join("crates");
    workspace_rust_files()
        .into_iter()
        .filter_map(|path| {
            let krate = floors
                .iter()
                .map(|(krate, _)| *krate)
                .find(|krate| path.starts_with(crates.join(krate)))?;
            Some((krate, path))
        })
        .collect()
}

/// Fails unless every root in `floors` contributed at least its floor to
/// `counts`, so one tree going dark fails on its own name; the other tree's
/// total cannot cover for it.
fn assert_each_root_read(
    floors: &[(&'static str, usize)],
    counts: &std::collections::BTreeMap<&str, usize>,
    what: &str,
) {
    for (krate, floor) in floors {
        let read = counts.get(krate).copied().unwrap_or(0);
        assert!(
            read >= *floor,
            "the walk read {read} {what} under `{krate}`, fewer than it holds \
             ({counts:?}); it has stopped seeing them"
        );
    }
}

/// The end of the call whose argument list opens at `open` in `code`, a body
/// with its literals and comments blanked, and the byte offset of the first
/// comma at the call's own depth when it has one.
fn call_span(code: &str, open: usize) -> (usize, Option<usize>) {
    let mut depth = 1usize;
    let mut first_comma = None;
    for (i, c) in code[open..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return (open + i, first_comma);
                }
            }
            ',' if depth == 1 && first_comma.is_none() => first_comma = Some(open + i),
            _ => {}
        }
    }
    (code.len(), first_comma)
}

/// Every `fold_home_in_text` call in `code` (literals and comments blanked),
/// and the 1-based lines of those whose argument renders a path natively.
fn home_folds_of_native_renders(code: &str) -> (usize, Vec<usize>) {
    let needle = "fold_home_in_text(";
    let mut calls = 0usize;
    let mut offenders = Vec::new();
    for (at, _) in code.match_indices(needle) {
        if code[..at].ends_with("fn ") {
            continue;
        }
        calls += 1;
        let open = at + needle.len();
        let (close, _) = call_span(code, open);
        let arg = &code[open..close];
        if arg.contains(".display()") || arg.contains("to_string_lossy()") {
            offenders.push(code[..at].matches('\n').count() + 1);
        }
    }
    (calls, offenders)
}

/// `fold_home_in_text` folds the home directory on its POSIX spelling, so a
/// path rendered natively reaches it on Windows as `C:\Users\…` and passes
/// through unfolded. A test expectation built that way named the absolute
/// path where the header row it compared against read `~/…`, and failed on
/// Windows alone; a production row built that way would show the absolute
/// path to every Windows user. The argument goes through `to_posix_string`
/// or `display_posix` first, in production code and tests alike.
#[test]
fn no_home_fold_is_handed_a_native_path_render() {
    let fixture = "fn f(p: &Path) {\n    a(&fold_home_in_text(&p.display().to_string()));\n    b(&fold_home_in_text(&format!(\n        \"{}\",\n        p.to_string_lossy()\n    )));\n    c(&fold_home_in_text(&to_posix_string(p)));\n    d(\"fold_home_in_text(&p.display())\");\n}\n";
    assert_eq!(
        home_folds_of_native_renders(&crate::test_helpers::blank_non_code(fixture)),
        (3, vec![2, 3]),
        "the scan reads a call's whole argument, across lines, and never a literal"
    );
    const FLOORS: [(&str, usize); 2] = [("cfgd", 120), ("cfgd-core", 30)];
    let mut per_crate: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut offenders = Vec::new();
    for (krate, path) in cross_os_fence_sources(&FLOORS) {
        // unfloored-slice-ok: a call anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        let shown = source_label(&path);
        let (calls, lines) =
            home_folds_of_native_renders(&crate::test_helpers::blank_non_code(&body));
        *per_crate.entry(krate).or_default() += calls;
        offenders.extend(lines.into_iter().map(|n| {
            format!(
                "{shown}:{n}: {}",
                body.lines().nth(n - 1).unwrap_or("").trim()
            )
        }));
    }
    assert_each_root_read(&FLOORS, &per_crate, "home folds");
    assert!(
        offenders.is_empty(),
        "`fold_home_in_text` folds the POSIX spelling of the home directory; \
         render the path with `to_posix_string` or `display_posix` first:\n{}",
        offenders.join("\n")
    );
}

/// The functions that ask the generator for the env files THIS host writes,
/// in whichever dialect this host writes them.
const RUNNING_PLATFORM_ENV_GENERATORS: [&str; 2] = ["plant_managed_env_files", "managed_env_files"];

/// Whether `func` (comments blanked) spells one assignment of a generated env
/// file by hand: a POSIX `export NAME=` or a PowerShell `$env:NAME`.
fn spells_an_env_assignment(func: &str) -> bool {
    let named = |rest: &str| rest.starts_with(|c: char| c.is_ascii_uppercase() || c == '_');
    func.match_indices("export ").any(|(at, m)| {
        let rest = &func[at + m.len()..];
        named(rest)
            && rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .is_some_and(|end| rest[end..].starts_with('='))
    }) || func
        .match_indices("$env:")
        .any(|(at, m)| named(&func[at + m.len()..]))
}

/// A fixture that plants the env files a converged machine holds takes them
/// from the generator for the RUNNING platform: `~/.cfgd.env` in POSIX syntax
/// on Unix, `~/.cfgd-env.ps1` in PowerShell on Windows. An expectation it
/// hand-spells in one dialect is the other platform's wrong answer, and a
/// verify premise written as `export EDITOR="vim"` failed on Windows alone,
/// where the planted line read `$env:EDITOR = 'vim'`. A line the fixture needs
/// comes from the same composer the generator writes with
/// (`MergedEnvItems::declared_line`).
#[test]
fn no_fixture_hand_spells_a_line_of_the_env_file_this_host_generates() {
    let fixture = "fn planted() {\n    let files = plant_managed_env_files(&m, home, scope);\n    assert!(body.contains(\"export EDITOR=\\\"vim\\\"\"));\n}\nfn derived() {\n    let files = plant_managed_env_files(&m, home, scope);\n    let line = m.declared_line(\"env-var\", \"EDITOR\");\n}\nfn powershell() {\n    let f = x.managed_env_files(home, scope);\n    // export FOO=1 in a comment is prose\n    let line = \"$env:EDITOR = 'vim'\";\n}\n";
    let judged: Vec<bool> = source_functions(&FIXTURE_SOURCE, fixture)
        .iter()
        .map(|(_, func)| spells_an_env_assignment(&crate::test_helpers::blank_comments(func)))
        .collect();
    assert_eq!(
        judged,
        [true, false, true],
        "the tell reads literals and skips comments"
    );

    const FLOORS: [(&str, usize); 2] = [("cfgd", 8), ("cfgd-core", 4)];
    let mut planting: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut offenders = Vec::new();
    for (krate, path) in cross_os_fence_sources(&FLOORS) {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: the fixtures judged here live in test regions.
        let body = walked_file_body(&path);
        let shown = source_label(&path);
        for (open, func) in source_functions(&shown, &body) {
            if !RUNNING_PLATFORM_ENV_GENERATORS
                .iter()
                .any(|name| names_identifier(&func, name))
            {
                continue;
            }
            *planting.entry(krate).or_default() += 1;
            if spells_an_env_assignment(&crate::test_helpers::blank_comments(&func)) {
                offenders.push(format!(
                    "{shown}:{open}: {}",
                    body.lines().nth(open - 1).unwrap_or("").trim()
                ));
            }
        }
    }
    assert_each_root_read(
        &FLOORS,
        &planting,
        "functions naming the running platform's env generator",
    );
    assert!(
        offenders.is_empty(),
        "a fixture reading the env files this host generates takes each line \
         from `MergedEnvItems::declared_line`, which spells this host's dialect:\n{}",
        offenders.join("\n")
    );
}

/// The marker that exempts one byte-exact read from the fence below, with
/// the reason written after it.
const EOL_EXACT_HATCH: &str = "eol-exact-ok:";

/// The 1-based lines, within `func`, of each equality assert (`assert_eq!`,
/// `assert_ne!`, or `assert!` over `==` / `!=`) with an operand that reads a
/// file byte for byte, and no hatch on its own line or the one above. An
/// operand reads one when it calls `read_to_string` itself or names a `let`
/// binding whose initializer did, in either case with no
/// `normalize_line_endings` in between.
fn byte_exact_reads(func: &str) -> Vec<usize> {
    static BINDING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=;]*)?=([^;]*);")
            .expect("the binding shape compiles")
    });
    let code = crate::test_helpers::blank_non_code(func);
    let hatches = crate::test_helpers::blank_literals(func);
    let hatch_lines: Vec<&str> = hatches.lines().collect();
    let exact =
        |text: &str| text.contains("read_to_string(") && !text.contains("normalize_line_endings");
    let bound: Vec<&str> = BINDING
        .captures_iter(&code)
        .filter(|caps| exact(&caps[2]))
        .map(|caps| caps.get(1).map_or("", |name| name.as_str()))
        .collect();
    let reads_exact = |operand: &str| {
        !operand.contains("normalize_line_endings")
            && (operand.contains("read_to_string(")
                || bound.iter().any(|name| names_identifier(operand, name)))
    };
    let mut lines = Vec::new();
    for needle in ["assert_eq!(", "assert_ne!(", "assert!("] {
        for (at, _) in code.match_indices(needle) {
            if at > 0
                && matches!(code.as_bytes()[at - 1], b'_' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
            {
                continue;
            }
            let open = at + needle.len();
            let (close, comma) = call_span(&code, open);
            let first = &code[open..comma.unwrap_or(close)];
            let operands: Vec<&str> = if needle == "assert!(" {
                match first.find("==").or_else(|| first.find("!=")) {
                    Some(op) => vec![&first[..op], &first[op + 2..]],
                    None => continue,
                }
            } else {
                let second = comma.map_or("", |comma| {
                    let (end, next) = call_span(&code, comma + 1);
                    &code[comma + 1..next.unwrap_or(end)]
                });
                vec![first, second]
            };
            let line = code[..at].matches('\n').count();
            let hatched = [line.checked_sub(1), Some(line)]
                .into_iter()
                .flatten()
                .any(|n| {
                    hatch_lines
                        .get(n)
                        .is_some_and(|l| carries_hatch(l, EOL_EXACT_HATCH))
                });
            if !hatched && operands.iter().any(|operand| reads_exact(operand)) {
                lines.push(line + 1);
            }
        }
    }
    lines.sort_unstable();
    lines
}

/// A clone checks files out under the cloning user's git config, and on
/// Windows that is `core.autocrlf=true`: a committed LF file lands as CRLF.
/// `cfgd init --from` and every source clone run the git CLI under that
/// config, so cfgd promises the committed content and leaves the line endings
/// to git. A test comparing a cloned file byte for byte therefore passed on
/// Unix and failed on Windows; it compares through `normalize_line_endings`.
/// A read of a file the test wrote itself (a refused clone checks nothing
/// out) keeps its exact bytes under `// eol-exact-ok: <why>`.
#[test]
fn no_cloned_file_is_compared_byte_for_byte() {
    let fixture = "fn cloned() {\n    run(\"--from\");\n    assert_eq!(\n        std::fs::read_to_string(dest.join(\"cfgd.yaml\")).unwrap(),\n        BEHIND\n    );\n    assert_eq!(\n        normalize_line_endings(&std::fs::read_to_string(p).unwrap()),\n        BEHIND\n    );\n    // eol-exact-ok: the test wrote this file itself\n    assert_eq!(std::fs::read_to_string(mine).unwrap(), \"x\\n\");\n    assert_eq!(n, std::fs::read_to_string(q).unwrap().len());\n    let bound = std::fs::read_to_string(dest.join(\"a\")).unwrap();\n    assert_eq!(bound, BEHIND);\n    assert_eq!(BEHIND, std::fs::read_to_string(r).unwrap());\n    assert!(std::fs::read_to_string(s).unwrap() == BEHIND);\n    assert_ne!(std::fs::read_to_string(t).unwrap(), AHEAD);\n    let folded = normalize_line_endings(&std::fs::read_to_string(u).unwrap());\n    assert_eq!(folded, BEHIND);\n    assert!(normalize_line_endings(&bound) == BEHIND);\n    assert!(std::fs::read_to_string(v).unwrap().contains(\"x\"));\n}\n";
    let funcs = source_functions(&FIXTURE_SOURCE, fixture);
    assert_eq!(
        byte_exact_reads(&funcs[0].1),
        [3, 13, 15, 16, 17, 18],
        "an unhatched read reaching either operand of an equality assert, bare \
         or through a binding, is a byte-exact read"
    );

    let clones = |func: &str| {
        let code = crate::test_helpers::blank_non_code(func);
        func.contains("\"--from\"")
            || ["clone_into", "git_clone_with_fallback"]
                .iter()
                .any(|name| names_identifier(&code, name))
            || code.contains("Repository::clone(")
    };
    const FLOORS: [(&str, usize); 2] = [("cfgd", 24), ("cfgd-core", 20)];
    let mut cloning: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut offenders = Vec::new();
    for (krate, path) in cross_os_fence_sources(&FLOORS) {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: the clones judged here live in test regions.
        let body = walked_file_body(&path);
        let shown = source_label(&path);
        for (open, func) in source_functions(&shown, &body) {
            if !clones(&func) {
                continue;
            }
            *cloning.entry(krate).or_default() += 1;
            for line in byte_exact_reads(&func) {
                let at = open + line - 1;
                offenders.push(format!(
                    "{shown}:{at}: {}",
                    body.lines().nth(at - 1).unwrap_or("").trim()
                ));
            }
        }
    }
    assert_each_root_read(&FLOORS, &cloning, "cloning functions");
    assert!(
        offenders.is_empty(),
        "a cloned file's line endings are the cloning user's git config; \
         compare through `normalize_line_endings`, or mark a file the test \
         wrote itself `// {EOL_EXACT_HATCH} <why>`:\n{}",
        offenders.join("\n")
    );
}

/// The functions that run the real caveat composer and read its env reminder
/// back.
const ENV_REMINDER_RENDERERS: [&str; 2] = ["print_caveats", "cmd_apply"];

/// Whether `func` (comments blanked) asserts on the env reminder a run
/// renders, ``Run `source …` `` or ``Run `. …` ``, through one of the real
/// renderers, and whether it states the shell it stands in: `MSYSTEM` set to
/// a non-blank literal decides the shell on its own, and otherwise both
/// `MSYSTEM` and `SHELL` carry an `EnvVarGuard`.
fn reads_the_env_reminder(func: &str) -> Option<bool> {
    let asserts = (func.contains("Run `source ") || func.contains("Run `. "))
        && ENV_REMINDER_RENDERERS
            .iter()
            .any(|name| names_identifier(func, name));
    let guards = |verbs: &str, var: &str| {
        regex::Regex::new(&format!(r#"EnvVarGuard::(?:{verbs})\(\s*"{var}""#))
            .expect("the guard shape compiles")
            .is_match(func)
    };
    // `preferred_env_file` reads a blank `MSYSTEM` as unset and goes on to
    // `SHELL`, so only a non-blank literal decides the shell alone.
    let sets_msystem = regex::Regex::new(r#"EnvVarGuard::set\(\s*"MSYSTEM"\s*,\s*"\s*[^"\s]"#)
        .expect("the set shape compiles")
        .is_match(func);
    asserts.then(|| {
        let states = |var: &str| guards("set|unset", var);
        sets_msystem || (states("MSYSTEM") && states("SHELL"))
    })
}

/// The env reminder an apply prints names the env file of the shell the run
/// stands in, read off `MSYSTEM` and `SHELL`, and Windows writes both files.
/// A test inheriting those variables asserts about how its runner was
/// launched: an apply test expecting ``Run `source ~/.cfgd.env` `` passed under
/// CI's Git Bash and failed from a PowerShell console, where the reminder
/// read ``Run `. ~/.cfgd-env.ps1` ``. A test reading the reminder back states
/// the shell through `EnvVarGuard` (unset, the platform alone decides).
#[test]
fn no_test_reads_the_env_reminder_under_the_ambient_shell() {
    let fixture = "fn a() {\n    print_caveats(&r, &p);\n    assert!(out.contains(\"Run `source ~/.cfgd.env`\"));\n}\n\nfn b() {\n    let _m = EnvVarGuard::unset(\"MSYSTEM\");\n    let _s = EnvVarGuard::unset(\"SHELL\");\n    cmd_apply(&c, &p, &a);\n    assert!(out.contains(\"Run `. ~/.cfgd-env.ps1`\"));\n}\n\nfn c() {\n    // Run `source ~/.cfgd.env` is what print_caveats says.\n    print_caveats(&r, &p);\n}\n\nfn d() {\n    let _m = EnvVarGuard::unset(\"MSYSTEM\");\n    cmd_apply(&c, &p, &a);\n    assert!(out.contains(\"Run `. ~/.cfgd-env.ps1`\"));\n}\n\nfn e() {\n    let _m = EnvVarGuard::set(\"MSYSTEM\", \"MINGW64\");\n    cmd_apply(&c, &p, &a);\n    assert!(out.contains(\"Run `source ~/.cfgd.env`\"));\n}\n\nfn f() {\n    let _m = EnvVarGuard::set(\"MSYSTEM\", \"\");\n    cmd_apply(&c, &p, &a);\n    assert!(out.contains(\"Run `. ~/.cfgd-env.ps1`\"));\n}\n";
    let judged: Vec<Option<bool>> = source_functions(&FIXTURE_SOURCE, fixture)
        .into_iter()
        .map(|(_, func)| reads_the_env_reminder(&crate::test_helpers::blank_comments(&func)))
        .collect();
    assert_eq!(
        judged,
        [
            Some(false),
            Some(true),
            None,
            Some(false),
            Some(true),
            Some(false)
        ],
        "the tell reads the reminder's literal and both guards (or `MSYSTEM` set to a \
         non-blank value), and skips comments"
    );

    const FLOORS: [(&str, usize); 1] = [("cfgd", 4)];
    let mut reading: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut offenders = Vec::new();
    for (krate, path) in cross_os_fence_sources(&FLOORS) {
        // unfloored-slice-ok: the tests judged here live in test regions.
        let body = walked_file_body(&path);
        let shown = source_label(&path);
        for (open, func) in source_functions(&shown, &body) {
            let Some(states_the_shell) =
                reads_the_env_reminder(&crate::test_helpers::blank_comments(&func))
            else {
                continue;
            };
            *reading.entry(krate).or_default() += 1;
            if !states_the_shell {
                offenders.push(format!(
                    "{shown}:{open}: {}",
                    body.lines().nth(open - 1).unwrap_or("").trim()
                ));
            }
        }
    }
    assert_each_root_read(&FLOORS, &reading, "tests reading the env reminder");
    assert!(
        offenders.is_empty(),
        "the env reminder names the running shell's env file; state the shell \
         with `EnvVarGuard` on both `MSYSTEM` and `SHELL`, or set `MSYSTEM` to a \
         non-blank value:\n{}",
        offenders.join("\n")
    );
}

/// Whether blanked `code` starts a child as the root of a process tree of its
/// own: a new process group on Unix, a suspended start ahead of a job on
/// Windows.
fn starts_a_process_tree(code: &str) -> bool {
    code.contains(".process_group(") || names_identifier(code, "CREATE_SUSPENDED")
}

/// Whether blanked `code` throws away the kill `spawn_tree` hands back with
/// the child, leaving nothing that can end the tree.
fn discards_the_tree_kill(code: &str) -> bool {
    static DISCARD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"let\s*\(\s*(?:mut\s+)?\w+\s*,\s*_\w*\s*\)\s*=[^;]*\bspawn_tree\(|let\s+_\w*\s*=[^;]*\bspawn_tree\(|\bspawn_tree\([^;]*?\)\??\s*\.0\b",
        )
        .expect("the discard shapes compile")
    });
    DISCARD.is_match(code)
}

/// Whether blanked `code` kills a child alone at a point no `#[cfg(unix)]`
/// covers: `.kill()` on a `Child`, a `TreeKill::child_alone`, or
/// `terminate_process`, declarations of those names aside. A `#[cfg(unix)]`
/// covers the statement or the braced block it is attached to; `unix_only`
/// says the declaration itself carries one.
fn kills_a_child_alone_off_unix(code: &str, unix_only: bool) -> bool {
    static KILL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\.kill\(\)|\bchild_alone\(|\bterminate_process\(")
            .expect("the kill shapes compile")
    });
    if unix_only {
        return false;
    }
    let covered: Vec<std::ops::Range<usize>> = code
        .match_indices("#[cfg(unix)]")
        .map(|(at, attr)| {
            let from = at + attr.len();
            let rest = &code[from..];
            let block = rest.trim_start().starts_with('{');
            let mut depth = 0i32;
            let mut end = code.len();
            for (i, c) in rest.char_indices() {
                match c {
                    '{' | '(' | '[' => depth += 1,
                    '}' | ')' | ']' => {
                        depth -= 1;
                        if depth < 0 || (block && depth == 0) {
                            end = from + i + 1;
                            break;
                        }
                    }
                    ';' if depth == 0 => {
                        end = from + i + 1;
                        break;
                    }
                    _ => {}
                }
            }
            from..end
        })
        .collect();
    KILL.find_iter(code).any(|m| {
        !code[..m.start()].trim_end().ends_with("fn")
            && !covered.iter().any(|r| r.contains(&m.start()))
    })
}

/// Whether the attribute lines directly above a declaration (`above`, nearest
/// last) gate it to Unix.
fn declared_unix_only(above: &[&str]) -> bool {
    above
        .iter()
        .rev()
        .map(|line| line.trim())
        .take_while(|line| line.starts_with("#[") || line.starts_with("//"))
        .any(|line| line == "#[cfg(unix)]")
}

/// Lone-child kills that stay on Windows, each with the reason no job is
/// involved: (file suffix, function, reason).
const LONE_KILLS_OFF_UNIX: &[(&str, &str, &str)] = &[
    (
        "util/process.rs",
        "spawn_tree",
        "a child whose resume failed is still suspended and has started nothing",
    ),
    (
        "util/process.rs",
        "terminate",
        "the fallback for a job cfgd could not create or join",
    ),
    (
        "upgrade/mod.rs",
        "terminate_daemon_if_running",
        "stops the daemon for an upgrade so its service manager restarts it; no timeout",
    ),
];

/// A child that leads a process group of its own is out of reach of a signal
/// to its pid alone: a timed-out guard whose shell forked its last command
/// left that grandchild running, holding the pipes the guard was handed.
/// `spawn_tree` is the one place a tree is set up, and it records the fact in
/// the `TreeKill` it returns, so the kill signals the group (Unix) or ends the
/// job (Windows). No caller of `spawn_tree` drops the kill it hands back.
///
/// A child is killed alone only on Unix, where it shares cfgd's foreground
/// process group so a prompt can read the terminal (`spawn_sharing_terminal`).
/// Console input on Windows does not depend on job membership, so there every
/// timeout kill goes through the job; the few lone kills left off Unix are
/// listed with their reasons in `LONE_KILLS_OFF_UNIX`.
#[test]
fn every_spawn_that_starts_a_process_tree_is_killed_as_one() {
    let fixture = "fn spawn_tree() {\n    cmd.process_group(0);\n}\n\nfn rogue() {\n    cmd.process_group(0);\n}\n\nfn suspended() {\n    cmd.creation_flags(CREATE_SUSPENDED);\n}\n\nfn quoted() {\n    // cmd.process_group(0);\n    let s = \"CREATE_SUSPENDED\";\n}\n\nfn drops() {\n    let (child, _kill) = crate::spawn_tree(&mut cmd)?;\n}\n\nfn first() {\n    let child = crate::spawn_tree(&mut cmd)?.0;\n}\n\nfn keeps() {\n    let (mut child, tree) = crate::spawn_tree(&mut cmd)?;\n}\n";
    let judged: Vec<(String, bool, bool)> = crate::test_helpers::fixture_declarations(fixture)
        .into_iter()
        .map(|(name, _, code)| {
            let code = crate::test_helpers::blank_non_code(&code);
            (
                name,
                starts_a_process_tree(&code),
                discards_the_tree_kill(&code),
            )
        })
        .collect();
    let expect = |name: &str, tree: bool, discard: bool| (name.to_string(), tree, discard);
    assert_eq!(
        judged,
        [
            expect("spawn_tree", true, false),
            expect("rogue", true, false),
            expect("suspended", true, false),
            expect("quoted", false, false),
            expect("drops", false, true),
            expect("first", false, true),
            expect("keeps", false, false),
        ],
        "the tells skip comments and literals and read a tree setup and a dropped kill in code"
    );

    let lone = "fn block() {\n    #[cfg(unix)]\n    {\n        let k = TreeKill::child_alone(pid);\n    }\n}\n\nfn statement() {\n    #[cfg(unix)]\n    let k = TreeKill::child_alone(child.id());\n}\n\nfn windows() {\n    #[cfg(windows)]\n    {\n        let _ = child.kill();\n    }\n}\n\nfn bare() {\n    let _ = child.kill();\n}\n\nfn after() {\n    #[cfg(unix)]\n    {\n        a();\n    }\n    let _ = child.kill();\n}\n\nfn stops() {\n    crate::terminate_process(pid);\n}\n\nfn quoted() {\n    // let _ = child.kill();\n    let s = \"terminate_process(\";\n}\n\npub fn child_alone(pid: u32) -> Self {\n    Self { pid }\n}\n";
    let judged: Vec<(String, bool)> = crate::test_helpers::fixture_declarations(lone)
        .into_iter()
        .map(|(name, _, code)| {
            let code = crate::test_helpers::blank_non_code(&code);
            let off_unix = kills_a_child_alone_off_unix(&code, false);
            (name, off_unix)
        })
        .collect();
    let expect = |name: &str, off_unix: bool| (name.to_string(), off_unix);
    assert_eq!(
        judged,
        [
            expect("block", false),
            expect("statement", false),
            expect("windows", true),
            expect("bare", true),
            expect("after", true),
            expect("stops", true),
            expect("quoted", false),
            expect("child_alone", false),
        ],
        "a lone kill counts as Unix-only inside the statement or block a `#[cfg(unix)]` gates"
    );
    assert!(
        !kills_a_child_alone_off_unix("let _ = child.kill();", true),
        "a declaration gated to Unix holds no lone kill off Unix"
    );
    assert!(
        declared_unix_only(&["", "#[cfg(unix)]", "/// doc", "#[test]"]),
        "a `#[cfg(unix)]` among the attributes gates the declaration"
    );
    assert!(
        !declared_unix_only(&["#[cfg(unix)]", "fn other() {}", "#[test]"]),
        "an attribute above an earlier item does not gate this one"
    );

    let workspace =
        crate::test_helpers::workspace_declarations(crate::test_helpers::WORKSPACE_CRATES);
    let mut tree_setups = Vec::new();
    let mut offenders = Vec::new();
    let mut listed_seen = std::collections::BTreeSet::new();
    let mut lone_offenders = Vec::new();
    for (row, (name, _, _)) in workspace.rows.iter().enumerate() {
        let code = workspace.code_of(row);
        let (_, path, body) = workspace.sites[row];
        let at = format!(
            "{}:{}",
            source_label(path),
            workspace.span_of(row).start() + 1
        );
        if starts_a_process_tree(&code) {
            if name == "spawn_tree" && path.ends_with("util/process.rs") {
                tree_setups.push(at.clone());
            } else {
                offenders.push(format!(
                    "{at}: `{name}` starts a process tree outside `spawn_tree`"
                ));
            }
        }
        if discards_the_tree_kill(&code) {
            offenders.push(format!(
                "{at}: `{name}` drops the kill `spawn_tree` returned"
            ));
        }
        let above: Vec<&str> = body.lines().take(*workspace.span_of(row).start()).collect();
        if kills_a_child_alone_off_unix(&code, declared_unix_only(&above)) {
            match LONE_KILLS_OFF_UNIX
                .iter()
                .position(|(file, func, _)| path.ends_with(file) && name == func)
            {
                Some(listed) => {
                    listed_seen.insert(listed);
                }
                None => lone_offenders.push(format!("{at}: `{name}`")),
            }
        }
    }
    assert!(
        lone_offenders.is_empty(),
        "these kill a child alone outside Unix-only code; on Windows a timeout kill \
         ends the job `spawn_sharing_terminal` or `spawn_tree` set up, so spawn \
         through one and kill through the `TreeKill` it returns:\n{}",
        lone_offenders.join("\n")
    );
    let stale: Vec<&str> = LONE_KILLS_OFF_UNIX
        .iter()
        .enumerate()
        .filter(|(i, _)| !listed_seen.contains(i))
        .map(|(_, (_, func, _))| *func)
        .collect();
    assert!(
        stale.is_empty(),
        "these listed lone kills no longer kill a child alone; drop them from \
         `LONE_KILLS_OFF_UNIX`: {stale:?}"
    );
    assert_eq!(
        tree_setups.len(),
        1,
        "the walk stopped seeing `spawn_tree` set up a tree: {tree_setups:?}"
    );
    assert!(
        offenders.is_empty(),
        "a child that leads its own process tree is killed through the \
         `TreeKill` `spawn_tree` returns; spawn through it and keep the kill:\n{}",
        offenders.join("\n")
    );
}

/// The function-open recognizer behind [`source_functions`], one case per
/// qualifier shape, so the next modifier added in front of a `fn` regresses
/// here instead of silently folding that function into its predecessor's
/// slice.
#[test]
fn the_function_open_recognizer_reads_every_qualifier_order() {
    let opens = [
        "fn f() {",
        "    fn indented() {",
        "pub fn f() {",
        "async fn f() {",
        "pub async fn f() {",
        "pub(crate) fn f() {",
        "pub(super) fn f() {",
        "pub(super) async fn f() {",
        "pub(in crate::daemon) fn f() {",
        "const fn f() {",
        "pub const fn f() {",
        "unsafe fn f() {",
        "pub(crate) const unsafe fn f() {",
        "async unsafe fn f() {",
        "extern \"C\" fn f() {",
        "pub unsafe extern \"C\" fn f() {",
        "pub extern fn f() {",
        "default fn f() {",
    ];
    for line in opens {
        assert!(opens_function(line), "must open a slice: {line}");
    }
    let not_opens = [
        "// pub fn f() {",
        "let f = make_fn();",
        "fn_table.insert(k, v);",
        "publish(fn_name);",
        "\"pub(super) async fn quoted in a fixture\",",
        "extern \"C\" {",
        "extern crate serde;",
        "pub struct Fn;",
        "pub(crate) mod tests;",
    ];
    for line in not_opens {
        assert!(!opens_function(line), "must not open a slice: {line}");
    }
}

/// A `fn `-shaped line inside a string literal is string BODY, not source: it
/// must not open a slice, or the enclosing function splits around it —
/// severing a folded tell from its opening line, or a tell from the producer
/// token that exempts it, both in the silent direction.
#[test]
fn a_fn_spelled_inside_a_string_literal_does_not_open_a_slice() {
    let body = concat!(
        "fn real() {\n",
        "    let raw = r#\"\n",
        "fn spelled_in_a_raw_literal() {\n",
        "\"#;\n",
        "    let cont = \"one \\\n",
        "fn spelled_in_a_continuation() {\";\n",
        "}\n",
        "fn second() {}\n"
    );
    let funcs = source_functions(&FIXTURE_SOURCE, body);
    assert_eq!(funcs.len(), 2, "{funcs:?}");
    assert_eq!(funcs[0].0, 1);
    assert!(
        funcs[0].1.contains("spelled_in_a_raw_literal")
            && funcs[0].1.contains("spelled_in_a_continuation"),
        "each literal stays inside its function's slice: {funcs:?}"
    );
    assert_eq!(funcs[1].0, 8);
}

/// The offender shape a PLAIN multi-line literal could hide: a fixture whose
/// banner spells `fn looks_like_an_fn() {` on a line of its own, with the
/// hand-spelled tell AFTER it. A false open there would put the tell in a
/// slice the literal's own text opened — reported off the fake line, or
/// exempted by whatever tokens that bogus slice happened to inherit — so the
/// offender must come back as ONE scan unit holding its opening line and its
/// tell together.
#[test]
fn an_offender_spelling_an_fn_line_inside_a_plain_literal_stays_one_scan_unit() {
    let tell = format!("join(\"{}\")", ".cfgd.env");
    let body = format!(
        "fn offender() {{\n    let banner = \"\nfn looks_like_an_fn() {{\n\";\n    \
         let p = home.{tell};\n}}\nfn sibling() {{}}\n"
    );
    let funcs = source_functions(&FIXTURE_SOURCE, &body);
    assert_eq!(funcs.len(), 2, "{funcs:?}");
    assert_eq!(funcs[0].0, 1, "the offender opens at its own line");
    assert!(
        funcs[0].1.contains("looks_like_an_fn") && funcs[0].1.contains(&tell),
        "the literal AND the tell stay inside the offender's slice: {funcs:?}"
    );
    assert_eq!(funcs[1].0, 7);
}

/// The comment and char-literal arms of [`LineMask`], each on the shape that
/// desyncs the scan if the arm breaks: a `//` cut keeps an unbalanced quote
/// in a comment from latching plain-string state, while a `//` INSIDE a
/// string cuts nothing (its closing quote still counts); a `'"'` opens no
/// string; `'\''` is consumed whole even hard against a following char
/// literal — searched from the escaped byte itself, `('\'','"')` swallows
/// the second literal's opening `'` and reads its `"` as a string opener; a
/// `'\''` before a real `"` still lets that quote open its string; and a
/// nested `/* /* */` block masks the lines inside it.
#[test]
fn the_masking_arms_read_comments_and_char_literals_as_not_source() {
    let body = concat!(
        "fn real() {\n",
        "    let odd = 1; // an unbalanced \" in a comment\n",
        "    let s = \"has a // inside\"; let q = '\"';\n",
        "    let pair = ('\\'','\"');\n",
        "    let esc = '\\''; let open = \"\n",
        "fn masked_by_the_open_string() {\n",
        "\";\n",
        "    /* outer /* nested */ still a comment\n",
        "fn masked_by_the_block_comment() {\n",
        "    */\n",
        "}\n",
        "fn after() {}\n"
    );
    let funcs = source_functions(&FIXTURE_SOURCE, body);
    assert_eq!(funcs.len(), 2, "{funcs:?}");
    assert_eq!(funcs[0].0, 1);
    assert!(
        funcs[0].1.contains("masked_by_the_open_string")
            && funcs[0].1.contains("masked_by_the_block_comment"),
        "every masked line stays inside the real function's slice: {funcs:?}"
    );
    assert_eq!(funcs[1].0, 12, "the sibling after the masks still opens");
}

/// A brace between `/*` and `*/` on one physical line is comment, not code.
///
/// That is the shape the workspace carries — an inline argument name
/// (`/*keep_when_empty=*/`) puts comment bytes in the middle of a line of
/// ordinary code — and counting a brace there moves where the scan thinks the
/// function closes: the slice runs on past its own `}` and swallows the
/// declaration below it, which then belongs to no slice and is judged by
/// nothing.
#[test]
fn a_brace_inside_a_one_line_block_comment_does_not_move_the_close() {
    let body = concat!(
        "fn real() {\n",
        "    let x = 1; /* { */\n",
        "    render(name, /*keep_when_empty=*/ true);\n",
        "}\n",
        "fn after() {}\n"
    );
    let funcs = source_functions(&FIXTURE_SOURCE, body);
    assert_eq!(funcs.len(), 2, "{funcs:?}");
    assert_eq!(funcs[0].0, 1);
    assert_eq!(
        funcs[0].1.lines().count(),
        4,
        "the slice ends at the function's own close: {:?}",
        funcs[0].1
    );
    assert_eq!(funcs[1].0, 5, "the sibling opens at its own line");
}

/// A declaration the brace scan cannot close stops the walk instead of
/// handing back a slice cut at the end of the input.
///
/// The fixture is what a desync looks like from here — a body the input ends
/// inside — and the alternative is a slice that merely LOOKS like a function,
/// judged as if it were whole. A caller reads whole files it does not author,
/// so the loud answer is the only one that cannot be believed by mistake.
///
/// The `expected` substring opens on the SOURCE the scan was handed, because
/// a walk reading the whole workspace panics from one file and a message
/// naming only a line number sends its reader to none of them.
#[test]
#[should_panic(
    expected = "<fixture>:2: the brace scan reached the end of the file without closing the declaration opened here"
)]
fn an_unbalanced_declaration_stops_the_walk() {
    source_functions(
        &FIXTURE_SOURCE,
        "// a header line\nfn f() {\n    let x = 1;\n",
    );
}

/// Every label [`source_label`] folds is workspace-relative and `/`-folded.
///
/// The fold the type cannot hold: [`SourceLabel`] closes which STRING a
/// scanner is handed, not what that string looks like. `workspace_root` is a
/// `..`-joined absolute path, so an unfolded label spells one file three ways
/// — with the `..` components here, with `\` between them on a Windows run,
/// and workspace-relative in a walk that strips the root — and a panic read
/// from a CI log matches none of the others.
#[test]
fn every_source_label_is_workspace_relative_and_posix_folded() {
    for path in workspace_rust_files() {
        let label = source_label(&path).to_string();
        assert!(
            !label.starts_with('/') && !label.contains('\\'),
            "the label of {} must be workspace-relative and `/`-folded: {label:?}",
            path.display()
        );
    }
}

/// A walk that folds a label for its scanner names its offender lines by that
/// same label.
///
/// The one shape [`SourceLabel`] does not close: nothing about the scanner's
/// parameter reaches the `format!` a walk builds its offender line with, so a
/// walk can label the file it read one way and report a finding in it another
/// — the exact defect the label exists to prevent, since a desync panic and
/// an offender line pasted out of one CI log then name two different paths.
///
/// Judged inside each declaration's own span, so the walks that fold no label
/// and spell their offender paths natively are outside the population rather
/// than hatched out of it. The needle is composed at run time because this
/// declaration would otherwise match itself.
#[test]
fn no_walk_folding_a_label_spells_an_offender_path_natively() {
    let own_path = workspace_root().join("crates/cfgd-core/src/output/tests/fences.rs");
    let own = walked_file_body(&own_path);
    let label = source_label(&own_path);
    let native = format!("path.{}()", "display");
    let mut folding = 0usize;
    let mut offenders = Vec::new();
    for (open, slice) in source_functions(&label, &own) {
        if !slice.contains("source_label(")
            || !(slice.contains("source_functions(")
                || slice.contains("const_items_outside_functions("))
        {
            continue;
        }
        folding += 1;
        if slice.contains(&native) {
            offenders.push(format!("{label}:{open}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a walk holding a `SourceLabel` renders that label in its offender \
         lines too, or it names one file two ways:\n{}",
        offenders.join("\n")
    );
    // The floor is the POPULATION: an empty offender list reads the same
    // whether the scan found every walk or none of them.
    assert!(
        folding >= 9,
        "the scan read {folding} label-folding walks; it has stopped seeing them"
    );
}

/// The producer-tell matcher reads whole identifiers: an identifier that
/// EXTENDS a producer's name exempts nothing, while the call, path and
/// pattern shapes around the real name still match.
#[test]
fn a_producer_tell_matches_only_a_whole_identifier() {
    let real_mentions = [
        ("let t = env_targets(", "env_targets"),
        ("EnvPlatform::Linux,", "EnvPlatform"),
        ("items.managed_env_files(&recorded)", "managed_env_files"),
        ("Action::WriteEnvFile { .. } => {}", "WriteEnvFile"),
        (
            "crate::reconciler::primary_env_file(home)",
            "primary_env_file",
        ),
        (
            "primary_env_file_path(home, platform)",
            "primary_env_file_path",
        ),
        ("env_targets", "env_targets"),
        ("items.managed_env_files", "managed_env_files"),
    ];
    for (func, ident) in real_mentions {
        assert!(
            names_identifier(func, ident),
            "{ident} must match in {func}"
        );
    }
    let extensions = [
        ("recorded_managed_env_files(state)", "managed_env_files"),
        ("neutralize_managed_env_files(scope)", "managed_env_files"),
        ("managed_env_files2(&files)", "managed_env_files"),
        ("primary_env_file_path(home, platform)", "primary_env_file"),
        ("fn env_targets_empty_yields_nothing() {", "env_targets"),
        (
            "generate_fish_env_content_basic()",
            "generate_fish_env_content",
        ),
    ];
    for (func, ident) in extensions {
        assert!(
            !names_identifier(func, ident),
            "{ident} must not match inside {func}"
        );
    }
}

/// How cfgd test code mutates the PROCESS-GLOBAL environment.
///
/// Every entry either writes an environment variable outright or installs a
/// guard that does. Matched as a substring of a call site's code half, so a
/// name covers the family spelled on top of it
/// (`install_named_path_shim_logged`, `ToolShim::install_failing_on`).
/// Deliberately absent: `Command::env(…)`, which hands a value to ONE child
/// and is precisely how a test avoids the race — keying on the variable's
/// name instead of on the mutation would flag those and train authors to
/// hatch the safe shape.
///
/// Kept honest from the other side by
/// [`every_env_mutating_test_helper_is_named_in_the_mutator_roster`], which
/// derives the helpers that reach a mutation from the test-helper sources
/// themselves: a helper added there and not named here fails that walk
/// instead of quietly uncounting every test that calls it.
const ENV_MUTATORS: &[&str] = &[
    "EnvVarGuard::set",
    "EnvVarGuard::unset",
    "clear_update_optouts",
    "EditorGuard::set",
    "ProbePath::containing",
    "install_named_path_shim",
    "with_test_env_var",
    "ToolShim::install",
    "NoHostManagers::pinned_missing",
    "CosignTestShim::install",
    "CosignTestShim::builder",
    "env::set_var",
    "env::remove_var",
];

const SERIAL_HATCH: &str = "serial-ok:";

/// Exempts an [`ENV_MUTATORS`] entry that matches no call site of its own.
const UNCALLED_HATCH: &str = "env-mutator-uncalled-ok:";

/// One function's source, from its `fn` line to its own closing brace.
///
/// Bounded by brace depth over the rest of the file rather than by the next
/// `fn` line: a helper declared INSIDE a test body would otherwise cut the
/// test's slice short and hide everything the test does after it. Braces are
/// counted on [`code_half`], so one inside a literal, behind a `//` or between
/// `/*` and `*/` does not move the depth.
///
/// The one brace it still counts is the CEILING: a `{` or `}` standing after
/// the inner close of a NESTED block comment on one physical line
/// (`/* outer /* inner */ { */`), where the line scan pairs the first `*/`
/// with the opening `/*` and rustc pairs it with the inner one. Nesting ACROSS
/// lines is not that shape — [`LineMask`] carries a depth there — and the
/// workspace spells every block comment it has as a single unnested one-line
/// span, so a line scan that counted nesting would be answering a question
/// nothing asks.
///
/// `head` is the declaration's own SPAN of `lines[open]`, which is the whole
/// line for an ordinary declaration and the remainder past the delimiter for
/// one sharing its line with a literal's close. A fresh mask handed that whole
/// line would read the literal's closing quote as a new string opener and stay
/// masked for the rest of the body, so the caller — which already holds the
/// mask state that found the declaration — hands the span it starts at.
///
/// The scan runs to the real close with no line ceiling. A ceiling returns a
/// slice that merely LOOKS like a function, and every tell below the cut is
/// then invisible to a walk that believes it read the whole body — the silent
/// direction. A scan that instead runs off the end of the file has desynced
/// (a brace inside a multi-line raw literal is the shape that does it), so it
/// panics naming `source`, the declaration it could not close and that
/// declaration's own text — the three facts a reader needs to open the file
/// and see the desync.
fn function_source(source: &SourceLabel, lines: &[&str], open: usize, head: &str) -> String {
    let mut mask = LineMask::default();
    let mut depth = 0i32;
    let mut opened = false;
    let below = lines[open + 1..].iter().copied();
    for (offset, line) in std::iter::once(head).chain(below).enumerate() {
        let code = mask.source_code(line);
        if !opened && !code.contains('{') && code.contains(';') {
            // A declaration with no body at all (a trait method's signature)
            // ends at its semicolon.
            return lines[open..=open + offset].join("\n");
        }
        depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
        opened |= code.contains('{');
        if opened && depth <= 0 {
            return lines[open..=open + offset].join("\n");
        }
    }
    panic!(
        "{source}:{}: the brace scan reached the end of the file without \
         closing the declaration opened here: {}",
        open + 1,
        lines[open].trim()
    );
}

/// The name a `fn` line declares.
fn declared_fn_name(slice: &str) -> Option<&str> {
    let first = slice.lines().next()?;
    let rest = &first[first.find("fn ")? + 3..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// Whether a declaration takes a receiver, i.e. is a method rather than a free
/// function. A method is never reached by the bare-name call below, so
/// following one would let ubiquitous names (`set`, `install`, `drop`) mark
/// every caller of anything as an env mutator.
fn takes_a_receiver(slice: &str) -> bool {
    let head: Vec<&str> = slice.lines().take(2).collect();
    let head = head.join(" ");
    let Some(at) = head.find('(') else {
        return false;
    };
    let args = head[at + 1..].trim_start();
    let args = args.strip_prefix('&').unwrap_or(args).trim_start();
    let args = args.strip_prefix("mut ").unwrap_or(args).trim_start();
    args.starts_with("self")
}

/// Whether `body` calls `name` as a bare function, not as `x.name(…)` or
/// `Type::name(…)`.
fn calls_by_bare_name(body: &str, name: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    body.match_indices(name).any(|(at, _)| {
        let prefix = &body[..at];
        let trimmed = prefix.trim_end();
        !prefix.chars().next_back().is_some_and(is_word)
            && !trimmed.ends_with('.')
            && !trimmed.ends_with(':')
            && body[at + name.len()..].trim_start().starts_with('(')
    })
}

/// Whether a function's own source reaches an [`ENV_MUTATORS`] entry.
///
/// Read through `logical_source_lines` so a `\`-continued call cannot hide its
/// needle across a line break, and through [`code_half`] so a needle spelled
/// inside a string literal or a comment is not source.
fn mutates_process_env(body: &str) -> bool {
    crate::test_helpers::logical_source_lines(body)
        .iter()
        .any(|(_, line)| {
            let code = code_half(line);
            ENV_MUTATORS.iter().any(|needle| code.contains(needle))
        })
}

/// The index of the first line of the contiguous attribute/comment block above
/// `open`.
fn attribute_block_start(lines: &[&str], open: usize) -> usize {
    let mut at = open;
    while at > 0 {
        let trimmed = lines[at - 1].trim_start();
        let is_attr = trimmed.starts_with("#[")
            || trimmed.starts_with("#!")
            || trimmed.starts_with("//")
            || trimmed.starts_with(")]")
            || trimmed.starts_with(']');
        if !is_attr {
            break;
        }
        at -= 1;
    }
    at
}

/// Serialized reachers each file must still yield: one row per file the walk
/// finds non-zero, floor = the count it finds there today.
///
/// Each floor is a minimum, so a fence cannot be removed from a file silently.
/// A file losing one reacher fails here: the failure mode a blinded needle
/// takes. A re-calibration REPLACES a row with what the scan now finds; it
/// never lowers one to make a failing check pass, because a count that fell is
/// the finding.
const SERIAL_FLOORS: &[(&str, usize)] = &[
    ("crates/cfgd-core/src/config/preferences.rs", 5),
    ("crates/cfgd-core/src/config/resolve.rs", 1),
    ("crates/cfgd-core/src/daemon/health_ipc.rs", 5),
    ("crates/cfgd-core/src/daemon/service/systemd.rs", 4),
    ("crates/cfgd-core/src/daemon/tests.rs", 40),
    ("crates/cfgd-core/src/modules/git.rs", 15),
    ("crates/cfgd-core/src/modules/tests.rs", 1),
    ("crates/cfgd-core/src/oci/auth/tests.rs", 12),
    ("crates/cfgd-core/src/oci/build.rs", 6),
    ("crates/cfgd-core/src/oci/pull.rs", 2),
    ("crates/cfgd-core/src/oci/sign/tests.rs", 25),
    ("crates/cfgd-core/src/oci/tests.rs", 2),
    ("crates/cfgd-core/src/output/printer.rs", 6),
    ("crates/cfgd-core/src/output/render_doc.rs", 3),
    ("crates/cfgd-core/src/output/tests/color_gate.rs", 1),
    ("crates/cfgd-core/src/output/tests/hyperlinks.rs", 5),
    ("crates/cfgd-core/src/output/tests/themes_raw.rs", 4),
    ("crates/cfgd-core/src/output/theme.rs", 8),
    ("crates/cfgd-core/src/platform/session.rs", 8),
    ("crates/cfgd-core/src/providers/skill/gemini.rs", 1),
    ("crates/cfgd-core/src/reconciler/managers.rs", 4),
    ("crates/cfgd-core/src/reconciler/scripts/tests.rs", 7),
    ("crates/cfgd-core/src/reconciler/tests.rs", 5),
    ("crates/cfgd-core/src/server_client/tests.rs", 2),
    ("crates/cfgd-core/src/sources/tests.rs", 43),
    ("crates/cfgd-core/src/test_helpers.rs", 17),
    ("crates/cfgd-core/src/tests.rs", 3),
    ("crates/cfgd-core/src/upgrade/check.rs", 24),
    ("crates/cfgd-core/src/upgrade/dedup.rs", 3),
    ("crates/cfgd-core/src/upgrade/tests.rs", 40),
    ("crates/cfgd-core/src/util/env_session.rs", 4),
    ("crates/cfgd-core/src/util/git.rs", 2),
    ("crates/cfgd-core/src/util/paths/tests.rs", 33),
    ("crates/cfgd-core/src/util/process.rs", 16),
    ("crates/cfgd-core/tests/skill_provider_io.rs", 2),
    ("crates/cfgd-core/tests/update_dedup.rs", 13),
    ("crates/cfgd-csi/src/app.rs", 2),
    ("crates/cfgd-csi/src/node/tests.rs", 12),
    ("crates/cfgd-operator/src/app.rs", 9),
    ("crates/cfgd-operator/src/controllers/tests.rs", 1),
    ("crates/cfgd-operator/src/env.rs", 14),
    ("crates/cfgd-operator/src/gateway/api/tests.rs", 3),
    ("crates/cfgd-operator/src/gateway/api/tests_router.rs", 18),
    ("crates/cfgd-operator/src/gateway/db/tests.rs", 4),
    ("crates/cfgd-operator/src/gateway/mod.rs", 11),
    ("crates/cfgd-operator/src/gateway/web/tests.rs", 11),
    ("crates/cfgd-operator/src/leader.rs", 12),
    ("crates/cfgd-operator/src/runtime.rs", 14),
    ("crates/cfgd-operator/src/test_helpers.rs", 3),
    ("crates/cfgd/src/ai/client.rs", 2),
    ("crates/cfgd/src/cli/checkin.rs", 9),
    ("crates/cfgd/src/cli/config_migration.rs", 5),
    ("crates/cfgd/src/cli/config_schema.rs", 1),
    ("crates/cfgd/src/cli/generate/tests.rs", 15),
    ("crates/cfgd/src/cli/helpers/tests.rs", 11),
    ("crates/cfgd/src/cli/image/pack.rs", 2),
    ("crates/cfgd/src/cli/init/tests.rs", 34),
    ("crates/cfgd/src/cli/kubectl.rs", 1),
    ("crates/cfgd/src/cli/module/build.rs", 1),
    ("crates/cfgd/src/cli/module/keys.rs", 9),
    ("crates/cfgd/src/cli/module/push_pull.rs", 5),
    ("crates/cfgd/src/cli/module/tests.rs", 10),
    ("crates/cfgd/src/cli/paths.rs", 11),
    ("crates/cfgd/src/cli/plan_ops/tests.rs", 3),
    ("crates/cfgd/src/cli/plugin/tests.rs", 7),
    ("crates/cfgd/src/cli/profile/tests.rs", 4),
    ("crates/cfgd/src/cli/registry.rs", 1),
    ("crates/cfgd/src/cli/source/remove.rs", 1),
    ("crates/cfgd/src/cli/status.rs", 5),
    ("crates/cfgd/src/cli/tests.rs", 113),
    ("crates/cfgd/src/cli/upgrade.rs", 14),
    ("crates/cfgd/src/cli/verify.rs", 1),
    ("crates/cfgd/src/files/tests.rs", 1),
    ("crates/cfgd/src/main.rs", 7),
    ("crates/cfgd/src/mcp/server/tests.rs", 1),
    ("crates/cfgd/src/packages/brew/tests.rs", 46),
    ("crates/cfgd/src/packages/cargo.rs", 11),
    ("crates/cfgd/src/packages/choco.rs", 14),
    ("crates/cfgd/src/packages/flatpak.rs", 9),
    ("crates/cfgd/src/packages/go.rs", 17),
    ("crates/cfgd/src/packages/nix.rs", 14),
    ("crates/cfgd/src/packages/npm.rs", 37),
    ("crates/cfgd/src/packages/pipx.rs", 10),
    ("crates/cfgd/src/packages/scoop.rs", 15),
    ("crates/cfgd/src/packages/shared/tests.rs", 31),
    ("crates/cfgd/src/packages/simple/tests.rs", 19),
    ("crates/cfgd/src/packages/snap.rs", 11),
    ("crates/cfgd/src/packages/tests.rs", 5),
    ("crates/cfgd/src/packages/versions/tests.rs", 32),
    ("crates/cfgd/src/packages/winget.rs", 8),
    ("crates/cfgd/src/secrets/age.rs", 8),
    ("crates/cfgd/src/secrets/tests.rs", 31),
    ("crates/cfgd/src/system/environment/tests.rs", 4),
    ("crates/cfgd/src/system/git_config.rs", 14),
    ("crates/cfgd/src/system/gpg_keys/tests.rs", 12),
    ("crates/cfgd/src/system/gsettings.rs", 6),
    ("crates/cfgd/src/system/kde_config.rs", 5),
    ("crates/cfgd/src/system/launch_agent.rs", 4),
    ("crates/cfgd/src/system/macos_defaults.rs", 3),
    ("crates/cfgd/src/system/node/tests.rs", 9),
    ("crates/cfgd/src/system/shell.rs", 14),
    ("crates/cfgd/src/system/systemd_unit.rs", 7),
    ("crates/cfgd/src/system/windows_registry.rs", 3),
    ("crates/cfgd/src/system/xfconf.rs", 4),
    ("crates/cfgd/tests/apply_snapshots.rs", 1),
    ("crates/cfgd/tests/backup_snapshots.rs", 2),
    ("crates/cfgd/tests/config_edit_snapshots.rs", 3),
    ("crates/cfgd/tests/image_pack_snapshots.rs", 1),
    ("crates/cfgd/tests/init_snapshots.rs", 3),
    ("crates/cfgd/tests/module_crud_snapshots.rs", 1),
    ("crates/cfgd/tests/module_keys_snapshots.rs", 7),
    ("crates/cfgd/tests/module_registry_snapshots.rs", 5),
    ("crates/cfgd/tests/module_search_snapshots.rs", 3),
    ("crates/cfgd/tests/module_upgrade_snapshots.rs", 4),
    ("crates/cfgd/tests/patch_strategy.rs", 4),
    ("crates/cfgd/tests/plan_snapshots.rs", 1),
    ("crates/cfgd/tests/plugin_deploy_snapshots.rs", 2),
    ("crates/cfgd/tests/plugin_snapshots.rs", 1),
    ("crates/cfgd/tests/profile_edit_snapshots.rs", 4),
    ("crates/cfgd/tests/profile_update_snapshots.rs", 1),
    ("crates/cfgd/tests/secret_snapshots.rs", 1),
    ("crates/cfgd/tests/source_add_snapshots.rs", 5),
    ("crates/cfgd/tests/source_edit_snapshots.rs", 3),
    ("crates/cfgd/tests/source_replace_snapshots.rs", 2),
    ("crates/cfgd/tests/source_update_snapshots.rs", 9),
    ("crates/cfgd/tests/sync_snapshots.rs", 6),
    ("crates/cfgd/tests/upgrade_snapshots.rs", 2),
];

/// A test that mutates the process-global environment runs under the serial
/// lock.
///
/// The workspace is edition 2024, where `std::env::set_var` is `unsafe`
/// because the C environment is not thread-safe: a write racing another
/// thread's read is undefined behaviour, not a flake, and `cargo test` runs
/// every test in one process on a thread pool. `serial_test`'s unnamed lock is
/// the workspace's answer, and a test reaching a mutation without joining it
/// is unsound however green it runs.
///
/// A test REACHES a mutation through its own body or through a same-file free
/// function it calls by bare name, transitively — a setup helper is where the
/// mutation usually lives. Methods are not followed: `set`, `install` and
/// `drop` name env-mutating associated items AND ordinary ones, and following
/// them would mark most of the suite.
///
/// `// serial-ok: <why>` on the declaration or in its attribute block exempts
/// a test whose mutation genuinely cannot race.
#[test]
fn every_test_mutating_the_process_environment_serializes_itself() {
    let root = workspace_root();
    let mut files_read = 0usize;
    let mut tests_seen = 0usize;
    let mut reaching = 0usize;
    let mut serialized = 0usize;
    let mut per_file: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut entry_hits: std::collections::BTreeMap<&str, usize> =
        ENV_MUTATORS.iter().map(|entry| (*entry, 0usize)).collect();
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        // This file spells every needle in order to hunt for it.
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: the tests judged here live in test regions.
        let body = walked_file_body(&path);
        if !body.contains("#[test]") && !body.contains("#[tokio::test") {
            continue;
        }
        files_read += 1;
        let lines: Vec<&str> = body.lines().collect();
        let relative = source_label(&path);

        // Every declaration in the file, by name: its sources (a name can be
        // declared more than once, in sibling modules or impl blocks), whether
        // any of them mutates, and whether it is reachable by a bare call.
        let mut sources: std::collections::BTreeMap<String, Vec<(usize, String)>> =
            std::collections::BTreeMap::new();
        let mut free: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
        for (open, slice) in source_functions(&relative, &body) {
            let Some(name) = declared_fn_name(&slice) else {
                continue;
            };
            let name = name.to_string();
            let free_here = !takes_a_receiver(&slice);
            free.entry(name.clone())
                .and_modify(|f| *f &= free_here)
                .or_insert(free_here);
            sources.entry(name).or_default().push((open, slice));
        }
        // A roster entry earns its place by COUNTING the source lines that CALL
        // it — inside the declarations this walk cut, never the file's own
        // `use` block, which names a helper without reaching it.
        // The files built only for tests are where the helpers live, so their
        // mentions prove nothing.
        //
        // Cutting to declarations is what makes an import not a call, and it is
        // also this count's ceiling: a needle reached from file scope — a macro
        // body, a `const` initializer — sits outside every declaration and is
        // not seen.
        //
        // The fold does not depend on the entry, so it happens once for the
        // file's declarations rather than once per entry per declaration.
        if !crate::test_helpers::is_test_only_file(&path) {
            let code: Vec<String> = sources
                .values()
                .flatten()
                .flat_map(|(_, src)| crate::test_helpers::logical_source_lines(src))
                .map(|(_, line)| code_half(&line))
                .collect();
            for (entry, hits) in &mut entry_hits {
                *hits += code.iter().filter(|line| line.contains(*entry)).count();
            }
        }
        let mut reaches: std::collections::BTreeMap<String, bool> = sources
            .iter()
            .map(|(name, decls)| {
                (
                    name.clone(),
                    decls.iter().any(|(_, src)| mutates_process_env(src)),
                )
            })
            .collect();
        // Transitive closure over bare calls to same-file free functions.
        loop {
            let mut grew = false;
            let seeds: Vec<String> = reaches
                .iter()
                .filter(|(name, hit)| **hit && free.get(*name).copied().unwrap_or(false))
                .map(|(name, _)| name.clone())
                .collect();
            for (name, decls) in &sources {
                if reaches.get(name).copied().unwrap_or(false) {
                    continue;
                }
                let hit = seeds.iter().any(|seed| {
                    seed != name && decls.iter().any(|(_, src)| calls_by_bare_name(src, seed))
                });
                if hit {
                    reaches.insert(name.clone(), true);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }

        for (name, decls) in &sources {
            for (open, _) in decls {
                let start = attribute_block_start(&lines, open - 1);
                let attrs = &lines[start..open - 1];
                let is_test = attrs.iter().any(|l| {
                    let t = l.trim_start();
                    t.starts_with("#[test]") || t.starts_with("#[tokio::test")
                });
                if !is_test {
                    continue;
                }
                tests_seen += 1;
                if !reaches.get(name).copied().unwrap_or(false) {
                    continue;
                }
                reaching += 1;
                if attrs
                    .iter()
                    .any(|l| l.trim_start().starts_with("#[") && l.contains("serial"))
                {
                    serialized += 1;
                    *per_file.entry(relative.to_string()).or_default() += 1;
                    continue;
                }
                if (start..*open).any(|at| hatched(&lines, at, SERIAL_HATCH)) {
                    continue;
                }
                offenders.push(format!("{relative}:{open}: {name}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a test mutating the process-global environment must carry \
         `#[serial_test::serial…]`, or `// serial-ok: <why>` saying why its \
         mutation cannot race:\n{}",
        offenders.join("\n")
    );
    // Floors, so a walk that stopped matching anything cannot pass silently.
    assert!(
        files_read > 100 && tests_seen > 5_000,
        "the walk read {files_read} files and {tests_seen} tests; it has stopped seeing the suite"
    );
    assert!(
        reaching > 500 && serialized > 500,
        "the walk found {reaching} tests reaching a mutation, {serialized} of them serialized; \
         it has stopped seeing the mutations"
    );
    // An entry matching nothing is the silent uncounting this walk exists to
    // prevent, worn as a green roster: it satisfies the derived walk while
    // watching no test at all.
    let own_path = root.join("crates/cfgd-core/src/output/tests/fences.rs");
    // unfloored-slice-ok: the roster and its hatches live in this file's tests.
    let own = walked_file_body(&own_path);
    let own_lines: Vec<&str> = own.lines().collect();
    let uncounted: Vec<&str> = entry_hits
        .iter()
        .filter(|(entry, hits)| **hits == 0 && !roster_entry_hatched(&own_lines, entry))
        .map(|(entry, _)| *entry)
        .collect();
    assert!(
        uncounted.is_empty(),
        "every `ENV_MUTATORS` entry must match a call site outside \
         `test_helpers.rs`, or carry `// env-mutator-uncalled-ok: <why>` — \
         an entry counting nothing watches nothing: {uncounted:?}"
    );
    for (file, floor) in SERIAL_FLOORS {
        let found = per_file.get(*file).copied().unwrap_or(0);
        assert!(
            found >= *floor,
            "{file} yielded {found} serialized reachers, under its floor of {floor} — \
             the walk has gone blind in that file"
        );
    }
    // A floor only guards a file it names, so the table has to be the whole
    // non-zero set: a file with no row is a file whose count may fall to zero
    // unwatched, and a row whose file no longer yields anything is a floor
    // guarding nothing.
    let floored: std::collections::BTreeSet<&str> =
        SERIAL_FLOORS.iter().map(|(file, _)| *file).collect();
    let missing: Vec<String> = per_file
        .iter()
        .filter(|(file, found)| **found > 0 && !floored.contains(file.as_str()))
        .map(|(file, found)| format!("    (\"{file}\", {found}),"))
        .collect();
    let stale: Vec<&str> = floored
        .iter()
        .filter(|file| per_file.get(**file).copied().unwrap_or(0) == 0)
        .copied()
        .collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "`SERIAL_FLOORS` must name every file the walk finds non-zero and no other.\n\
         Rows to add:\n{}\nRows naming a file that now yields nothing: {stale:?}",
        missing.join("\n")
    );
}

/// Every test guard that pins a process-global seam: the call a pin is written
/// as, the serial group the guard's own rustdoc names, the UNPINNED reader whose
/// answer only holds inside that group, and the number of pin call sites it
/// watches today. An empty group is `serial_test`'s unnamed lock.
///
/// The pin is a needle rather than a type name because a seam whose guard is
/// private to one file is reached through a helper call (`with_test_elevated`)
/// rather than a `Type::` constructor, and one column holding both shapes is
/// what lets this table name every seam in the workspace.
///
/// The reader is the accessor that answers the override. A test asserting what
/// it returns with NOTHING pinned is the designated victim of every pin on that
/// seam, so it belongs to the same group as the pins themselves; a test reading
/// the seam's CONSTANT is not in the class, the constant being the value an
/// override displaces rather than the override.
///
/// A guard that takes the lock itself (`GitRefreshWindowGuard`) or needs no
/// serialization at all (`CommandPathMemoTtlGuard`) is deliberately absent, as
/// are the env-var guards, whose serialization
/// [`every_test_mutating_the_process_environment_serializes_itself`] demands
/// instead.
///
/// Not every seam is a numeric ceiling. The process-global tracing journal
/// ([`crate::test_helpers::install_tracing_journal`]) is one as well, and it has
/// no guard: one buffer holds what every thread logs, one wrapper per binary
/// clears it, and the daemon-loop tests read it back. Its `set_global_default`
/// lives in `test_helpers.rs`, a file built only for tests, which this walk
/// skips, so the pin is the wrapper the journal is cleared through and the
/// reader is the journal read itself.
///
/// A declaration that STARTS A DAEMON is a writer of that journal and joins the
/// group too, which the `run_daemon_with` and `run_daemon_loop` rows are: a
/// reader waiting on the startup banner as proof its OWN daemon installed a
/// signal handler reads a sibling's banner as its own otherwise, raises SIGTERM
/// before any handler exists, and the default disposition kills the whole test
/// process.
///
/// A SCOPED capture (`tracing::subscriber::with_default`, `WithSubscriber`) is
/// in neither class and joins no group: it replaces one thread's own default for
/// the length of a closure, so what another thread registers reaches neither its
/// buffer nor its verdict — provided the journal is already installed under it,
/// which is what makes the per-callsite interest cache unable to strand an event
/// either way. That order is held by
/// [`every_scoped_tracing_capture_installs_the_journal_under_it`] instead, and a
/// declaration whose only tracing reach is such a capture carries no serial
/// attribute at all.
const SERIAL_PINS: &[(&str, &str, &str, usize)] = &[
    (
        "AvailabilityMemoTtlGuard::",
        "",
        "availability_memo_ttl(",
        4,
    ),
    (
        "AvailableVersionMemoTtlGuard::",
        "available_version_memo",
        "available_version_memo_ttl(",
        5,
    ),
    (
        "ConfigReuseMaxAgeGuard::",
        "tick_cache_reuse",
        "config_reuse_max_age(",
        1,
    ),
    (
        "EnumerationMemoTtlGuard::",
        "enumeration_memo",
        "enumeration_memo_ttl(",
        13,
    ),
    (
        "ModuleReuseTtlGuard::",
        "tick_cache_reuse",
        "module_reuse_ttl(",
        1,
    ),
    (
        "RateLimitedBackoffGuard::",
        "rate_limited_backoff",
        "BackoffConfig::rate_limited(",
        3,
    ),
    (
        "TickCountingHooks {",
        "tick_cache_reuse",
        "TickCountingHooks {",
        5,
    ),
    (
        "fn reset_daemon_log",
        "tracing_dispatcher",
        "daemon_log()",
        1,
    ),
    ("lower_soft_nofile(", "", "raise_soft_nofile_toward(", 2),
    (
        "runner::run_daemon_loop(",
        "tracing_dispatcher",
        "wait_for_daemon_log(",
        12,
    ),
    (
        "super::super::run_daemon_with(",
        "tracing_dispatcher",
        "run_daemon(",
        10,
    ),
    ("with_test_elevated", "", "effective_elevated(", 14),
];

/// Exempts one declaration from the walk below, with the reason after it.
const SERIAL_GROUP_HATCH: &str = "serial-group-ok:";

/// Whether the attribute line `line` joins the serial group `group` — the EXACT
/// group, because `serial_test` locks per name and two groups serialize nothing
/// against each other. An empty `group` is the unnamed lock, which is a whole
/// attribute rather than a prefix: `#[serial_test::serial]` joins it and
/// `#[serial_test::serial(other)]` does not.
fn joins_serial_group(line: &str, group: &str) -> bool {
    let attr = code_half(line);
    let attr = attr.trim();
    if !attr.starts_with("#[") {
        return false;
    }
    if group.is_empty() {
        return attr == "#[serial_test::serial]" || attr == "#[serial]";
    }
    attr.contains(&format!("serial({group})"))
}

/// A test whose verdict depends on a serialized seam joins that seam's group.
///
/// Most [`SERIAL_PINS`] seams are one process-global `AtomicU64` a guard
/// overrides, saving the value it found and restoring it on drop. Two of them
/// live at once is not a flake but a lost override: the second pin captures the
/// FIRST one's value as the one to restore, so the seam stays pinned for the
/// rest of the binary and every later test reads a ceiling nobody set. The
/// group that prevents it is named in the guard's own rustdoc, which no
/// compiler reads, and it has to be the same name every other caller wrote — a
/// pin serialized under a group of its own serializes against nothing.
///
/// A test READING the unpinned seam is in the same class one property over: its
/// whole claim is that nothing is pinned, which a concurrent pin displaces, so
/// the roster's reader column is demanded of `#[test]` declarations too. A
/// production call site is no declaration the walk can demand an attribute of
/// and is left alone.
///
/// The walk judges the DECLARATION a pin is written in, so a pin inside a shared
/// helper is an offender like an unserialized test: the helper's callers are not
/// visible here, and a helper that pins is a helper every caller must serialize.
/// A pin outside every declaration is reported per file for the same reason.
/// `// serial-group-ok: <why>` on the declaration or in its attribute block
/// exempts one.
///
/// Its subject is TEST code, so unlike a production walk it reads each file
/// whole rather than through `production_slice_of`: the cut would drop every
/// pin, each one living in a `#[cfg(test)]` module. A file it cannot read is a
/// file it cannot judge, so an unreadable one fails the walk outright.
#[test]
fn every_test_pinning_a_serialized_seam_joins_its_own_group() {
    let mut files_read = 0usize;
    let mut hits: std::collections::BTreeMap<&str, usize> = SERIAL_PINS
        .iter()
        .map(|(pin, _, _, _)| (*pin, 0usize))
        .collect();
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        // This file spells every needle in order to hunt for it, and
        // the files built only for tests are where the guards are declared.
        if path.ends_with(Path::new("output/tests/fences.rs"))
            || crate::test_helpers::is_test_only_file(&path)
        {
            continue;
        }
        let labelled = source_label(&path);
        // unfloored-slice-ok: the declarations judged here are tests.
        let body = walked_file_body(&path);
        if !SERIAL_PINS
            .iter()
            .any(|(pin, _, reader, _)| body.contains(pin) || body.contains(reader))
        {
            continue;
        }
        files_read += 1;
        let lines: Vec<&str> = body.lines().collect();
        let relative = labelled;

        let mut attributed = 0usize;
        for (open, slice) in source_functions(&relative, &body) {
            let code: Vec<String> = crate::test_helpers::logical_source_lines(&slice)
                .into_iter()
                .map(|(_, line)| code_half(&line))
                .collect();
            let start = attribute_block_start(&lines, open - 1);
            let attrs = &lines[start..open - 1];
            let is_test = attrs.iter().any(|l| {
                let t = l.trim_start();
                t.starts_with("#[test]") || t.starts_with("#[tokio::test")
            });
            let name = declared_fn_name(&slice).unwrap_or("<unnamed>").to_string();
            for (pin, group, reader, _) in SERIAL_PINS {
                let found = code.iter().filter(|line| line.contains(pin)).count();
                let reads = is_test && code.iter().any(|line| line.contains(reader));
                if found == 0 && !reads {
                    continue;
                }
                *hits.entry(*pin).or_insert(0) += found;
                attributed += found;
                if (start..open).any(|at| hatched(&lines, at, SERIAL_GROUP_HATCH)) {
                    continue;
                }
                if is_test && attrs.iter().any(|l| joins_serial_group(l, group)) {
                    continue;
                }
                let wanted = if group.is_empty() {
                    "#[serial_test::serial]".to_string()
                } else {
                    format!("#[serial_test::serial({group})]")
                };
                if found > 0 {
                    offenders.push(format!(
                        "{relative}:{open}: {name} pins {pin} without {wanted}"
                    ));
                }
                if reads {
                    offenders.push(format!(
                        "{relative}:{open}: {name} reads the unpinned {reader} without {wanted}"
                    ));
                }
            }
        }
        // A pin written outside every declaration is serialized by no attribute
        // at all, and the pass above reads declarations only. A type's own
        // declaration is not one: a needle naming a fixture TYPE matches the
        // `struct` line that defines it and every `impl` block written for it,
        // none of which a `#[serial_test::serial…]` can be hung on, and
        // narrowing the needle to dodge them (`(TickCountingHooks {`) leaves
        // the construction spelled without the call — `let hooks =
        // TickCountingHooks { … }` — matching nothing at all.
        let loose: usize = crate::test_helpers::logical_source_lines(&body)
            .into_iter()
            .map(|(_, line)| code_half(&line))
            .filter(|code| !declares_a_type(code))
            .map(|code| {
                SERIAL_PINS
                    .iter()
                    .filter(|(pin, _, _, _)| code.contains(*pin))
                    .count()
            })
            .sum();
        if loose > attributed {
            offenders.push(format!(
                "{relative}: {} pin(s) outside every function declaration",
                loose - attributed
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "a declaration pinning or reading a serialized seam must carry that \
         seam's own `#[serial_test::serial…]` group, or `// serial-group-ok: <why>`:\n{}",
        offenders.join("\n")
    );
    // Floors, so a walk that stopped matching anything cannot pass silently.
    assert!(
        files_read >= 15,
        "the walk read {files_read} files holding a pin or a reader; it has stopped seeing them"
    );
    // Each needle's floor is a workspace-wide minimum; a needle whose count drops under it fails.
    for (pin, _, _, floor) in SERIAL_PINS {
        let found = hits.get(*pin).copied().unwrap_or(0);
        assert!(
            found >= *floor,
            "{pin} matched {found} lines, under its floor of {floor} — \
             the walk has gone blind to it"
        );
    }
}

/// The serial group an attribute line joins, `""` for `serial_test`'s unnamed
/// lock, or `None` for a line that is no serial attribute.
fn serial_group_of(attr: &str) -> Option<String> {
    let attr = attr.trim();
    if attr == "#[serial_test::serial]" || attr == "#[serial]" {
        return Some(String::new());
    }
    let rest = attr
        .strip_prefix("#[serial_test::serial(")
        .or_else(|| attr.strip_prefix("#[serial("))?;
    Some(rest.split(')').next().unwrap_or_default().to_string())
}

/// Every pair of serial attributes `body` writes on one declaration, and the
/// ones written in the other order, each named by the line the second attribute
/// of the pair sits on.
///
/// Split out of the walk below so a fixture can prove the scan classifies an
/// inversion as one: the walk's own subject is every source in the workspace,
/// which holds none by construction once the walk is green.
fn serial_lock_order_offenders(body: &str) -> (usize, Vec<String>) {
    let named = |group: &str| {
        if group.is_empty() {
            "unnamed".to_string()
        } else {
            group.to_string()
        }
    };
    let mut pairs = 0usize;
    let mut offenders = Vec::new();
    let mut held: Option<String> = None;
    for (nth, line) in body.lines().enumerate() {
        let code = code_half(line);
        let trimmed = code.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with("#[") {
            held = None;
            continue;
        }
        let Some(group) = serial_group_of(trimmed) else {
            continue;
        };
        if let Some(previous) = held.replace(group.clone()) {
            pairs += 1;
            if previous > group {
                offenders.push(format!(
                    "{}: takes the {} lock before the {} one",
                    nth + 1,
                    named(&previous),
                    named(&group)
                ));
            }
        }
    }
    (pairs, offenders)
}

/// A declaration carrying two `serial_test::serial` attributes takes both locks
/// in one order, the unnamed one first and named ones alphabetically.
///
/// Two attributes are two locks, taken in the order they are written: the first
/// attribute expands around the rest. A declaration writing them the other way
/// round holds lock B while it waits for A, against a sibling holding A and
/// waiting for B, and both tests hang until the harness is killed — no timeout
/// fires, because neither is waiting on anything it can see. One inverted pair
/// hung five `select_loop_*` declarations and every test behind them in the
/// unnamed lock's queue, which a full parallel run reported only as `has been
/// running for over 60 seconds`.
///
/// There is no hatch: an order is arbitrary, and the whole value of this one is
/// that every declaration writes the same one.
#[test]
fn every_declaration_taking_two_serial_locks_takes_them_in_one_order() {
    let mut pairs = 0usize;
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        // This file spells the attribute in fixtures rather than wearing it.
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        let labelled = source_label(&path);
        // unfloored-slice-ok: the declarations judged here are tests.
        let body = walked_file_body(&path);
        if !body.contains("serial_test::serial") {
            continue;
        }
        let (seen, found) = serial_lock_order_offenders(&body);
        pairs += seen;
        offenders.extend(found.into_iter().map(|at| format!("{labelled}:{at}")));
    }

    assert!(
        offenders.is_empty(),
        "a declaration taking two serial locks takes them in the order every \
         other declaration does — the unnamed lock first, named groups \
         alphabetically — or the two deadlock against each other:\n{}",
        offenders.join("\n")
    );
    // A floor, so a walk that stopped matching anything cannot pass silently.
    assert!(
        pairs >= 8,
        "the walk saw {pairs} declarations taking two serial locks; it has gone blind to them"
    );
}

/// The order is what the scan judges, and a declaration writing the canonical
/// one is no offender however many locks it takes.
#[test]
fn the_serial_lock_order_scan_reads_an_inverted_pair_as_the_offence() {
    let (pairs, offenders) = serial_lock_order_offenders(
        "#[tokio::test]\n#[serial_test::serial(tracing_dispatcher)]\n#[serial_test::serial]\nasync fn a() {}\n\
         #[tokio::test]\n#[serial_test::serial]\n#[serial_test::serial(tracing_dispatcher)]\nasync fn b() {}\n",
    );
    assert_eq!(pairs, 2, "both declarations take two locks");
    assert_eq!(
        offenders.len(),
        1,
        "and only the inverted one is an offence: {offenders:?}"
    );
    assert!(
        offenders[0].starts_with("3: takes the tracing_dispatcher lock before the unnamed one"),
        "named by line and by both groups: {offenders:?}"
    );
}

/// The spellings a test binds a scoped tracing subscriber with. Each is a line
/// the process-global journal must already be installed before.
const SCOPED_CAPTURE_BINDS: &[&str] = &["tracing::subscriber::with_default(", ".with_subscriber("];

/// Exempts one scoped bind from the walk below, with the reason after it.
const JOURNAL_FLOOR_HATCH: &str = "journal-floor-ok:";

/// Every scoped tracing capture installs the process-global journal under it
/// first.
///
/// `tracing` caches one `Interest` per callsite for the whole process and
/// computes it from what the REGISTERING thread can see, so while a single
/// dispatcher is registered, a callsite first reached from a thread holding no
/// subscriber at all caches `never` — and every later event there is dropped
/// until an unrelated registration rebuilds the cache, including the event a
/// capture on another thread is waiting for.
/// [`crate::test_helpers::install_tracing_journal`] carries the rest of the
/// mechanism; what this walk keeps is the ORDER, a floor installed after the
/// bind being one the cached verdict already escaped.
///
/// A site handing on a dispatcher it was given (`spawn_blocking_with_test_home`)
/// binds no capture of its own and is not in the class.
/// `// journal-floor-ok: <why>` on the bind's line or the one above exempts one.
#[test]
fn every_scoped_tracing_capture_installs_the_journal_under_it() {
    let mut binds = 0usize;
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        // This file spells every needle in order to hunt for it, and
        // the files built only for tests declare the floor itself.
        if path.ends_with(Path::new("output/tests/fences.rs"))
            || crate::test_helpers::is_test_only_file(&path)
        {
            continue;
        }
        let labelled = source_label(&path);
        // unfloored-slice-ok: the declarations judged here are tests.
        let body = walked_file_body(&path);
        if !SCOPED_CAPTURE_BINDS.iter().any(|bind| body.contains(bind)) {
            continue;
        }
        let lines: Vec<&str> = body.lines().collect();
        for (open, slice) in source_functions(&labelled, &body) {
            let code: Vec<(usize, String)> = crate::test_helpers::logical_source_lines(&slice)
                .into_iter()
                .map(|(at, line)| (at, code_half(&line)))
                .collect();
            let installed = code
                .iter()
                .position(|(_, line)| line.contains("install_tracing_journal("));
            for (nth, (at, line)) in code.iter().enumerate() {
                if !SCOPED_CAPTURE_BINDS.iter().any(|bind| line.contains(bind)) {
                    continue;
                }
                binds += 1;
                let absolute = open + at - 1;
                if hatched(&lines, absolute - 1, JOURNAL_FLOOR_HATCH) {
                    continue;
                }
                if installed.is_some_and(|first| first < nth) {
                    continue;
                }
                let name = declared_fn_name(&slice).unwrap_or("<unnamed>");
                offenders.push(format!(
                    "{labelled}:{absolute}: {name} binds a scoped subscriber with no \
                     `install_tracing_journal()` above it"
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a scoped tracing capture reads back empty when a callsite cached \
         `never`; install the process-global journal first, or say why with \
         `// journal-floor-ok: <why>`:\n{}",
        offenders.join("\n")
    );
    // A floor, so a walk that stopped matching anything cannot pass silently.
    assert!(
        binds >= 6,
        "the walk saw {binds} scoped binds; it has gone blind to them"
    );
}

/// No item outside a function body writes the process environment.
///
/// The walks above are FUNCTION-scoped: they cut a file into
/// [`source_functions`] slices and read those, so a `const` or `static`
/// declared between two of them is read by neither
/// [`every_test_mutating_the_process_environment_serializes_itself`] nor
/// [`every_env_mutating_test_helper_is_named_in_the_mutator_roster`]. A
/// `LazyLock` initializer calling `EnvVarGuard::set` would write the
/// environment of every test in the binary — ordered by first use rather than
/// by any test, serialized by no attribute, and named in no offender list.
///
/// The population is empty today, so this walk keeps it empty rather than
/// teaching four walks to attribute a file-scope write to tests that never
/// mention it. `// env-mutator-ok: <why>` above the declaration hatches a
/// write that is not process-global, as it does for a helper.
/// Every production read of `PATH` sits inside a `path_env_read_guard()` span.
///
/// `PATH` is process-global and the test suite empties it to drive
/// command-not-found branches, so an unguarded read answers from whatever
/// window it lands in: a walk let through a mutation window reports every
/// package manager on the machine missing, and its caller acts on that at
/// once. The gate is what makes a read and a mutation unable to overlap, and
/// a reader that never takes it is outside the gate whatever the readers
/// beside it do.
///
/// The judgement is the INNERMOST declaration the read sits in, and the guard
/// has to be named on a code line of that declaration: it is held to the end
/// of the span it was taken in, and an enclosing declaration's guard says
/// nothing about a nested one that runs on its own. A guard taken by a callee
/// is deliberately not accepted — the span it holds is the callee's, and a
/// caller reading `PATH` before or after that call reads it unguarded. A
/// mention inside a comment or a string literal vouches for nothing, the
/// needle being read off [`code_half`].
///
/// A read outside every declaration — a file-scope `static` or `LazyLock`
/// initializer — is counted and FAILS, as
/// [`no_item_outside_a_function_body_mutates_the_process_environment`]
/// holds the mutation half: an initializer runs ordered by first use, inside
/// no span any guard could bracket. The needle is the two `env::var`
/// spellings, which is the walk's ceiling — a read through a
/// `use std::env::var` import would go unseen, and nothing in the workspace
/// writes one.
///
/// `// path-read-ok: <why>` on the read's own line, or the line above it,
/// hatches a reader whose crate cannot name the `test-helpers` feature the
/// guard is gated on.
#[test]
fn every_production_path_read_takes_the_read_guard() {
    const HATCH: &str = "path-read-ok:";
    // The literal is blanked out of the code half, so the variable NAME is read
    // off the raw line and only its `env::var` call off the half that proves it
    // is code at all.
    let (reads, offenders) = unguarded_env_reads(
        |line, code| line.contains("\"PATH\"") && ENV_READ_NEEDLES.iter().any(|n| code.contains(n)),
        HATCH,
    );
    assert!(
        offenders.is_empty(),
        "a production read of `PATH` must sit in a span that takes \
         `path_env_read_guard()`, or carry `// {HATCH} <why>`:\n{}",
        offenders.join("\n")
    );
    // cfgd-core's three: `reconciler/scripts.rs`'s script PATH and
    // `util/process.rs`'s lookup and prepend. Brew's PATH composition routes
    // through `process_path_with_dirs_prepended` and reads nothing of its own,
    // and no other crate reads PATH directly.
    const WALK_ROOTS: &[(&str, usize)] = &[("cfgd-core", 3)];
    for (root, floor) in WALK_ROOTS {
        let found = reads.get(*root).copied().unwrap_or(0);
        assert!(
            found >= *floor,
            "the walk found {found} production `PATH` reads under {root}, below its floor of \
             {floor}; it has stopped finding them"
        );
    }
}

/// Every production read of a `CFGD_*_BIN` tool seam sits inside a
/// `path_env_read_guard()` span, as a `PATH` read does.
///
/// A test pins a seam process-wide (`NoHostManagers`, `ToolShim`) under
/// `path_env_mutation_guard`, so an unguarded read on another thread answers
/// from that window: a manager whose seam is pinned missing there reports this
/// host's real binary absent, and a plan running beside the pin finds no
/// manager at all.
///
/// A seam read is an `env::var` call naming a `*_BIN_ENV` const or a
/// `tool_seam_var(..)` derivation, or reading a parameter spelled `env_var`,
/// the name every cfgd-core seam helper takes its seam by. That spelling is the
/// walk's ceiling: a seam read through another name goes unseen.
/// `// unseamed-read-ok: <why>` on the read's line, or the line above it,
/// hatches an `env_var` parameter that names no tool.
#[test]
fn every_production_seam_read_takes_the_read_guard() {
    const HATCH: &str = "unseamed-read-ok:";
    let (reads, offenders) = unguarded_env_reads(
        |_, code| {
            ENV_READ_NEEDLES.iter().any(|n| code.contains(n))
                && (code.contains("BIN_ENV")
                    || code.contains("tool_seam_var(")
                    || code.contains("var(env_var)")
                    || code.contains("var_os(env_var)"))
        },
        HATCH,
    );
    assert!(
        offenders.is_empty(),
        "a production read of a tool seam must sit in a span that takes \
         `path_env_read_guard()`, or carry `// {HATCH} <why>`:\n{}",
        offenders.join("\n")
    );
    // cfgd-core: the three `util/process.rs` seam helpers and the hatched
    // systemd directory read in `util/paths.rs`. cfgd: the ten package and
    // system readers that take the guard. cfgd-operator: the hatched lease
    // timing read in `leader.rs`.
    const WALK_ROOTS: &[(&str, usize)] = &[("cfgd-core", 4), ("cfgd", 10), ("cfgd-operator", 1)];
    for (root, floor) in WALK_ROOTS {
        let found = reads.get(*root).copied().unwrap_or(0);
        assert!(
            found >= *floor,
            "the walk found {found} production seam reads under {root}, below its floor of \
             {floor}; it has stopped finding them"
        );
    }
}

/// The two `env::var` spellings a guarded-read walk looks for.
const ENV_READ_NEEDLES: &[&str] = &["env::var(", "env::var_os("];

/// Every production line `is_read` accepts (handed the raw line and its
/// [`code_half`]), counted per crate (the directory under `crates/`), and the
/// ones outside a span naming
/// `path_env_read_guard()` that carry no `hatch`, labelled.
///
/// The judgement is the INNERMOST declaration the read sits in, and the guard
/// has to be named on a code line of that declaration: it is held to the end
/// of the span it was taken in, and an enclosing declaration's guard says
/// nothing about a nested one that runs on its own. A guard taken by a callee
/// is deliberately not accepted, since the span it holds is the callee's. A
/// read outside every declaration (a file-scope `static` or `LazyLock`
/// initializer) is counted and fails: an initializer runs ordered by first use,
/// inside no span any guard could bracket.
fn unguarded_env_reads(
    is_read: impl Fn(&str, &str) -> bool,
    hatch: &str,
) -> (std::collections::BTreeMap<String, usize>, Vec<String>) {
    let mut reads = std::collections::BTreeMap::<String, usize>::new();
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        // This file spells every needle in order to hunt for it, and the test
        // corpus mutates the environment on purpose.
        if path.ends_with(Path::new("output/tests/fences.rs"))
            || crate::test_helpers::is_test_source(&path)
            || crate::test_helpers::is_test_only_file(&path)
        {
            continue;
        }
        // The guard is taken under `cfg(any(test, feature = "test-helpers"))`,
        // so the production half holds the read without the guard: each span is
        // read whole, and a read is judged on every row a test can drive, a
        // `test-helpers` seam's included.
        // unfloored-slice-ok: the test-only guard statements are part of every span judged.
        let body = walked_file_body(&path);
        let gates = crate::test_helpers::line_gates_of(&path);
        let lines: Vec<&str> = body.lines().collect();
        let relative = source_label(&path);
        // Each declaration as the line range it covers and whether its own
        // code names the guard. Both ends are 0-based indices of `lines`, off
        // a 1-based opening line and a slice holding at least the declaration
        // itself.
        let spans: Vec<(std::ops::RangeInclusive<usize>, bool)> =
            source_functions(&relative, &body)
                .into_iter()
                .map(|(open, slice)| {
                    let guarded = slice
                        .lines()
                        .any(|line| code_half(line).contains("path_env_read_guard()"));
                    ((open - 1)..=(open + slice.lines().count() - 2), guarded)
                })
                .collect();
        // Line by line rather than slice by slice: `source_functions` yields
        // OVERLAPPING slices, so a read inside a nested declaration counted
        // once per enclosing slice would inflate the floor a deletion has to
        // clear.
        for (at, line) in lines.iter().enumerate() {
            if gates[at] == Some(crate::test_helpers::Gate::Test)
                || !is_read(line, &code_half(line))
            {
                continue;
            }
            let label = relative.to_string();
            let root = label.strip_prefix("crates/").unwrap_or(&label);
            *reads
                .entry(root.split('/').next().unwrap_or(root).to_string())
                .or_default() += 1;
            let innermost = spans
                .iter()
                .filter(|(span, _)| span.contains(&at))
                .min_by_key(|(span, _)| span.end() - span.start());
            if innermost.is_some_and(|(_, guarded)| *guarded) || hatched(&lines, at, hatch) {
                continue;
            }
            offenders.push(format!("{relative}:{}: {}", at + 1, line.trim()));
        }
    }
    (reads, offenders)
}

#[test]
fn no_item_outside_a_function_body_mutates_the_process_environment() {
    let mut items = 0usize;
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        // This file spells every needle in order to hunt for it.
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        // unfloored-slice-ok: an item anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        let relative = source_label(&path);
        for (open, item) in const_items_outside_functions(&relative, &body) {
            items += 1;
            if !mutates_process_env(&item) || hatched(&lines, open - 1, MUTATOR_HATCH) {
                continue;
            }
            offenders.push(format!("{relative}:{open}: {}", lines[open - 1].trim()));
        }
    }
    assert!(
        offenders.is_empty(),
        "an item outside every function body must not write the process \
         environment — no test can serialize against it and no walk can \
         attribute it:\n{}",
        offenders.join("\n")
    );
    // The floor is the POPULATION, not the offenders: an empty offender list
    // reads the same whether the walk saw every item or none of them, and the
    // scan it sees them through is the one `source_functions` keeps changing.
    // Set AT what the workspace holds rather than under it, the way
    // `SERIAL_FLOORS` is: a margin is exactly the room a slice change needs to
    // stop seeing a few files in silence.
    assert!(
        items >= 668,
        "the walk read {items} items outside a function body; it has stopped \
         seeing the workspace's declarations"
    );
}

/// Whether the roster's own line for `entry` carries an
/// [`UNCALLED_HATCH`] reason.
///
/// The hatch has to sit on the ROSTER's line, inside the `ENV_MUTATORS` slice
/// itself. A needle this file also spells in a neighbouring table would
/// otherwise be exempted by a hatch on THAT line, which is a hatch no reader
/// of the roster would ever see.
fn roster_entry_hatched(lines: &[&str], entry: &str) -> bool {
    let Some(open) = lines
        .iter()
        .position(|line| line.starts_with("const ENV_MUTATORS"))
    else {
        panic!("no `const ENV_MUTATORS` declaration");
    };
    let Some(len) = lines[open..].iter().position(|line| line.starts_with("];")) else {
        panic!("`ENV_MUTATORS` is never closed");
    };
    let quoted = format!("\"{entry}\",");
    (open..open + len).any(|at| lines[at].contains(&quoted) && hatched(lines, at, UNCALLED_HATCH))
}

/// The primitive writes a helper's own body can carry, from which reaching is
/// derived. `EnvVarGuard`'s two constructors are primitives rather than
/// derived helpers: they are the guard every other one is built on, and
/// deriving them from their own `unsafe` block would make the seed set a
/// restatement of `std::env`.
const ENV_MUTATION_SEEDS: &[&str] = &[
    "env::set_var",
    "env::remove_var",
    "EnvVarGuard::set",
    "EnvVarGuard::unset",
];

/// Helpers the derivation must still find, so a walk that has stopped
/// resolving calls fails instead of demanding nothing.
const DERIVED_CALIBRATION: &[&str] = &[
    "install_named_path_shim",
    "install_named_path_shim_logged",
    "EditorGuard::set",
    "ProbePath::containing",
];

const MUTATOR_HATCH: &str = "env-mutator-ok:";

/// The type name an `impl` line opens a block for, as a call site spells it:
/// the implementing type, not the trait, and without its path or generics.
fn impl_type_name(code: &str) -> Option<String> {
    let rest = code.strip_prefix("impl")?.trim_start();
    let rest = match rest.strip_prefix('<') {
        Some(generics) => generics.split_once('>')?.1.trim_start(),
        None => rest,
    };
    let rest = match rest.split_once(" for ") {
        Some((_, implementing)) => implementing.trim_start(),
        None => rest,
    };
    let path: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    let name = path.rsplit("::").next()?.to_string();
    (!name.is_empty()).then_some(name)
}

/// The type each line sits inside the `impl` block of, or `None` outside one.
///
/// Only a block opened in column zero counts: a nested `impl` belongs to a
/// function body, and the qualified name a call site spells is the top-level
/// one. The owner is dropped when a block that was open closes, so a
/// multi-line `impl` header keeps it.
fn impl_owners(lines: &[&str]) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut owner: Option<String> = None;
    let mut depth = 0i32;
    // Braces inside a multi-line literal are not source, and an owner column
    // that has desynced names every declaration below it after the wrong type.
    let mut mask = LineMask::default();
    for line in lines {
        let code = mask.source_code(line);
        if depth == 0 && code.starts_with("impl") {
            owner = impl_type_name(&code);
        }
        out.push(owner.clone());
        let before = depth;
        depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
        if depth <= 0 {
            depth = 0;
            if before > 0 {
                owner = None;
            }
        }
    }
    out
}

/// Whether `body` calls `name` — as a bare call, a method call or an
/// associated one, the three spellings being indistinguishable without type
/// resolution. Widening that way is safe only because the caller follows a
/// name ONLY when every declaration of it reaches a mutation.
fn calls_named(body: &str, name: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    body.match_indices(name).any(|(at, _)| {
        !body[..at].chars().next_back().is_some_and(is_word)
            && body[at + name.len()..].trim_start().starts_with('(')
    })
}

/// Every test helper that writes the process environment is named in
/// [`ENV_MUTATORS`].
///
/// The serial walk above reaches ACROSS files only through that list, so a
/// helper missing from it uncounts every test in the workspace whose only
/// mutation is the one it installs — silently, and the more useful the helper
/// the more tests go unwatched. A hand-maintained list is exactly the shape
/// that drifts, so the roster is checked against a derivation from the
/// test-helper sources themselves.
///
/// A helper REACHES a mutation through one of [`ENV_MUTATION_SEEDS`] in its
/// own body, or through a same-file declaration it calls whose name ANY
/// declaration of reaches — a call is followed by name alone, the bare, method
/// and associated spellings being indistinguishable without type resolution.
/// Names are shared (`set`, `install` and `drop` name env-mutating associated
/// items and ordinary ones alike), so the widening derives helpers that write
/// no environment variable, and each of those carries a hatch saying so at its
/// own declaration. The narrower rule — follow only a name EVERY declaration
/// of which reaches — needed no hatches and left no trace: a helper whose only
/// route is a shared name went underived, so nothing demanded it and nothing
/// said why.
///
/// Scoped to the files built only for tests (`is_test_only_file`), where the
/// helpers live. Run over every source instead, the same derivation adds exactly
/// one public reacher — CLI startup code that writes `XDG_CONFIG_HOME` before
/// any thread exists — which is a production concern with its own safety
/// argument, not a helper a test calls.
///
/// `// env-mutator-ok: <why>` exempts a helper that writes no environment
/// variable of its own, or whose every call site another roster entry already
/// counts.
#[test]
fn every_env_mutating_test_helper_is_named_in_the_mutator_roster() {
    let mut derived: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut files_read = 0usize;
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        if !crate::test_helpers::is_test_only_file(&path) {
            continue;
        }
        files_read += 1;
        // The seam view: a `test`-gated item's own tests reach every seed and
        // would be derived as helpers themselves, and a `test-helpers` item is
        // a helper a test drives.
        let body = crate::test_helpers::test_module_cut_of(&path);
        let lines: Vec<&str> = body.lines().collect();
        let owners = impl_owners(&lines);
        let relative = source_label(&path);

        let mut names: Vec<String> = Vec::new();
        let mut qualified: Vec<String> = Vec::new();
        let mut opens: Vec<usize> = Vec::new();
        let mut sources: Vec<String> = Vec::new();
        let mut exported: Vec<bool> = Vec::new();
        for (open, slice) in source_functions(&relative, &body) {
            let Some(name) = declared_fn_name(&slice).map(str::to_string) else {
                continue;
            };
            let owner = owners[open - 1].clone();
            qualified.push(match &owner {
                Some(ty) => format!("{ty}::{name}"),
                None => name.clone(),
            });
            names.push(name);
            exported.push(
                crate::test_helpers::item_lead(lines[open - 1]).0
                    == crate::test_helpers::ItemLead::Visible,
            );
            sources.push(slice);
            opens.push(open);
        }

        let mut reaches: Vec<bool> = sources
            .iter()
            .map(|src| {
                crate::test_helpers::logical_source_lines(src)
                    .iter()
                    .any(|(_, line)| {
                        let code = code_half(line);
                        ENV_MUTATION_SEEDS.iter().any(|seed| code.contains(seed))
                    })
            })
            .collect();
        loop {
            // The REACHING declarations, each carrying its own index: what a
            // follow must not count is the declaration it is scanning, not
            // every declaration sharing that name — a helper whose overload
            // in another impl writes the environment reaches through it.
            let followable: Vec<(usize, &str)> = names
                .iter()
                .enumerate()
                .filter(|(at, _)| reaches[*at])
                .map(|(at, name)| (at, name.as_str()))
                .collect();
            let mut grew = false;
            for at in 0..sources.len() {
                if reaches[at] {
                    continue;
                }
                if followable
                    .iter()
                    .any(|(from, name)| *from != at && calls_named(&sources[at], name))
                {
                    reaches[at] = true;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }

        for at in 0..sources.len() {
            if !reaches[at] || !exported[at] {
                continue;
            }
            derived.insert(qualified[at].clone());
            let named = ENV_MUTATORS
                .iter()
                .any(|entry| qualified[at].contains(entry) || names[at].contains(entry));
            if named || hatched(&lines, opens[at] - 1, MUTATOR_HATCH) {
                continue;
            }
            offenders.push(format!("{relative}:{}: {}", opens[at], qualified[at]));
        }
    }

    assert!(
        offenders.is_empty(),
        "a public test helper that writes the process environment must be \
         named in `ENV_MUTATORS`, or every test whose only mutation it is \
         goes uncounted by the serial walk — add the name, or \
         `// env-mutator-ok: <why>` if the write is not process-global:\n{}",
        offenders.join("\n")
    );
    assert!(
        files_read >= 2,
        "the walk read {files_read} test-helper sources; it has stopped finding them"
    );
    for name in DERIVED_CALIBRATION {
        assert!(
            derived.contains(*name),
            "the derivation no longer finds {name}; it has stopped resolving calls \
             (found: {derived:?})"
        );
    }
    assert!(
        derived.len() >= 10,
        "the derivation found {} env-mutating helpers: {derived:?}",
        derived.len()
    );
}

/// A walk that reads several sources reads each one through
/// [`crate::test_helpers::production_slice_of`], or through
/// [`crate::test_helpers::floored_production_body`], which own both halves the walk needs: the
/// read that must not be swallowed, and the per-file floor on what the cut
/// returned.
///
/// The pure [`crate::test_helpers::production_slice`] takes a body, so a caller
/// reaching it inside a loop has already read the file itself and can only
/// carry the floor by hand — which is how the same block came to be copied,
/// and how most walks came to carry no floor at all.
/// A caller that genuinely holds one compiled-in body and no path keeps the pure
/// cut and says so with `// unfloored-slice-ok: <why>` on that line or the one
/// above it. A walk over the files built only for tests reads their region
/// through [`crate::test_helpers::test_module_cut_of`], the floored seam view
/// with no test-only guard.
///
/// A source walk is a test-scope function calling `rust_sources_under`,
/// `workspace_rust_files`, `is_test_source` or `is_test_only_file`, or one
/// enumerating `.rs` files itself with `read_dir`. A source walk reads each
/// production file through `production_slice_of` or `floored_production_body`,
/// or through their siblings on the same floored scan, `production_and_seams_of`
/// for the text a test can drive and `production_code_of` for the production
/// text with its literals and comments blanked; when it reads a whole file,
/// inline `#[cfg(test)]` included, it calls
/// [`crate::test_helpers::walked_file_body`] and carries
/// `// unfloored-slice-ok: <why>` (on that line or the one above). A raw
/// `read_to_string` in a source walk fails whatever it carries. The reads
/// through `floored_production_body` and `production_and_seams_of` are floored
/// per crate at the count each holds, so walks drifting off the floored helper
/// fail here.
///
/// Where a file's test text ends is the one scanner's answer too. A test-scope
/// line searching a string for a test gate's spelling (`.starts_with("#[cfg(test)]")`,
/// `.find("cfg(any(test")`, `== "#[cfg(test)]"`) or for a test module's head
/// (`.contains("mod tests")`) decides that cut by itself, and a second cut
/// disagrees with the first on every shape it was not written for: a composite
/// gate, an item beside production code, a `test-helpers` seam. Such a walk
/// reads [`crate::test_helpers::test_region_of`], `line_gates_of` or
/// `attribute_gate` instead. `test_helpers.rs` is where the scanner
/// lives, so its own test module is outside this search.
#[test]
fn every_multi_file_production_walk_reads_through_the_floored_helper() {
    // Spelled in parts, or this walk's own needles are the first offenders it
    // finds.
    let needle = concat!("production_", "slice");
    let hatch = concat!("unfloored-", "slice-ok:");
    let walk_calls = [
        concat!("rust_sources", "_under("),
        concat!("workspace_rust", "_files("),
        concat!("is_test", "_source("),
        concat!("is_test_only", "_file("),
    ];
    let enumerates_sources = |func: &str| {
        func.contains(concat!("read", "_dir("))
            && (func.contains(concat!("\"", "rs\"")) || func.contains(concat!("\".", "rs\"")))
    };
    let raw_read = concat!("read_to", "_string(");
    let whole_read = concat!("walked_file", "_body(");
    // The reads through the floored helper, and through its seam-view sibling,
    // per crate root, each floored at the count it holds today.
    let floored_reads = [
        concat!("floored_production", "_body("),
        concat!("production_and_seams", "_of("),
    ];
    // Per crate root, each floored at the count it holds today: the functions
    // walking sources, the sources spelling the pure cut, and the reads
    // through the floored helper or its seam-view sibling.
    const FLOORS: [(&str, usize, usize, usize); 3] = [
        ("cfgd", 82, 8, 35),
        ("cfgd-core", 64, 10, 4),
        ("cfgd-operator", 2, 2, 0),
    ];
    let mut hand_cuts = Vec::new();
    let mut unparsed = Vec::new();
    let crates_dir = workspace_root().join("crates");
    let mut floored: std::collections::BTreeMap<String, usize> = Default::default();
    let mut offenders = Vec::new();
    let mut raw_reads = Vec::new();
    let mut walks: std::collections::BTreeMap<String, usize> = Default::default();
    let mut sources: std::collections::BTreeMap<String, usize> = Default::default();
    for path in workspace_rust_files() {
        let root = path
            .strip_prefix(&crates_dir)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        // `test_helpers.rs` holds the shared walk BODIES, so exempting the file
        // would hide the newest multi-file walk from this rule. Only its own
        // PRODUCTION region is judged, because the unit tests of the cut below
        // hold one body each and legitimately call the pure form, and a
        // declaration line names the cut rather than reaching it.
        let own_file = crate::to_posix_string(&path).ends_with("cfgd-core/src/test_helpers.rs");
        let label = source_label(&path);
        let body = if own_file {
            crate::test_helpers::test_module_cut_of(&path).into()
        } else {
            // unfloored-slice-ok: the walks judged here are tests.
            walked_file_body(&path)
        };
        let lines: Vec<&str> = body.lines().collect();
        let mut spells = false;
        for (n, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let code = line.trim_start();
            if line.contains(" fn ") || code.starts_with("fn ") {
                continue;
            }
            if floored_reads.iter().any(|read| line.contains(read)) {
                *floored.entry(root.clone()).or_default() += 1;
            }
            if !line.contains(needle) {
                continue;
            }
            spells = true;
            // The bare identifier, judged by both its edges: a neighbouring
            // identifier character is a floored helper (`production_slice_of`)
            // or one of its own test names, and anything else is the pure cut
            // reached by name — called, handed to a `map`, or imported under a
            // name this walk would never see.
            let bare = line.match_indices(needle).any(|(at, _)| {
                let after = line[at + needle.len()..].chars().next();
                let before = line[..at].chars().next_back();
                !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            if !bare {
                continue;
            }
            let above = n.checked_sub(1).map(|i| lines[i]).unwrap_or_default();
            if carries_hatch(line, hatch) || carries_hatch(above, hatch) {
                continue;
            }
            offenders.push(format!("{label}:{}: {}", n + 1, line.trim()));
        }
        if spells {
            *sources.entry(root.clone()).or_default() += 1;
        }
        // Production code reading files is outside the rule: only test scope
        // walks sources.
        let whole_test = own_file
            || crate::test_helpers::is_test_source(&path)
            || crate::test_helpers::is_test_only_file(&path);
        let gates = if whole_test {
            Default::default()
        } else {
            crate::test_helpers::line_gates_of(&path)
        };
        let in_test = |n: usize| whole_test || gates.get(n).is_some_and(Option::is_some);
        if !own_file {
            match syntax_of(&path) {
                Ok(syntax) => {
                    for n in hand_cut_gate_rows(syntax, in_test) {
                        hand_cuts.push(format!("{label}:{}: {}", n + 1, lines[n].trim()));
                    }
                }
                Err(e) => unparsed.push(format!("{label}: {e}")),
            }
        }
        for (open, func) in source_functions(&label, &body) {
            // `open` is 1-based and the slice starts on the declaration's line.
            if !in_test(open - 1)
                || !(walk_calls.iter().any(|call| func.contains(call)) || enumerates_sources(&func))
            {
                continue;
            }
            *walks.entry(root.clone()).or_default() += 1;
            for (k, line) in func.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") || crate::test_helpers::opens_function(code) {
                    continue;
                }
                let n = open - 1 + k;
                if line.contains(raw_read) {
                    raw_reads.push(format!("{label}:{}: {}", n + 1, line.trim()));
                    continue;
                }
                if !line.contains(whole_read) {
                    continue;
                }
                let above = n.checked_sub(1).map(|i| lines[i]).unwrap_or_default();
                if !(carries_hatch(line, hatch) || carries_hatch(above, hatch)) {
                    raw_reads.push(format!("{label}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "read the file through `cfgd_core::test_helpers::production_slice_of` \
         or `floored_production_body`, which read it and floor the \
         cut at the lines preceding its test module, or say why one body needs \
         the pure cut with `// unfloored-slice-ok: <why>`; a walk over the files \
         built only for tests reads through `test_module_cut_of`:\n{}",
        offenders.join("\n")
    );
    assert!(
        raw_reads.is_empty(),
        "a source walk reads each file through `production_slice_of`, \
         `floored_production_body` or `test_module_cut_of`; a walk whose subject \
         is the whole file reads it through `walked_file_body` and says why with \
         `// unfloored-slice-ok: <why>`:\n{}",
        raw_reads.join("\n")
    );
    assert!(
        unparsed.is_empty(),
        "a source `syn` cannot parse is a source the hand-cut walk cannot judge:\n{}",
        unparsed.join("\n")
    );
    assert!(
        hand_cuts.is_empty(),
        "a test-scope search for a test gate's spelling cuts test text from \
         production beside the one scanner; read the file's test region through \
         `test_region_of`, its gates through `line_gates_of`, or judge one \
         attribute with `attribute_gate`:\n{}",
        hand_cuts.join("\n")
    );
    let unfloored: Vec<&String> = walks
        .keys()
        .chain(sources.keys())
        .chain(floored.keys())
        .filter(|root| FLOORS.iter().all(|(floored_root, ..)| floored_root != root))
        .collect();
    assert!(
        unfloored.is_empty(),
        "a crate holding source walks takes a row in FLOORS at the count it holds: {unfloored:?}"
    );
    let count = |counts: &std::collections::BTreeMap<String, usize>, root: &str| {
        counts.get(root).copied().unwrap_or_default()
    };
    let short: Vec<String> = FLOORS
        .iter()
        .flat_map(|&(root, walk_floor, source_floor, read_floor)| {
            [
                ("source walks", count(&walks, root), walk_floor),
                (
                    "sources spelling the pure cut",
                    count(&sources, root),
                    source_floor,
                ),
                ("floored reads", count(&floored, root), read_floor),
            ]
            .into_iter()
            .filter(|&(_, found, floor)| found < floor)
            .map(move |(what, found, floor)| format!("{root}: {found} {what}, floor {floor}"))
        })
        .collect();
    assert!(
        short.is_empty(),
        "the walk has stopped reading the population it judges:\n{}\n\
         walks: {walks:?}\nsources: {sources:?}\nfloored reads: {floored:?}",
        short.join("\n")
    );
}

/// The spellings a test gate opens on, split so this file spells none whole.
const GATE_SPELLINGS: [&str; 4] = [
    concat!("cfg", "(test"),
    concat!("cfg", "(all(test"),
    concat!("cfg", "(any(test"),
    concat!("mod ", "tests"),
];

/// Whether `text` opens on a test gate's spelling, or on the attribute holding
/// one.
fn opens_on_a_gate(text: &str) -> bool {
    let rest = text
        .strip_prefix("#![")
        .or_else(|| text.strip_prefix("#["))
        .unwrap_or(text);
    GATE_SPELLINGS
        .iter()
        .any(|spelling| rest.starts_with(spelling))
}

/// The rows of `syntax` in test scope (`in_test`) holding a string search, or a
/// `==` / `!=` comparison, whose needle reaches a literal opening on a test
/// gate's spelling. Each cuts test text from production beside the one scanner.
fn hand_cut_gate_rows(syntax: &Syntax, in_test: impl Fn(usize) -> bool) -> Vec<usize> {
    let mut rows: Vec<usize> = syntax
        .searches
        .iter()
        .filter(|site| in_test(site.row))
        .filter(|site| {
            syntax
                .reach(&site.reads)
                .literals
                .iter()
                .any(|literal| opens_on_a_gate(literal))
        })
        .map(|site| site.row)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// The rows of `syntax` in test scope (`in_test`) holding a `read_to_string`
/// call whose path reaches a literal naming a `.rs` file and anchors at the
/// workspace: a literal holding `CARGO_MANIFEST_DIR`, or a call of
/// `workspace_root`.
fn workspace_source_reads(syntax: &Syntax, in_test: impl Fn(usize) -> bool) -> Vec<usize> {
    let mut rows: Vec<usize> = syntax
        .reads
        .iter()
        .filter(|site| in_test(site.row))
        .filter(|site| {
            let reached = syntax.reach(&site.reads);
            let anchored = reached
                .literals
                .iter()
                .any(|literal| literal.contains("CARGO_MANIFEST_DIR"))
                || reached.calls.contains(&"workspace_root");
            anchored
                && reached
                    .literals
                    .iter()
                    .any(|literal| literal.ends_with(".rs"))
        })
        .map(|site| site.row)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// The string searches a hand cut hands a needle to, named as the method.
const SEARCH_METHODS: [&str; 9] = [
    "starts_with",
    "ends_with",
    "contains",
    "find",
    "rfind",
    "strip_prefix",
    "split_once",
    "matches",
    "match_indices",
];

/// What an expression reads in the scope it is written in: the string literals
/// it holds, the functions it calls, the local bindings it names, the names no
/// local binding in scope holds, which a `const` or `static` item may, and the
/// reads of the subexpressions already worked out.
#[derive(Default)]
struct Reads {
    literals: Vec<String>,
    calls: Vec<String>,
    locals: Vec<usize>,
    items: Vec<String>,
    parts: Vec<std::sync::Arc<Reads>>,
}

/// A call a walk judges: the 0-based row its call node starts on, and what the
/// argument it judges reads.
struct Site {
    row: usize,
    reads: std::sync::Arc<Reads>,
}

/// Everything [`Reads`] reaches through the bindings it names, and theirs in
/// turn.
struct Reached<'s> {
    literals: Vec<&'s str>,
    calls: Vec<&'s str>,
}

/// The facts the walks judge about one Rust source, read off its `syn` tree.
///
/// Every binding a pattern declares is recorded with what it holds:
/// - a `let` (and its `else`), `if let`, `while let` or `match` arm pattern
///   holds its initializer or scrutinee, and a later `x = …` assignment adds to
///   what `x` holds;
/// - a `for` pattern holds the expression it iterates;
/// - a closure's parameters hold the receiver of the method call it is handed
///   to, the other arguments of the function call it is handed to, or the
///   arguments of every call of the local it is bound to;
/// - a function's parameters, `self` aside, hold the argument in their place at
///   every call of that function in the file, whether called by path or as a
///   method;
/// - a `const` or `static` item holds its value and is read by name anywhere
///   in the file.
///
/// Macro arguments are read as expressions, or as statements, and otherwise as
/// the literals, names and calls among their tokens; a name a string literal
/// captures as `{name}` is read too.
#[derive(Default)]
struct Syntax {
    /// What each binding holds, indexed by binding.
    sources: Vec<Vec<std::sync::Arc<Reads>>>,
    /// The bindings each `const` or `static` name declares.
    items: std::collections::HashMap<String, Vec<usize>>,
    /// Needles handed to a string search, and the sides of a comparison.
    searches: Vec<Site>,
    /// Paths handed to `read_to_string`, under any name `use` gives it.
    reads: Vec<Site>,
}

impl Syntax {
    fn reach<'s>(&'s self, reads: &'s Reads) -> Reached<'s> {
        let mut reached = Reached {
            literals: Vec::new(),
            calls: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        let mut frontier = vec![reads];
        while let Some(reads) = frontier.pop() {
            reached
                .literals
                .extend(reads.literals.iter().map(String::as_str));
            reached.calls.extend(reads.calls.iter().map(String::as_str));
            frontier.extend(reads.parts.iter().map(|part| &**part));
            let items = reads
                .items
                .iter()
                .filter_map(|name| self.items.get(name))
                .flatten();
            for &binding in reads.locals.iter().chain(items) {
                if seen.insert(binding) {
                    frontier.extend(self.sources[binding].iter().map(|reads| &**reads));
                }
            }
        }
        reached
    }
}

/// The [`Syntax`] of `body`, or the parse error naming the line it stops on.
fn syntax(body: &str) -> Result<Syntax, String> {
    let built = syn::parse_file(body)
        .map(|file| {
            let mut aliases = ReadAliases(vec!["read_to_string".to_string()]);
            syn::visit::Visit::visit_file(&mut aliases, &file);
            let mut builder = Builder {
                syntax: Syntax::default(),
                scopes: vec![Vec::new()],
                functions: Default::default(),
                calls: Vec::new(),
                closures: Default::default(),
                read_names: aliases.0,
                memo: Default::default(),
                macros: Default::default(),
            };
            syn::visit::Visit::visit_file(&mut builder, &file);
            builder.finish()
        })
        .map_err(|e| format!("line {}: {e}", e.span().start().line));
    // Every span this parse minted is dropped with the tree, and the thread's
    // span table would otherwise hold each parsed file for the whole process.
    proc_macro2::extra::invalidate_current_thread_spans();
    built
}

/// The [`Syntax`] of the workspace source at `path`, from the body
/// [`walked_file_body`] holds. Every workspace source is parsed on the first
/// call, spread over the machine's cores, since parsing is most of a walk.
fn syntax_of(path: &std::path::Path) -> &'static Result<Syntax, String> {
    type Parsed = std::collections::HashMap<PathBuf, Result<Syntax, String>>;
    static PARSED: std::sync::LazyLock<Parsed> = std::sync::LazyLock::new(|| {
        let files = workspace_rust_files();
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let chunk = files.len().div_ceil(cores).max(1);
        std::thread::scope(|scope| {
            let parsers: Vec<_> = files
                .chunks(chunk)
                .map(|paths| {
                    scope.spawn(move || {
                        let parse = |path: &PathBuf| {
                            // unfloored-slice-ok: the tree of the whole file is the subject.
                            (path.clone(), syntax(&walked_file_body(path)))
                        };
                        paths.iter().map(parse).collect::<Vec<_>>()
                    })
                })
                .collect();
            parsers
                .into_iter()
                .flat_map(|parser| {
                    parser
                        .join()
                        .unwrap_or_else(|e| std::panic::resume_unwind(e))
                })
                .collect()
        })
    });
    PARSED
        .get(path)
        .unwrap_or_else(|| panic!("{}: not a workspace source", path.display()))
}

/// The 0-based row a span starts on.
fn row_of(span: proc_macro2::Span) -> usize {
    span.start().line.saturating_sub(1)
}

/// `expr` past any `&` and parentheses around it.
fn peel(expr: &syn::Expr) -> &syn::Expr {
    match expr {
        syn::Expr::Reference(reference) => peel(&reference.expr),
        syn::Expr::Paren(paren) => peel(&paren.expr),
        _ => expr,
    }
}

/// What a macro's arguments parse as.
enum MacroBody {
    Exprs(Vec<syn::Expr>),
    Stmts(Vec<syn::Stmt>),
    Tokens,
}

fn macro_body(mac: &syn::Macro) -> MacroBody {
    use syn::parse::Parser;
    let exprs = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    if let Ok(exprs) = exprs.parse2(mac.tokens.clone()) {
        return MacroBody::Exprs(exprs.into_iter().collect());
    }
    syn::Block::parse_within
        .parse2(mac.tokens.clone())
        .map_or(MacroBody::Tokens, MacroBody::Stmts)
}

/// The names `use` gives `read_to_string`.
struct ReadAliases(Vec<String>);

impl<'ast> syn::visit::Visit<'ast> for ReadAliases {
    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if rename.ident == "read_to_string" {
            self.0.push(rename.rename.to_string());
        }
    }
}

/// The names a pattern binds.
struct PatNames(Vec<String>);

impl<'ast> syn::visit::Visit<'ast> for PatNames {
    fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
        self.0.push(pat.ident.to_string());
        syn::visit::visit_pat_ident(self, pat);
    }
}

/// A function's parameter bindings in order, and whether it takes `self`.
struct Function {
    method: bool,
    params: Vec<Vec<usize>>,
}

/// One call by name: whether it is a method call, and its arguments.
type Call = (String, bool, Vec<std::sync::Arc<Reads>>);

/// The pass over one file's tree that builds its [`Syntax`], holding the
/// bindings in scope at the node it visits.
struct Builder {
    syntax: Syntax,
    scopes: Vec<Vec<(String, usize)>>,
    functions: std::collections::HashMap<String, Vec<Function>>,
    calls: Vec<Call>,
    /// The parameter bindings of each closure a `let` binds, by that binding.
    closures: std::collections::HashMap<usize, Vec<Vec<usize>>>,
    read_names: Vec<String>,
    /// The reads worked out for each expression visited so far. Every key
    /// stays alive for the whole pass: it is a node of the file's tree or of a
    /// macro body `macros` holds.
    memo: std::cell::RefCell<std::collections::HashMap<*const syn::Expr, std::sync::Arc<Reads>>>,
    /// Each macro's arguments, parsed once.
    macros:
        std::cell::RefCell<std::collections::HashMap<*const syn::Macro, std::rc::Rc<MacroBody>>>,
}

impl Builder {
    fn lookup(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(bound, _)| bound == name)
            .map(|&(_, binding)| binding)
    }

    fn declare(&mut self, pat: &syn::Pat, sources: Vec<std::sync::Arc<Reads>>) -> Vec<usize> {
        let mut names = PatNames(Vec::new());
        syn::visit::Visit::visit_pat(&mut names, pat);
        names
            .0
            .into_iter()
            .map(|name| {
                let binding = self.syntax.sources.len();
                self.syntax.sources.push(sources.clone());
                if let Some(scope) = self.scopes.last_mut() {
                    scope.push((name, binding));
                }
                binding
            })
            .collect()
    }

    /// What `expr` reads, worked out once. The pass visits an expression's
    /// subexpressions before asking this of it, so their reads are reused.
    fn reads(&self, expr: &syn::Expr) -> std::sync::Arc<Reads> {
        let key = std::ptr::from_ref(expr);
        if let Some(reads) = self.memo.borrow().get(&key) {
            return reads.clone();
        }
        let mut collected = ReadsOf {
            builder: self,
            reads: Reads::default(),
        };
        syn::visit::Visit::visit_expr(&mut collected, expr);
        let reads = std::sync::Arc::new(collected.reads);
        self.memo.borrow_mut().insert(key, reads.clone());
        reads
    }

    fn macro_body(&self, mac: &syn::Macro) -> std::rc::Rc<MacroBody> {
        let key = std::ptr::from_ref(mac);
        if let Some(body) = self.macros.borrow().get(&key) {
            return body.clone();
        }
        let body = std::rc::Rc::new(macro_body(mac));
        self.macros.borrow_mut().insert(key, body.clone());
        body
    }

    fn scoped(&mut self, visit: impl FnOnce(&mut Self)) {
        self.scopes.push(Vec::new());
        visit(self);
        self.scopes.pop();
    }

    /// Visit `closure` with its parameters holding `sources`, and hand back
    /// their bindings.
    fn closure(
        &mut self,
        closure: &syn::ExprClosure,
        sources: &[std::sync::Arc<Reads>],
    ) -> Vec<Vec<usize>> {
        let mut params = Vec::new();
        self.scoped(|builder| {
            params = closure
                .inputs
                .iter()
                .map(|input| builder.declare(input, sources.to_vec()))
                .collect();
            syn::visit::Visit::visit_expr(builder, &closure.body);
        });
        params
    }

    /// Visit a function's body in a scope of its own, which sees no local of
    /// the code around it.
    fn function(&mut self, sig: &syn::Signature, body: &syn::Block) {
        let outer = std::mem::replace(&mut self.scopes, vec![Vec::new()]);
        let mut method = false;
        let mut params = Vec::new();
        for input in &sig.inputs {
            match input {
                syn::FnArg::Receiver(_) => method = true,
                syn::FnArg::Typed(typed) => params.push(self.declare(&typed.pat, Vec::new())),
            }
        }
        self.functions
            .entry(sig.ident.to_string())
            .or_default()
            .push(Function { method, params });
        syn::visit::Visit::visit_block(self, body);
        self.scopes = outer;
    }

    fn item(&mut self, ident: &syn::Ident, expr: &syn::Expr) {
        syn::visit::Visit::visit_expr(self, expr);
        let reads = self.reads(expr);
        self.syntax
            .items
            .entry(ident.to_string())
            .or_default()
            .push(self.syntax.sources.len());
        self.syntax.sources.push(vec![reads]);
    }

    /// Visit a call's arguments, each closure among them last, with its
    /// parameters holding `sources` and the reads of the other arguments, and
    /// hand back what each argument reads.
    fn arguments<'a>(
        &mut self,
        args: impl Iterator<Item = &'a syn::Expr> + Clone,
        sources: &[std::sync::Arc<Reads>],
    ) -> Vec<std::sync::Arc<Reads>> {
        let closure = |arg: &'a syn::Expr| match peel(arg) {
            syn::Expr::Closure(closure) => Some(closure),
            _ => None,
        };
        for arg in args.clone().filter(|arg| closure(arg).is_none()) {
            syn::visit::Visit::visit_expr(self, arg);
        }
        let others: Vec<_> = args
            .clone()
            .filter(|arg| closure(arg).is_none())
            .map(|arg| self.reads(arg))
            .collect();
        for arg in args.clone() {
            if let Some(found) = closure(arg) {
                let fed: Vec<_> = sources.iter().chain(&others).cloned().collect();
                self.closure(found, &fed);
            }
        }
        args.map(|arg| self.reads(arg)).collect()
    }

    /// Hand each call's arguments to the parameters of every function of its
    /// name, and give back the finished facts.
    fn finish(mut self) -> Syntax {
        for (name, method_call, args) in std::mem::take(&mut self.calls) {
            for function in self.functions.get(&name).into_iter().flatten() {
                // A method called by path takes its receiver as the first
                // argument; one called as a method takes it before the dot.
                let skip = match (function.method, method_call) {
                    (true, false) => 1,
                    (false, true) => continue,
                    _ => 0,
                };
                for (params, arg) in function.params.iter().zip(args.iter().skip(skip)) {
                    for &binding in params {
                        self.syntax.sources[binding].push(arg.clone());
                    }
                }
            }
        }
        self.syntax
    }
}

impl<'ast> syn::visit::Visit<'ast> for Builder {
    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.function(&item.sig, &item.block);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(&item.sig, &item.block);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if let Some(body) = &item.default {
            self.function(&item.sig, body);
        }
    }

    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        self.item(&item.ident, &item.expr);
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        self.item(&item.ident, &item.expr);
    }

    fn visit_impl_item_const(&mut self, item: &'ast syn::ImplItemConst) {
        self.item(&item.ident, &item.expr);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scoped(|builder| syn::visit::visit_block(builder, block));
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        let Some(init) = &local.init else {
            self.declare(&local.pat, Vec::new());
            return;
        };
        let params = match peel(&init.expr) {
            syn::Expr::Closure(closure) => Some(self.closure(closure, &[])),
            _ => {
                self.visit_expr(&init.expr);
                None
            }
        };
        let reads = self.reads(&init.expr);
        if let Some((_, diverge)) = &init.diverge {
            self.visit_expr(diverge);
        }
        let bindings = self.declare(&local.pat, vec![reads]);
        if let (Some(params), [binding]) = (params, bindings.as_slice()) {
            self.closures.insert(*binding, params);
        }
    }

    fn visit_expr_assign(&mut self, assign: &'ast syn::ExprAssign) {
        syn::visit::visit_expr_assign(self, assign);
        if let syn::Expr::Path(path) = &*assign.left
            && let Some(name) = path.path.get_ident()
            && let Some(binding) = self.lookup(&name.to_string())
        {
            let reads = self.reads(&assign.right);
            self.syntax.sources[binding].push(reads);
        }
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        self.closure(closure, &[]);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.visit_expr(&call.receiver);
        let receiver = self.reads(&call.receiver);
        let args = self.arguments(call.args.iter(), &[receiver]);
        let method = call.method.to_string();
        if SEARCH_METHODS.contains(&method.as_str())
            && let Some(needle) = call.args.first()
            && !matches!(peel(needle), syn::Expr::Closure(_))
        {
            self.syntax.searches.push(Site {
                row: row_of(call.method.span()),
                reads: args[0].clone(),
            });
        }
        self.calls.push((method, true, args));
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        self.visit_expr(&call.func);
        let args = self.arguments(call.args.iter(), &[]);
        let syn::Expr::Path(func) = &*call.func else {
            return;
        };
        let Some(last) = func.path.segments.last() else {
            return;
        };
        let name = last.ident.to_string();
        if let Some(params) = func
            .path
            .get_ident()
            .and_then(|_| self.lookup(&name))
            .and_then(|binding| self.closures.get(&binding))
        {
            for (params, arg) in params.clone().iter().zip(&args) {
                for &binding in params {
                    self.syntax.sources[binding].push(arg.clone());
                }
            }
            return;
        }
        if self.read_names.contains(&name)
            && let Some(path) = args.first()
        {
            self.syntax.reads.push(Site {
                row: row_of(last.ident.span()),
                reads: path.clone(),
            });
        }
        self.calls.push((name, false, args));
    }

    fn visit_expr_binary(&mut self, binary: &'ast syn::ExprBinary) {
        syn::visit::visit_expr_binary(self, binary);
        // A side written as a character or number literal compares no text.
        let compares_text = [&binary.left, &binary.right].iter().all(|side| {
            !matches!(peel(side), syn::Expr::Lit(lit) if !matches!(lit.lit, syn::Lit::Str(_)))
        });
        if compares_text && matches!(binary.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) {
            let reads = Reads {
                parts: vec![self.reads(&binary.left), self.reads(&binary.right)],
                ..Reads::default()
            };
            self.syntax.searches.push(Site {
                row: row_of(syn::spanned::Spanned::span(&binary.op)),
                reads: std::sync::Arc::new(reads),
            });
        }
    }

    fn visit_expr_for_loop(&mut self, for_loop: &'ast syn::ExprForLoop) {
        self.visit_expr(&for_loop.expr);
        let reads = self.reads(&for_loop.expr);
        self.scoped(|builder| {
            builder.declare(&for_loop.pat, vec![reads]);
            builder.visit_block(&for_loop.body);
        });
    }

    fn visit_expr_let(&mut self, expr: &'ast syn::ExprLet) {
        self.visit_expr(&expr.expr);
        let reads = self.reads(&expr.expr);
        self.declare(&expr.pat, vec![reads]);
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        self.scoped(|builder| {
            builder.visit_expr(&expr.cond);
            builder.visit_block(&expr.then_branch);
        });
        if let Some((_, otherwise)) = &expr.else_branch {
            self.visit_expr(otherwise);
        }
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        self.scoped(|builder| {
            builder.visit_expr(&expr.cond);
            builder.visit_block(&expr.body);
        });
    }

    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        self.visit_expr(&expr.expr);
        let reads = self.reads(&expr.expr);
        for arm in &expr.arms {
            self.scoped(|builder| {
                builder.declare(&arm.pat, vec![reads.clone()]);
                if let Some((_, guard)) = &arm.guard {
                    builder.visit_expr(guard);
                }
                builder.visit_expr(&arm.body);
            });
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let body = self.macro_body(mac);
        match &*body {
            MacroBody::Exprs(exprs) => exprs.iter().for_each(|expr| self.visit_expr(expr)),
            MacroBody::Stmts(stmts) => {
                self.scoped(|builder| stmts.iter().for_each(|stmt| builder.visit_stmt(stmt)));
            }
            MacroBody::Tokens => {}
        }
    }
}

/// The pass collecting what one expression reads, naming bindings as the
/// scope around the expression holds them.
struct ReadsOf<'b> {
    builder: &'b Builder,
    reads: Reads,
}

impl ReadsOf<'_> {
    fn name(&mut self, name: String) {
        match self.builder.lookup(&name) {
            Some(binding) => self.reads.locals.push(binding),
            None => self.reads.items.push(name),
        }
    }

    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        let mut tokens = tokens.into_iter().peekable();
        while let Some(token) = tokens.next() {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                proc_macro2::TokenTree::Literal(literal) => {
                    let token = proc_macro2::TokenTree::Literal(literal);
                    if let Ok(literal) = syn::parse2::<syn::LitStr>(token.into()) {
                        syn::visit::Visit::visit_lit_str(self, &literal);
                    }
                }
                proc_macro2::TokenTree::Ident(ident) => {
                    let called = matches!(
                        tokens.peek(),
                        Some(proc_macro2::TokenTree::Group(group))
                            if group.delimiter() == proc_macro2::Delimiter::Parenthesis
                    );
                    if called {
                        self.reads.calls.push(ident.to_string());
                    } else {
                        self.name(ident.to_string());
                    }
                }
                proc_macro2::TokenTree::Punct(_) => {}
            }
        }
    }
}

impl<'ast> syn::visit::Visit<'ast> for ReadsOf<'_> {
    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        let value = literal.value();
        // A format string reads each `{name}` it captures.
        for capture in value.split('{').skip(1) {
            let name = capture.split(['}', ':']).next().unwrap_or_default().trim();
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                self.name(name.to_string());
            }
        }
        self.reads.literals.push(value);
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        match (path.qself.is_none(), path.path.get_ident()) {
            (true, Some(ident)) => self.name(ident.to_string()),
            _ => {
                if let Some(last) = path.path.segments.last() {
                    self.reads.items.push(last.ident.to_string());
                }
            }
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(func) = &*call.func
            && let Some(last) = func.path.segments.last()
        {
            self.reads.calls.push(last.ident.to_string());
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        let known = self
            .builder
            .memo
            .borrow()
            .get(&std::ptr::from_ref(expr))
            .cloned();
        match known {
            Some(reads) => self.reads.parts.push(reads),
            None => syn::visit::visit_expr(self, expr),
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let body = self.builder.macro_body(mac);
        match &*body {
            MacroBody::Exprs(exprs) => exprs.iter().for_each(|expr| self.visit_expr(expr)),
            MacroBody::Stmts(stmts) => stmts.iter().for_each(|stmt| self.visit_stmt(stmt)),
            MacroBody::Tokens => self.tokens(mac.tokens.clone()),
        }
    }
}

/// A search for a gate's spelling is a hand cut whichever binding form carries
/// its needle to it, reported at the row of the searching method's name: a written
/// literal, a `let` and a chain of them, a later assignment, a `const` and a
/// `static` declared anywhere in the file, a `for`, `if let`, `while let` or
/// `match` pattern, a closure's parameter fed by the receiver it is handed to,
/// by the other arguments of a call, or by the calls of the local it is bound
/// to, a function's or a method's parameter fed by its callers, a macro's
/// argument, and a name a format string captures. Each form's twin fed only
/// other text is no cut, a comparison with a character literal compares no
/// text, and a row outside test scope is outside the rule.
#[test]
fn a_gate_search_is_a_hand_cut_whichever_binding_carries_its_needle() {
    // Assembled from parts, so this file writes no gate's spelling whole.
    let gate = concat!("#[cfg", "(test)]");
    let all = concat!("#[cfg", "(all(test");
    let module = concat!("mod ", "tests");
    let fixture = [
        "fn planted(body: &str, lines: &[&str], planted: &Planted) {".to_string(),
        format!("    const GATES: [&str; 2] = [\"{gate}\", \"{module}\"];"),
        "    static QUIET: &str = \"production\";".to_string(),
        format!("    let needle = \"{all}\";"),
        "    let other = \"production\";".to_string(),
        format!("    body.contains(\"{gate}\");"),
        "    body.contains(\"production\");".to_string(),
        "    body.starts_with(needle);".to_string(),
        "    body.starts_with(other);".to_string(),
        "    body.contains(GATES[0]);".to_string(),
        "    body.contains(QUIET);".to_string(),
        "    body.contains(LOUD);".to_string(),
        "    for gate in GATES { body.contains(gate); }".to_string(),
        "    for word in lines { body.contains(word); }".to_string(),
        "    if let Some(gate) = GATES.first() { body.contains(gate); }".to_string(),
        "    if let Some(word) = lines.first() { body.contains(word); }".to_string(),
        "    while let Some(gate) = GATES.last() { body.contains(gate); }".to_string(),
        "    while let Some(word) = lines.last() { body.contains(word); }".to_string(),
        "    match GATES.first() { Some(gate) => body.contains(gate), None => false };".to_string(),
        "    match lines.first() { Some(word) => body.contains(word), None => false };".to_string(),
        "    GATES.iter().any(|gate| body.contains(gate));".to_string(),
        "    lines.iter().any(|word| body.contains(word));".to_string(),
        "    let probe = |gate: &str| body.contains(gate);".to_string(),
        "    probe(GATES[1]);".to_string(),
        "    let quiet = |word: &str| body.contains(word);".to_string(),
        "    quiet(\"production\");".to_string(),
        "    check(GATES[0], |gate| body.contains(gate));".to_string(),
        "    check(\"production\", |word| body.contains(word));".to_string(),
        "    let deep = GATES[1];".to_string(),
        "    let deeper = deep;".to_string(),
        "    let deepest = deeper;".to_string(),
        "    let bottom = deepest;".to_string(),
        "    body.contains(bottom);".to_string(),
        "    let later;".to_string(),
        "    later = GATES[0];".to_string(),
        "    body.contains(later);".to_string(),
        format!("    body == \"{gate}\";"),
        "    body == \"production\";".to_string(),
        "    assert!(body.ends_with(needle));".to_string(),
        "    let label = format!(\"{needle}\");".to_string(),
        "    body.contains(&label);".to_string(),
        "    planted.has(GATES[0]);".to_string(),
        format!("    searches(body, \"{gate}\");"),
        "    quietly(body, \"production\");".to_string(),
        "    body".to_string(),
        "        .lines()".to_string(),
        "        .position(|l| {".to_string(),
        "            GATES".to_string(),
        "                .iter()".to_string(),
        "                .any(|g| l.contains(g))".to_string(),
        "        });".to_string(),
        "    body.lines().position(|l| {".to_string(),
        "        GATES.iter().any(|g| {".to_string(),
        "            let t = l.trim_start();".to_string(),
        "            t.starts_with(g)".to_string(),
        "        })".to_string(),
        "    });".to_string(),
        "}".to_string(),
        "fn searches(body: &str, needle: &str) -> bool {".to_string(),
        "    body.contains(needle)".to_string(),
        "}".to_string(),
        "fn quietly(body: &str, needle: &str) -> bool {".to_string(),
        "    body.contains(needle)".to_string(),
        "}".to_string(),
        "impl Planted {".to_string(),
        "    fn has(&self, needle: &str) -> bool {".to_string(),
        "        self.0.contains(needle)".to_string(),
        "    }".to_string(),
        "}".to_string(),
        format!("static LOUD: &str = \"{gate}\";"),
        "fn chars() -> bool { LOUD.chars().any(|c| c == '_') }".to_string(),
        "fn chained(body: &str) -> bool {".to_string(),
        "    body".to_string(),
        "        .trim_start()".to_string(),
        "        .starts_with(LOUD)".to_string(),
        "}".to_string(),
    ];
    let syntax = syntax(&fixture.join("\n")).unwrap_or_else(|e| panic!("fixture: {e}"));
    let cuts = [
        5, 7, 9, 11, 12, 14, 16, 18, 20, 22, 26, 32, 35, 36, 38, 40, 49, 54, 59, 66, 74,
    ];
    assert_eq!(
        hand_cut_gate_rows(&syntax, |_| true),
        cuts,
        "each binding form carrying a gate's spelling is a hand cut at its search \
         call; each twin carrying other text is none"
    );
    assert_eq!(
        hand_cut_gate_rows(&syntax, |n| n != 7),
        cuts.into_iter().filter(|&n| n != 7).collect::<Vec<_>>(),
        "a row outside test scope is outside the rule"
    );
}

/// A source `syn` cannot parse is named with the line its parse stops on, so a
/// walk reading it fails.
#[test]
fn a_source_syn_cannot_parse_is_an_error_naming_its_line() {
    let parsed = syntax("fn whole() {}\nfn broken( {\n");
    assert!(
        parsed.as_ref().is_err_and(|e| e.starts_with("line 2")),
        "the parse error names line 2: {:?}",
        parsed.err()
    );
}

/// Whether `name` holds a hatch marker, judged on the name alone.
///
/// A marker is held in a name that says so: a `*_HATCH` / `*_MARKER` const, or
/// the bare `marker` / `hatch` a shared lookup takes as its parameter. The walk
/// below has to tell `contains(NATIVE_HATCH)` — reading a hatch off a source
/// line — from `contains(STD_MARKER)`, which reads a `PATH` entry, and both
/// arrive as identifiers, so the convention is what separates them.
fn names_a_hatch_marker(name: &str) -> bool {
    let shouted = name.to_ascii_uppercase();
    shouted.contains("HATCH") || shouted.ends_with("_MARKER") || shouted == "MARKER"
}

/// [`crate::test_helpers::test_region_of`] of `body`, written to `name` below
/// a fresh directory: a fixture reaches the region through the same reader
/// every walk over the test scope uses.
fn written_test_region(name: &str, body: &str) -> String {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("temp dir: {e}"));
    let path = dir.path().join(name);
    let parent = path.parent().unwrap_or(dir.path());
    std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{}: {e}", parent.display()));
    std::fs::write(&path, body).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    crate::test_helpers::test_region_of(&path).into_owned()
}

/// The partition of `file` into its test region and its production slice,
/// rebuilt into the file again: each row takes the region's line where a marked
/// item claims the row and the next production line where none does.
///
/// Membership is the RANGES. A blank line inside a test item is blank in the
/// mask exactly as a blanked production row is, so a reader asking `is_empty()`
/// hands that row to the production half, takes a line that belongs further
/// down, and every row after it is off by one.
fn assert_reassembles(file: &str) {
    let region = written_test_region("thing.rs", file);
    // unfloored-slice-ok: the subject is one fixture held in memory; no source on disk is read
    let slice = crate::test_helpers::production_slice(file);
    let production: Vec<&str> = slice.lines().collect();
    let ranges = crate::test_helpers::inline_test_item_ranges(file);
    let mask: Vec<&str> = region.lines().collect();
    let mut spent = 0usize;
    let rebuilt: Vec<&str> = (0..file.lines().count())
        .map(|at| {
            if ranges.iter().any(|(from, to)| (*from..*to).contains(&at)) {
                mask.get(at).copied().unwrap_or_default()
            } else {
                let line = production.get(spent).copied().unwrap_or_default();
                spent += 1;
                line
            }
        })
        .collect();
    assert_eq!(
        rebuilt,
        file.lines().collect::<Vec<_>>(),
        "the region and the production slice reassemble the file line for line:\n{region}"
    );
    assert_eq!(
        spent,
        production.len(),
        "every production line lands in a row no marked item claims:\n{region}"
    );
}

/// The region is every column-0 `#[cfg(test)]` item and nothing else, wherever
/// the items sit and whatever kind of item carries the marker.
///
/// Each leg is a tell one of the two cuts this walk has had got wrong. The
/// first opened at the FIRST `#[cfg(test)]` in the file, so a test-only item
/// standing beside the production code it serves pulled every production line
/// below it into the region. The second read `mod` alone, so that same item was
/// in NEITHER half's test text: a marker lookup or a declared-document
/// interpolation written in a `#[cfg(test)] fn` answered to no walk. And a
/// `#[cfg(test)]` row inside a multi-line literal is a row of a string, which
/// opens a region in a file that may hold no test at all.
#[test]
fn the_test_region_is_every_inline_test_item_and_nothing_else() {
    // Assembled from two literals: a `#[cfg(test)]` written out here would
    // read as this scaffolding file declaring a second test region of its own.
    let gate = concat!("#[cfg", "(test)]");
    // Line 5 and line 14 are blank INSIDE a test item and line 8 is blank
    // between two production lines: the three rows a partition deciding
    // membership from the mask's own content reads the same way and gets two
    // of them wrong.
    let file = format!(
        "fn production() {{}}\n\
         {gate}\n\
         fn item_before() {{\n\
         \x20   let held = \"x\";\n\
         \n\
         }}\n\
         fn production_below_the_item() {{}}\n\
         \n\
         {gate}\n\
         const ITEM_CONST: usize = 2;\n\
         {gate}\n\
         mod tests {{\n\
         \x20   fn a_pin() {{}}\n\
         \n\
         }}\n\
         fn production_below_the_module() {{}}\n\
         const IN_A_LITERAL: &str = \"\n\
         {gate}\n\
         fn not_in_the_region() {{}}\n\
         \";\n\
         // {gate} in a comment opens nothing\n"
    );
    let file = file.as_str();
    let region = written_test_region("thing.rs", file);
    let held: Vec<(usize, &str)> = region
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(at, line)| (at + 1, line))
        .collect();
    assert_eq!(
        held,
        vec![
            (2, gate),
            (3, "fn item_before() {"),
            (4, "    let held = \"x\";"),
            (6, "}"),
            (9, gate),
            (10, "const ITEM_CONST: usize = 2;"),
            (11, gate),
            (12, "mod tests {"),
            (13, "    fn a_pin() {}"),
            (15, "}"),
        ],
        "every marked item is the region, module or not, at the file's own line numbers:\n{region}"
    );
    for held in [
        "fn production()",
        "fn production_below_the_item()",
        "fn production_below_the_module()",
        "IN_A_LITERAL",
        "fn not_in_the_region()",
        "in a comment",
    ] {
        assert!(
            !region.contains(held),
            "{held} is production text, whichever side of a marked item it sits on:\n{region}"
        );
    }
    assert_eq!(
        region.lines().count(),
        file.lines().count(),
        "a blanked line still occupies its own row, or an offender cannot be opened where it is reported"
    );
    // The partition stated as a REASSEMBLY: each row takes the region's line
    // where the row belongs to a marked item and the next production line where
    // it does not, and the result has to be the file again with every
    // production line spent. A count is satisfied by two lines that swapped
    // halves in opposite directions; this is not.
    assert_reassembles(file);
    assert_eq!(
        // unfloored-slice-ok: the subject is one fixture held in memory; no source on disk is read
        crate::test_helpers::production_slice(file)
            .lines()
            .filter(|line| line.is_empty())
            .count(),
        1,
        "a blank line between two production lines is KEPT, at its own position"
    );

    // An item the file never terminates runs to the end of the file, and the
    // range is still yielded: handing an unclosed tail back to the production
    // half is the blinding this scan exists to prevent, arriving silently. The
    // range says which half the tail landed in — a reassembly holds either way,
    // since a file whose tail went wholly to production rebuilds too.
    let truncated = format!(
        "fn production() {{}}\n\
         {gate}\n\
         fn never_closes() {{\n\
         \x20   let held = 1;\n"
    );
    assert_eq!(
        crate::test_helpers::inline_test_item_ranges(&truncated),
        vec![(1, truncated.lines().count())],
        "an unterminated item is one range, its marker through the end of the file"
    );
    assert_reassembles(&truncated);
}

/// A workspace source is read once per test process: every later read of its
/// body borrows that one read, and its gates and test region are cut from it,
/// so a gate row always indexes a row of the body a walk holds. A fixture
/// written outside the workspace is read afresh on every call, since a test
/// may rewrite it between two reads.
#[test]
fn a_workspace_source_is_read_once_and_its_views_borrow_that_read() {
    use std::borrow::Cow;
    let path = workspace_root().join("crates/cfgd-core/src/daemon/reconcile.rs");
    let (first, again) = (walked_file_body(&path), walked_file_body(&path));
    let (Cow::Borrowed(first), Cow::Borrowed(again)) = (first, again) else {
        panic!("a workspace source is borrowed from the one read of it");
    };
    assert!(
        std::ptr::eq(first, again),
        "two reads of one source share its body"
    );
    let Cow::Borrowed(gates) = crate::test_helpers::line_gates_of(&path) else {
        panic!("the gates of a workspace source are borrowed from its scan");
    };
    assert_eq!(
        gates.len(),
        first.lines().count(),
        "one gate per row of the body"
    );
    let Cow::Borrowed(region) = crate::test_helpers::test_region_of(&path) else {
        panic!("the test region of a workspace source is borrowed from its scan");
    };
    assert_eq!(
        region.lines().count(),
        first.lines().count(),
        "the region keeps every row"
    );

    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("temp dir: {e}"));
    let fixture = dir.path().join("fixture.rs");
    for body in ["fn before() {}\n", "fn after() {}\n"] {
        std::fs::write(&fixture, body).unwrap_or_else(|e| panic!("{}: {e}", fixture.display()));
        assert_eq!(
            walked_file_body(&fixture),
            body,
            "a fixture is read as it stands"
        );
    }
}

/// A read of a workspace `.rs` source is reported at the row holding its call,
/// however its path reaches it: written into the call, split below the `let`
/// it initializes, handed to a closure down an iterated array, passed to a
/// function parameter by a caller, read through a name `use` gives
/// `read_to_string`, or captured by a format string. A read of any other file
/// is none.
#[test]
fn a_workspace_source_read_is_found_at_its_call_whichever_way_its_path_reaches_it() {
    // Assembled from parts, so the fixture's calls read as literals here.
    let read = concat!("read_to", "_string");
    let fixture = [
        format!("use std::fs::{read} as slurp;"),
        "fn planted() {".to_string(),
        format!(
            "    let whole = std::fs::{read}(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/lib.rs\"));"
        ),
        "    let split =".to_string(),
        format!("        std::fs::{read}(workspace_root().join(\"crates/a/src/lib.rs\"));"),
        "    const SOURCES: [&str; 1] = [\"src/main.rs\"];".to_string(),
        "    let root = std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\"));".to_string(),
        format!(
            "    let bodies: Vec<_> = SOURCES.iter().map(|s| std::fs::{read}(root.join(s))).collect();"
        ),
        format!("    let other = std::fs::{read}(workspace_root().join(\"Cargo.toml\"));"),
        "    let first = source(\"crates/b/src/lib.rs\");".to_string(),
        "    let second = source(\"chart/values.yaml\");".to_string(),
        "    let aliased = slurp(workspace_root().join(\"crates/c/src/lib.rs\"));".to_string(),
        "    let dir = env!(\"CARGO_MANIFEST_DIR\");".to_string(),
        format!("    let captured = std::fs::{read}(format!(\"{{dir}}/src/d.rs\"));"),
        "}".to_string(),
        "fn source(relative: &str) -> String {".to_string(),
        format!("    std::fs::{read}(workspace_root().join(relative)).unwrap_or_default()"),
        "}".to_string(),
    ];
    let syntax = syntax(&fixture.join("\n")).unwrap_or_else(|e| panic!("fixture: {e}"));
    assert_eq!(
        workspace_source_reads(&syntax, |_| true),
        [2, 4, 7, 11, 13, 16],
        "the written path, the split read, the closure over an iterated array, the \
         aliased read, the captured path and the parameter a caller hands a source \
         are each a read at their call; the manifest read is none"
    );
}

/// A test reading a Rust source of the workspace reads it through
/// [`walked_file_body`], which hands every reader the one body the source's
/// gates and views were cut from. A read [`workspace_source_reads`] finds reads
/// that source a second time.
#[test]
fn no_test_reads_a_workspace_source_past_the_one_cache() {
    let mut files = 0usize;
    let mut offenders = Vec::new();
    let mut unparsed = Vec::new();
    for path in workspace_rust_files() {
        files += 1;
        let label = source_label(&path);
        let whole_test = crate::test_helpers::is_test_source(&path)
            || crate::test_helpers::is_test_only_file(&path);
        let gates = if whole_test {
            Default::default()
        } else {
            crate::test_helpers::line_gates_of(&path)
        };
        let in_test = |n: usize| whole_test || gates.get(n).is_some_and(Option::is_some);
        match syntax_of(&path) {
            Ok(syntax) => {
                // unfloored-slice-ok: a report quotes the whole file's row the tree names.
                let body = walked_file_body(&path);
                let lines: Vec<&str> = body.lines().collect();
                for n in workspace_source_reads(syntax, in_test) {
                    offenders.push(format!("{label}:{}: {}", n + 1, lines[n].trim()));
                }
            }
            Err(e) => unparsed.push(format!("{label}: {e}")),
        }
    }
    assert!(
        files > 300,
        "the walk read {files} sources of the workspace"
    );
    assert!(
        unparsed.is_empty(),
        "a source `syn` cannot parse is a source this walk cannot judge:\n{}",
        unparsed.join("\n")
    );
    assert!(
        offenders.is_empty(),
        "a workspace source is read through `walked_file_body`, which reads it once \
         per test process:\n{}",
        offenders.join("\n")
    );
}

/// A file holding tests alone is its own test region end to end: a production
/// line in it, above or below a gate, is test text too.
#[test]
fn a_file_of_tests_alone_is_its_whole_test_region() {
    // Assembled from two literals, so this scaffolding file declares no gate.
    let gate = concat!("#[cfg", "(test)]");
    let scaffolding = format!("fn above() {{}}\n{gate}\nfn below() {{}}\n");
    assert_eq!(
        written_test_region("tests/scaffolding.rs", &scaffolding),
        scaffolding,
        "a scaffolding file is test text end to end"
    );
}

/// A test-only item's extent ends where the ITEM ends, whatever delimiter it
/// closes on.
///
/// Every multi-file walk and the workspace memo read what
/// [`crate::test_helpers::production_slice`] hands back, so an extent that runs
/// past its item takes production code out of the population with it and the
/// walk still reports the file as read. Two shapes did exactly that: the
/// one-line `use source::{…};` at `cli/mod.rs:57`, whose brace opens and shuts
/// on its own line, dropped the 97 lines down to the next top-level `}` and hid
/// `local_pull_next_step` from every walk; and the gate on a struct FIELD in
/// `daemon/mod.rs`, which closes on a comma, dropped the three methods declared
/// after it.
///
/// Three more shapes rustfmt writes freely: an item whose braced body opens and
/// shuts on one line, one closing on a compound delimiter (`});`), and a field
/// whose TYPE wraps. Each is paired here with the neighbour an over-long extent
/// would eat, and with the wrapped generic parameter list that must NOT be read
/// as a field — a terminator rule loose enough to end the field early leaves
/// half a gated function standing as production text. The `where` clause is
/// that rule's other edge: its last bound ends a line on a comma with every
/// delimiter shut, and `crates/cfgd/src/packages/npm.rs` had its gated helper's
/// body handed to every walk until the comma was refused on an item's head.
/// A block nested inside a gated item closes at a deeper indent, which is not
/// the item's own end.
#[test]
fn a_gated_items_extent_ends_where_the_item_does() {
    // Assembled, so this file declares no gated item of its own.
    let gate = concat!("#[cfg", "(test)]");
    let src = concat!(
        "pub fn kept_before() {}\n",
        "@gate\n",
        "use source::{alpha, beta};\n",
        "pub fn kept_after_one_line_use() {}\n",
        "@gate\n",
        "use source::{\n",
        "    gamma,\n",
        "};\n",
        "pub fn kept_after_wrapped_use() {}\n",
        "struct Holder {\n",
        "    @gate\n",
        "    captured: Mutex<Vec<String>>,\n",
        "    kept_field: usize,\n",
        "}\n",
        "@gate\n",
        "static WRAPPED: LazyLock<\n",
        "    Mutex<HashMap<Duration, usize>>,\n",
        "> = LazyLock::new(|| Mutex::new(HashMap::new()));\n",
        "pub fn kept_after_wrapped_static() {}\n",
        "@gate\n",
        "fn gated_empty_body() {}\n",
        "pub fn kept_after_self_closing_body() {}\n",
        "@gate\n",
        "static COMPOUND: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| {\n",
        "    Mutex::new(Vec::new())\n",
        "});\n",
        "pub fn kept_after_compound_closer() {}\n",
        "struct Wrapper {\n",
        "    @gate\n",
        "    wrapped_capture: Mutex<\n",
        "        Vec<String>,\n",
        "    >,\n",
        "    kept_wrapped_neighbour: usize,\n",
        "}\n",
        "@gate\n",
        "fn gated_wrapped_generics<\n",
        "    F,\n",
        "    R,\n",
        ">(f: F) -> R {\n",
        "    f()\n",
        "}\n",
        "pub fn kept_after_wrapped_generics() {}\n",
        "@gate\n",
        "fn gated_where_clause<F, R>(elevated: bool, f: F) -> R\n",
        "where\n",
        "    F: FnOnce() -> R,\n",
        "{\n",
        "    leaked_where_body(elevated);\n",
        "    f()\n",
        "}\n",
        "pub fn kept_after_where_clause() {}\n",
        "@gate\n",
        "fn gated_nested_block() {\n",
        "    if ready() {\n",
        "        inner_call();\n",
        "    }\n",
        "    leaked_after_inner_close();\n",
        "}\n",
        "pub fn kept_after_nested_block() {}\n",
        "@gate\n",
        "mod tests {\n",
        "    fn gated_away() {}\n",
        "}\n",
    )
    .replace("@gate", gate);
    // unfloored-slice-ok: the subject is this fixture's own string, so there is no read to floor.
    let production = crate::test_helpers::production_slice(&src);
    for kept in [
        "kept_before",
        "kept_after_one_line_use",
        "kept_after_wrapped_use",
        "kept_field",
        "kept_after_wrapped_static",
        "kept_after_self_closing_body",
        "kept_after_compound_closer",
        "kept_wrapped_neighbour",
        "kept_after_wrapped_generics",
        "kept_after_where_clause",
        "kept_after_nested_block",
    ] {
        assert!(
            production.contains(kept),
            "the extent ran past its item and dropped `{kept}`:\n{production}"
        );
    }
    for gated in [
        "alpha, beta",
        "gamma",
        "captured:",
        "WRAPPED",
        "gated_away",
        "gated_empty_body",
        "COMPOUND",
        "wrapped_capture",
        "gated_wrapped_generics",
        "gated_where_clause",
        "leaked_where_body",
        "leaked_after_inner_close",
    ] {
        assert!(
            !production.contains(gated),
            "a gated item survived the cut: `{gated}`:\n{production}"
        );
    }
}

/// The production half and the test half of a source are one partition, read
/// by one scanner: every function a file declares is on exactly one side, and
/// the workspace memo reads the side [`crate::test_helpers::production_slice_of`]
/// does.
///
/// Each test-only shape the tree holds is here beside the production function
/// an over-long cut would take with it: an inline `mod tests`, a
/// `#[cfg(test)] fn` at column 0 and nested in an `impl`, and a
/// `cfg(any(test, feature = "test-helpers"))` fn and mod, the gate cfgd-core's
/// harness overrides are built under. Two readers once disagreed on 25
/// functions, and both read the `test-helpers` gate as production.
///
/// A `test-helpers` item is a seam, so it has a view of its own: the seam view
/// keeps it beside production and drops every `#[cfg(test)]` item, one nested
/// in a seam included. The production view, the seam view and the test region
/// put the file back together line for line, each line taken from the view its
/// gate routes it to and none copied from the file.
#[test]
fn the_production_slice_and_the_test_region_partition_every_test_only_shape() {
    let gate = concat!("#[cfg", "(test)]");
    let helpers = concat!("#[cfg", "(any(test, feature = \"test-helpers\"))]");
    let src = concat!(
        "pub fn production_before() {}\n",
        "@gate\n",
        "fn gated_fn() {}\n",
        "pub struct Held;\n",
        "impl Held {\n",
        "    pub fn production_method(&self) {}\n",
        "    @gate\n",
        "    fn nested_gated_method(&self) {}\n",
        "    pub fn production_after_nested(&self) {}\n",
        "}\n",
        "@helpers\n",
        "pub fn helper_gated_fn() {}\n",
        "@helpers\n",
        "pub mod helper_gated_mod {\n",
        "    pub fn inside_helper_mod() {}\n",
        "    @gate\n",
        "    fn test_inside_helper_mod() {}\n",
        "}\n",
        "pub fn production_after_helpers() {}\n",
        "@gate\n",
        "mod tests {\n",
        "    fn a_pin() {}\n",
        "}\n",
    )
    .replace("@helpers", helpers)
    .replace("@gate", gate);
    let root = tempfile::tempdir().unwrap_or_else(|e| panic!("temp dir: {e}"));
    let path = root.path().join("lib.rs");
    std::fs::write(&path, &src).unwrap_or_else(|e| panic!("write the fixture: {e}"));
    let declared = |text: &str| -> Vec<String> {
        text.lines()
            .filter_map(|line| {
                crate::test_helpers::declared_fn_name(&crate::test_helpers::code_line(line))
            })
            .collect()
    };
    let production = crate::test_helpers::production_slice_of(&path);
    let seams = crate::test_helpers::production_and_seams_of(&path);
    let region = crate::test_helpers::test_region_of(&path);
    let region_lines: Vec<&str> = region.lines().collect();
    let gates = crate::test_helpers::line_gates_of(&path);
    let (mut kept, mut seam_kept) = (production.lines(), seams.lines());
    let mut rebuilt = String::new();
    let mut test_only: Vec<&str> = Vec::new();
    for (at, gate) in gates.iter().enumerate() {
        let line = match gate {
            None => {
                let from_production = kept.next();
                assert_eq!(
                    seam_kept.next(),
                    from_production,
                    "line {at}: the seam view keeps production"
                );
                from_production
            }
            Some(crate::test_helpers::Gate::TestHelpers) => {
                let from_seams = seam_kept.next();
                assert_eq!(
                    from_seams,
                    region_lines.get(at).copied(),
                    "line {at}: a `test-helpers` line is the seam view's next line and the \
                     test region's own"
                );
                from_seams
            }
            Some(crate::test_helpers::Gate::Test) => {
                let from_region = region_lines.get(at).copied();
                test_only.extend(from_region);
                from_region
            }
        };
        rebuilt.push_str(line.unwrap_or("<no line>"));
        rebuilt.push('\n');
    }
    assert!(
        kept.next().is_none() && seam_kept.next().is_none(),
        "a view holds a line no gate routes to it"
    );
    assert_eq!(
        rebuilt, src,
        "the three views rebuild the file byte for byte"
    );
    assert_eq!(
        declared(&production),
        [
            "production_before",
            "production_method",
            "production_after_nested",
            "production_after_helpers",
        ],
        "the production half is every production function and nothing else:\n{production}"
    );
    assert_eq!(
        declared(&crate::test_helpers::test_region_mask(&src)),
        [
            "gated_fn",
            "nested_gated_method",
            "helper_gated_fn",
            "inside_helper_mod",
            "test_inside_helper_mod",
            "a_pin",
        ],
        "the test half is every test-only function and nothing else"
    );
    assert_eq!(
        declared(&seams),
        [
            "production_before",
            "production_method",
            "production_after_nested",
            "helper_gated_fn",
            "inside_helper_mod",
            "production_after_helpers",
        ],
        "the seam view is production and every `test-helpers` item, and no test:\n{seams}"
    );
    assert_eq!(
        seams,
        crate::test_helpers::production_and_seams_slice(&src),
        "the seam view of a file is the seam slice of its body"
    );
    assert_eq!(
        declared(&test_only.join("\n")),
        [
            "gated_fn",
            "nested_gated_method",
            "test_inside_helper_mod",
            "a_pin"
        ],
        "the test-only part is every `#[cfg(test)]` function, one nested in a seam included"
    );
    // A walk over the files built only for tests reads their seam view.
    let cut = crate::test_helpers::test_module_cut_of(&path);
    assert_eq!(
        declared(&cut),
        declared(&seams),
        "the cut keeps every `test-helpers` item and drops every test:\n{cut}"
    );
    // The memo's body reader and its rows, on the same file.
    assert_eq!(
        crate::test_helpers::floored_production_body(&path),
        production,
        "the memo reads the production half byte for byte"
    );
    let rows: Vec<String> = crate::test_helpers::file_declarations(&path)
        .into_iter()
        .map(|(name, ..)| name)
        .collect();
    assert_eq!(
        rows,
        declared(&production),
        "the memo's rows are the production half's functions"
    );
    // The workspace files the two readers split on, read through the memo.
    let workspace =
        crate::test_helpers::workspace_declarations(crate::test_helpers::WORKSPACE_CRATES);
    let crates_dir = workspace_root().join("crates");
    for (file, gated) in [
        ("cfgd/src/cli/helpers.rs", "registry_built"),
        ("cfgd/src/packages/npm.rs", "npm_path_dirs_for"),
        (
            "cfgd-core/src/daemon/tick_cache.rs",
            "set_module_reuse_ttl_override",
        ),
    ] {
        let path = crates_dir.join(file);
        let rows = workspace.rows_in(&path);
        assert!(!rows.is_empty(), "{file} declares production functions");
        assert!(
            rows.iter().all(|(name, ..)| name != gated),
            "{file}: the memo reads the test-only `{gated}` as production"
        );
        assert_eq!(
            rows,
            crate::test_helpers::file_declarations(&path).as_slice(),
            "{file}: the memo's rows and the file's production half disagree"
        );
    }
    // A seam in the tree: the production memo leaves it out, and the seam memo
    // holds it as a `test-helpers` row.
    let daemon = crates_dir.join("cfgd-core/src/daemon/mod.rs");
    let seam = "run_compliance_and_reconcile_ticks";
    assert!(
        workspace
            .rows_in(&daemon)
            .iter()
            .all(|(name, ..)| name != seam),
        "the production memo reads the `test-helpers` seam `{seam}` as production"
    );
    let seams_memo =
        crate::test_helpers::workspace_seam_declarations(crate::test_helpers::WORKSPACE_CRATES);
    let gated: Vec<Option<crate::test_helpers::Gate>> = seams_memo
        .rows
        .iter()
        .zip(&seams_memo.gates)
        .zip(&seams_memo.sites)
        .filter(|((row, _), site)| row.0 == seam && site.1 == daemon.as_path())
        .map(|((_, gate), _)| *gate)
        .collect();
    assert_eq!(
        gated,
        [Some(crate::test_helpers::Gate::TestHelpers)],
        "the seam memo holds `{seam}` once, gated to `test-helpers`"
    );
    // Two arms of one function gated apart share its name and owner, so each
    // row's gate is read off its own line.
    let reconciler = crates_dir.join("cfgd-core/src/reconciler/mod.rs");
    let twins: Vec<Option<crate::test_helpers::Gate>> = seams_memo
        .rows
        .iter()
        .zip(&seams_memo.gates)
        .zip(&seams_memo.sites)
        .filter(|((row, _), site)| row.0 == "resolved_home" && site.1 == reconciler.as_path())
        .map(|((_, gate), _)| *gate)
        .collect();
    assert_eq!(
        twins,
        [None, Some(crate::test_helpers::Gate::TestHelpers)],
        "the shipped arm of `resolved_home` is ungated and its `test-helpers` arm is gated"
    );
}

/// A file built only for tests, or holding tests alone, has no production
/// region, and the per-root population leaves both kinds out.
#[test]
fn floored_production_body_reads_no_test_only_file_as_production() {
    let root = workspace_root();
    for held in [
        "crates/cfgd-core/src/test_helpers.rs",
        "crates/cfgd/src/cli/test_support.rs",
        "crates/cfgd/src/cli/tests.rs",
    ] {
        assert_eq!(
            crate::test_helpers::floored_production_body(&root.join(held)),
            "",
            "{held} reads as production"
        );
    }
    assert!(
        !crate::test_helpers::floored_production_body(&root.join("crates/cfgd/src/cli/mod.rs"))
            .is_empty()
    );
    // one-root-population-ok: the question is an absence, which no root has a
    // count of; each root's own floor is `production_sources_per_root`'s.
    let held: Vec<&PathBuf> =
        crate::test_helpers::production_sources_per_root(crate::test_helpers::WORKSPACE_CRATES)
            .into_iter()
            .flat_map(|(_, sources)| sources.iter().map(|(path, _)| path))
            .filter(|p| {
                crate::test_helpers::is_test_only_file(p) || crate::test_helpers::is_test_source(p)
            })
            .collect();
    assert!(
        held.is_empty(),
        "the population holds test-only files: {held:?}"
    );
}

/// The item keywords a lead-folding needle may name, and nothing else.
const ITEM_LEAD_KEYWORDS: [&str; 11] = [
    "fn", "mod", "struct", "enum", "union", "trait", "impl", "type", "const", "static", "use",
];

/// The readers that match a needle against the START or the END of a line,
/// where a needle carrying no boundary of its own still folds a lead. Both
/// halves of every pair are here, because an end reader folds the same lead as
/// well as its start twin: `ends_with("pub")` also matches `sub`.
const ITEM_LEAD_EDGE_READERS: [&str; 7] = [
    ".starts_with(",
    ".ends_with(",
    ".strip_prefix(",
    ".strip_suffix(",
    ".trim_start_matches(",
    ".trim_end_matches(",
    ".trim_matches(",
];

/// Whether a string literal's content is a hand-written item-lead fold.
///
/// The NEEDLE's shape is the tell, whatever the reader it was handed to:
/// a needle that folds a lead is the lead itself plus at most the item keyword
/// behind it (`"pub "`, `"pub("`, `"pub fn "`, `"unsafe fn "`), and a needle
/// carrying anything item-SPECIFIC after that names one declaration this tree
/// really holds, and folds no lead off any. So `contains("pub fn ")` is
/// a fold and `find("pub enum BackupCommand {")` is not.
///
/// The reader decides ONE case: a bare lead word carrying no boundary at all.
/// `"default"` is the name of a profile in a few hundred fixtures, so compared,
/// searched for or stored it is a value — but handed to an EDGE reader
/// (`edge_reader`) it is the buggy fold itself, the one whose `pub` also
/// matches `publish` and whose `default` also matches `defaults`.
fn needle_folds_an_item_lead(content: &str, edge_reader: bool) -> bool {
    // A file with CRLF endings spells the same continuation with a `\r` the
    // compiler eats alongside the newline.
    if content.contains("\\\r\n") {
        return needle_folds_an_item_lead(&content.replace("\\\r\n", "\\\n"), edge_reader);
    }
    // A `\`-continued literal spells one needle across two rows: the compiler
    // eats the backslash, the newline and the next row's indent, so the needle
    // the code really carries is the joined one.
    if content.contains("\\\n") {
        let mut joined = String::new();
        for (n, part) in content.split("\\\n").enumerate() {
            joined.push_str(if n == 0 { part } else { part.trim_start() });
        }
        return needle_folds_an_item_lead(&joined, edge_reader);
    }
    if !edge_reader && !content.trim_start().contains([' ', '(']) {
        return false;
    }
    let (lead, rest) = crate::test_helpers::item_lead(content);
    if lead == crate::test_helpers::ItemLead::Bare {
        return false;
    }
    let rest = rest.trim();
    rest.is_empty() || ITEM_LEAD_KEYWORDS.contains(&rest)
}

/// Every place a source body folds an item's visibility or qualifier lead by
/// hand, as the byte offset of the literal that spells the fold.
///
/// `code` is the body read as CODE
/// ([`crate::test_helpers::blank_non_code`]), which blanks every literal's
/// BODY and every comment while keeping each byte's position, so the quotes
/// left standing in it are exactly the literal delimiters of real code — a
/// needle written in a comment, or nested inside another literal, has no quotes
/// of its own there. Each pair's content is then read back off the RAW body
/// between the same two offsets. The caller passes the mask in, so a file is
/// read as code once however many literals it holds.
fn hand_folded_lead_offsets(body: &str, code: &str) -> Vec<usize> {
    debug_assert_eq!(
        body.len(),
        code.len(),
        "the code view indexes the raw body, so the two must agree byte for byte"
    );
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(open) = code[from..].find('"') {
        let open = from + open;
        let Some(close) = code[open + 1..].find('"') else {
            break;
        };
        let close = open + 1 + close;
        from = close + 1;
        // rustfmt puts a long call's argument on the row below it, so the
        // reader is looked for past the whitespace before the quote.
        let before = code[..open].trim_end();
        let edge_reader = ITEM_LEAD_EDGE_READERS.iter().any(|r| before.ends_with(r));
        if needle_folds_an_item_lead(&body[open + 1..close], edge_reader) {
            hits.push(open);
        }
    }
    hits
}

/// The lead folds one file's test region spells by hand, each as
/// `<rel>:<line>: <text>`, less the ones a hatch exempts.
fn lead_fold_offenders(rel: &str, body: &str) -> Vec<String> {
    let hatch = concat!("item-lead", "-ok:");
    let code = crate::test_helpers::blank_non_code(body);
    let lines: Vec<&str> = body.lines().collect();
    let mut offenders = Vec::new();
    for at in hand_folded_lead_offsets(body, &code) {
        let n = body[..at].matches('\n').count();
        let line = lines.get(n).copied().unwrap_or_default();
        let above = n.checked_sub(1).and_then(|p| lines.get(p).copied());
        if carries_hatch(line, hatch) || above.is_some_and(|a| carries_hatch(a, hatch)) {
            continue;
        }
        offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
    }
    offenders
}

/// The lead-fold tell reads a FOLD, and never a mention of one.
///
/// The walk below judges a population that is now EMPTY — every fold the tree
/// held is routed — so what proves the tell can still see one is this fixture:
/// one case per placement it has to tell apart, each taken from a real line
/// this tree held before the sweep, plus both halves of the hatch.
#[test]
fn the_item_lead_tell_reads_a_fold_and_not_a_mention() {
    let folds = [
        r#"if code.contains("pub fn ") {}"#,
        // A bare lead word under an edge reader: the fold whose `pub` also
        // matches `publish`.
        r#"if code.starts_with("pub") {}"#,
        r#"let rest = code.strip_prefix("default");"#,
        // The end half of every reader pair folds the same lead as its start
        // twin: `ends_with("pub")` also matches `sub`.
        r#"if head.ends_with("pub") {}"#,
        r#"let t = name.trim_end_matches("unsafe");"#,
        r#"let t = name.trim_matches("default");"#,
        r#"if line.strip_suffix("unsafe").is_some() {}"#,
        // One needle across two rows, the way the compiler reads it.
        "if code.contains(\"pub \\\n    fn \") {}",
        // The same continuation as a CRLF file spells it.
        "if code.contains(\"pub \\\r\n    fn \") {}",
        r#"if code.starts_with("pub ") {}"#,
        r#"if code.starts_with("pub(") {}"#,
        r#"let rest = code.strip_prefix("pub(crate) ");"#,
        r#"if head.ends_with("unsafe fn ") {}"#,
        r#"let t = code.trim_start_matches("default ");"#,
        r#"let n = 1; if code.contains("pub fn ") {}"#,
        "if code.contains(\n    \"pub fn \",\n) {}",
    ];
    for fold in folds {
        let code = crate::test_helpers::blank_non_code(fold);
        assert_eq!(
            hand_folded_lead_offsets(fold, &code).len(),
            1,
            "a hand-written lead fold is one hit:\n{fold}"
        );
    }

    let mentions = [
        r#"if code.contains("pub enum BackupCommand {") {}"#,
        r#"let at = body.find("pub struct SourceListEntry {");"#,
        r#"if !line.contains("pub const SOURCES_SECTION") {}"#,
        r#"if name.starts_with("public_api") {}"#,
        r#"assert!(ok, "default registry backend must be sops");"#,
        r#"// code.contains("pub fn ") is how this used to be written"#,
        r##"let sample = r#"a.contains("pub fn ")"#;"##,
        r#"let n = 1; let fixture = "pub fn kept() {}";"#,
        r#"let profile = "default";"#,
        r#"assert_eq!(name, "pub");"#,
        r#"if name == "default" {}"#,
        r#"if code.contains("pub") {}"#,
        r#"let by_name = map.get("default");"#,
        // A continued literal the walk must read without panicking, and which
        // joins to a needle that names one declaration.
        "let held = \"pub enum \\\n    BackupCommand {\";",
        "let held = 1;\nlet fixture = \"pub fn kept() {}\";",
    ];
    for mention in mentions {
        let code = crate::test_helpers::blank_non_code(mention);
        assert!(
            hand_folded_lead_offsets(mention, &code).is_empty(),
            "a lead word that folds nothing is not a fold:\n{mention}"
        );
    }

    let bare = r#"let e = line.starts_with("pub ");"#;
    assert_eq!(
        lead_fold_offenders("fixture.rs", bare).len(),
        1,
        "an unhatched fold is reported: {bare}"
    );
    let above = "// item-lead-ok: the subject is the visibility itself\nlet e = line.starts_with(\"pub \");";
    let trailing =
        "let e = line.starts_with(\"pub \"); // item-lead-ok: the subject is the visibility itself";
    for hatched in [above, trailing] {
        assert!(
            lead_fold_offenders("fixture.rs", hatched).is_empty(),
            "a hatch on the line or the one above exempts the fold:\n{hatched}"
        );
    }
}

/// No walk folds an item's visibility or qualifier lead off by hand.
///
/// `starts_with("pub ")` misses `pub(crate)`, `pub(super)` and `pub(in path)`;
/// a list of four spellings misses the fifth; and none of them see `unsafe fn`
/// or `default fn`. Each miss is silent — the scanner reads the item as
/// something other than what it is and walks on — and the tree held such folds
/// in six files, one of which let every `pub(crate) mod tests;` declaration out
/// of the production half. [`crate::test_helpers::item_lead`] is the one
/// spelling of the list: it folds the lead and says whether that lead was a
/// VISIBILITY, which is what a reader asking whether a declaration is public
/// reads; spelling `pub` by hand misses it. [`item_keyword`] asks what the item
/// is, [`crate::test_helpers::opens_function`] whether it is a function, and
/// `declared_fn_name` what it is called.
///
/// A site whose subject is genuinely not an item's lead says so with
/// `// item-lead-ok: <why>` on the line or the one above. There are none today.
#[test]
fn no_test_scope_scanner_folds_an_item_lead_by_hand() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        let body = crate::test_helpers::test_region_of(&path);
        let rel = crate::to_posix_string(path.strip_prefix(workspace_root()).unwrap_or(&path));
        offenders.extend(lead_fold_offenders(&rel, &body));
    }
    assert!(
        offenders.is_empty(),
        "read the item head through `cfgd_core::test_helpers::{{item_lead, \
         item_keyword, opens_function, declared_fn_name}}`, which fold every \
         visibility and qualifier lead once — or say why this needle is not an \
         item lead with `// item-lead-ok: <why>`:\n{}",
        offenders.join("\n")
    );
}

/// A hatch is read off a source line through
/// [`crate::test_helpers::carries_hatch`]; a bare `contains` misreads it.
///
/// `"/// name-row-ok: …"` contains `"// name-row-ok:"`, so a lookup asking only
/// whether a line holds the marker accepts a rustdoc line as the hatch, and
/// this tree is full of rustdoc lines that QUOTE a marker while describing its
/// rule. Any of them sitting above an offending item silently exempts it, which
/// is a walk reporting a population it never judged. `carries_hatch` refuses a
/// `///` or `//!` line and accepts both shapes a hatch is really written in.
///
/// `starts_with` and `strip_prefix` are not judged: both anchor at the start of
/// the trimmed line, and a marker spelled with its own `// ` prefix cannot match
/// a `///` line, while one spelled without it cannot match a comment line at
/// all. `contains`, `split_once` and `find` are the three that read the marker
/// from anywhere on the line, so they are the three that can be fooled.
///
/// Two escapes the tell cannot see, stated here. A marker const named outside
/// the `*_HATCH` / `*_MARKER` / `marker` / `hatch` convention is invisible to
/// [`names_a_hatch_marker`], which has the name alone to go on; `NOT_A_CHILD`
/// is the one such const today and it is routed. And the statement scan below
/// asks only whether a routed call appears; it never asks which marker, so a
/// chain filtering on one marker and destructuring another would pass. That
/// same latitude is what lets the legitimate filter-then-destructure shape
/// work.
///
/// A site whose subject is not a source line — a fixture's own rows, a rendered
/// screen — says so with `// doc-comment-ok: <why>` on the line or the one above.
#[test]
fn every_hatch_a_walk_reads_comes_from_the_one_line_reader() {
    // Spelled in parts, or the walk's own needles are its first offenders.
    let reads = [
        concat!(".contains", "("),
        concat!(".split_once", "("),
        concat!(".find", "("),
    ];
    let routed = [
        concat!("carries_", "hatch("),
        concat!("is_plain_", "line_comment("),
    ];
    let hatch = concat!("doc-comment", "-ok:");
    let mut offenders = Vec::new();
    let mut per_crate: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for path in workspace_rust_files() {
        let body = crate::test_helpers::test_region_of(&path);
        let lines: Vec<&str> = body.lines().collect();
        let mut read_here = 0usize;
        for (n, line) in lines.iter().enumerate() {
            let code = crate::test_helpers::code_line(line);
            let mut reads_a_marker = false;
            for read in reads {
                let mut from = 0;
                while let Some(at) = code[from..].find(read) {
                    from = from + at + read.len();
                    let argument: String = code[from..]
                        .trim_start()
                        .trim_start_matches('&')
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    // The literal is blanked out of `code`, so the marker it
                    // spells is read off the raw line at the same position.
                    let spells_a_marker = line[from..].trim_start().starts_with('"')
                        && line[from..]
                            .split('"')
                            .nth(1)
                            // doc-comment-ok: the literal is this walk's own subject
                            .is_some_and(|literal| literal.contains("-ok:"));
                    reads_a_marker |= spells_a_marker || names_a_hatch_marker(&argument);
                }
            }
            let routed_here = routed.iter().any(|call| line.contains(call));
            if routed_here || reads_a_marker {
                read_here += 1;
            }
            if !reads_a_marker || routed_here {
                continue;
            }
            // The routed call may sit on an earlier line of the same method
            // chain, which is where a filter belongs when the read that follows
            // it destructures what it kept.
            let mut statement = vec![*line];
            for above in lines[..n].iter().rev() {
                let ended = crate::test_helpers::code_line(above);
                let ended = ended.trim_end();
                if ended.ends_with(';') || ended.ends_with('{') || ended.ends_with('}') {
                    break;
                }
                statement.push(above);
            }
            if statement
                .iter()
                .any(|l| routed.iter().any(|call| l.contains(call)))
            {
                continue;
            }
            let above = n.checked_sub(1).map(|p| lines[p]).unwrap_or_default();
            if carries_hatch(line, hatch) || carries_hatch(above, hatch) {
                continue;
            }
            offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
        }
        if read_here > 0 {
            let owner = path
                .strip_prefix(workspace_root().join("crates"))
                .ok()
                .and_then(|rest| rest.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            *per_crate.entry(owner).or_default() += read_here;
        }
    }
    assert!(
        offenders.is_empty(),
        "read the hatch through `cfgd_core::test_helpers::carries_hatch`, which \
         refuses a `///` or `//!` line so a rustdoc paragraph quoting the marker \
         cannot exempt the item below it — or say why this subject is no source \
         line with `// doc-comment-ok: <why>`:\n{}",
        offenders.join("\n")
    );
    for (owner, floor) in FLOORS {
        let found = per_crate.get(owner).copied().unwrap_or_default();
        assert!(
            found >= floor,
            "the walk read {found} marker lookups in {owner}; it has stopped \
             reading the population it judges"
        );
    }
}

/// The marker lookups each crate holds, so a walk that stopped reading one of
/// them fails by that crate's name; the other's count cannot fill it.
const FLOORS: [(&str, usize); 2] = [("cfgd", 79), ("cfgd-core", 28)];

/// Every fence in this file is a claim about a POPULATION, so the walk that
/// enumerates it fails when it cannot return all of it.
///
/// Both failures are read off the panic's message: a walk that swallowed the directory it could not
/// open still ends up with nothing to return, so "I could not look" and "there was nothing there"
/// arrive as the same empty list, and only the wording — the directory's own name included — tells
/// them apart.
///
/// The success arm asserts the whole vector, because the three properties
/// every caller reads off this list are separable and each fails silently on
/// its own: a walk that stopped descending, one that stopped filtering, and
/// one that returned the filesystem's order all answer a length check.
#[test]
fn the_source_walk_fails_on_a_root_it_cannot_open_and_lists_every_source_under_one_it_can() {
    fn walk_failure(root: &Path) -> String {
        match std::panic::catch_unwind(|| rust_sources_under(root)) {
            Ok(files) => format!("the walk returned {} files", files.len()),
            Err(payload) => payload
                .downcast::<String>()
                .map(|m| *m)
                .unwrap_or_else(|_| String::from("a panic carrying no message")),
        }
    }

    let root = tempfile::tempdir().unwrap_or_else(|e| panic!("temp dir: {e}"));

    let missing = root.path().join("nothing-here");
    let unopenable = walk_failure(&missing);
    assert!(
        unopenable.contains("must read every directory") && unopenable.contains("nothing-here"),
        "a root the walk could not open answered: {unopenable}"
    );

    let empty = walk_failure(root.path());
    assert!(
        empty.contains("found no sources"),
        "a root holding no source answered: {empty}"
    );

    let nested = root.path().join("sub");
    std::fs::create_dir(&nested).unwrap_or_else(|e| panic!("{}: mkdir: {e}", nested.display()));
    for (file, body) in [
        (root.path().join("z.rs"), "fn z() {}\n"),
        (root.path().join("m.rs"), "fn m() {}\n"),
        (nested.join("a.rs"), "fn a() {}\n"),
        (root.path().join("notes.txt"), "not a source\n"),
    ] {
        std::fs::write(&file, body).unwrap_or_else(|e| panic!("{}: write: {e}", file.display()));
    }
    assert_eq!(
        rust_sources_under(root.path()),
        vec![
            root.path().join("m.rs"),
            nested.join("a.rs"),
            root.path().join("z.rs")
        ],
        "every .rs under the root, at every depth, nothing else, sorted"
    );
}

/// The line that CLOSES a multi-line literal is source after the closing
/// delimiter.
///
/// Read as masked whole, a `"#; }` line loses the brace that ends the
/// function, and the scan runs on into the next declaration — a slice that
/// ends somewhere other than where the code does, reported by nothing.
#[test]
fn a_brace_after_a_literals_close_still_ends_the_function() {
    let body = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "{ { {\n",
        "\"#; }\n",
        "fn sibling() {}\n"
    );
    let lines: Vec<&str> = body.lines().collect();
    let source = function_source(&FIXTURE_SOURCE, &lines, 0, lines[0]);
    assert!(
        source.ends_with("\"#; }"),
        "the slice ends at the brace after the literal's close: {source:?}"
    );
    assert!(
        !source.contains("sibling"),
        "the slice must not run into the next declaration: {source:?}"
    );
}

/// Braces inside a multi-line raw literal move no `impl` depth.
///
/// Counted as source, the two closes inside the literal below end the block
/// early and every declaration after them is named after the wrong type — or
/// after none, which reads as a free function and is followed by bare name.
#[test]
fn an_unbalanced_literal_inside_an_impl_does_not_close_its_owner() {
    let body = concat!(
        "impl Holder {\n",
        "    fn f() {\n",
        "        let s = r#\"\n",
        "}\n",
        "}\n",
        "\"#;\n",
        "    }\n",
        "}\n",
        "fn after() {}\n"
    );
    let lines: Vec<&str> = body.lines().collect();
    let owners = impl_owners(&lines);
    assert_eq!(owners[1].as_deref(), Some("Holder"), "{owners:?}");
    assert_eq!(
        owners[6].as_deref(),
        Some("Holder"),
        "the literal's braces closed the block early: {owners:?}"
    );
    assert_eq!(
        owners[8], None,
        "the block's real close drops it: {owners:?}"
    );
}

/// A declaration sharing its line with a literal's close opens a slice.
///
/// The reverse of the brace mistake: a function the walk never cuts out is a
/// function whose tells are read as the previous one's, so a needle in it is
/// exempted by whatever the neighbour above happened to carry.
#[test]
fn a_declaration_after_a_literals_close_opens_a_slice() {
    let body = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "fn hidden() {\n",
        "\"#; }\n",
        "fn after() {}\n"
    );
    let cut = source_functions(&FIXTURE_SOURCE, body);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 5],
        "a declaration inside the literal opened a slice, or the one after \
         its close did not: {opens:?}"
    );

    // Each holder closes on the same line its literal does: a slice ends at its
    // own brace, so a fixture leaving one open is a body with no end, and the
    // scan reports it as a desync.
    let closing = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "\"#; fn sibling() { let x = 1; } }\n",
        "fn after() {}\n"
    );
    let cut = source_functions(&FIXTURE_SOURCE, closing);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 3, 4],
        "the declaration on the literal's closing line opened no slice: {opens:?}"
    );

    let quoted = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "\"#; let s = \"a;b\"; fn sibling() {} }\n",
        "fn after() {}\n"
    );
    let cut = source_functions(&FIXTURE_SOURCE, quoted);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 3, 4],
        "a `;` inside the tail's own literal ended it: {opens:?}"
    );

    // Both tells sit inside the tail's literal, so a scan that reads the raw
    // remainder finds a terminator and a declaration and opens on a `fn` that
    // is a string.
    let inside = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "\"#; let s = \"; fn ghost() {\"; }\n",
        "fn after() {}\n"
    );
    let cut = source_functions(&FIXTURE_SOURCE, inside);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 4],
        "a declaration spelled inside the tail's literal opened a slice: {opens:?}"
    );

    // A comment's text is not code either: the compiler never reads it, so a
    // terminator and a declaration written there describe nothing the file
    // does, and a scan that stopped at literals would still open on them.
    let commented = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "\"#; } // ; fn f() {}\n",
        "fn after() {}\n"
    );
    let cut = source_functions(&FIXTURE_SOURCE, commented);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 4],
        "a declaration spelled in the tail's comment opened a slice: {opens:?}"
    );
}

/// A slice ends at its declaration's own close.
///
/// Everything between two declarations is file scope, and cut at the next `fn`
/// it lands in the first one's slice: a `const` holding a YAML fixture then
/// reads as the body of the test above it, which is how a test that declares
/// nothing became a member of the shell-items population — judged, and floored,
/// on text it does not contain.
#[test]
fn a_slice_ends_at_its_declarations_own_close_not_at_the_next_fn() {
    let body = concat!(
        "fn subject() {\n",
        "    let x = 1;\n",
        "}\n",
        "\n",
        "const FIXTURE: &str = \"aliases: - name: a\";\n",
        "\n",
        "fn sibling() {}\n"
    );
    let cut = source_functions(&FIXTURE_SOURCE, body);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(opens, vec![1, 7], "{opens:?}");
    assert!(
        !cut[0].1.contains("FIXTURE"),
        "the file-scope const between the two declarations landed in the \
         slice: {:?}",
        cut[0].1
    );
}

/// The uncalled-entry hatch is read off the roster's own line.
///
/// A needle this file spells in more than one table would otherwise be
/// exempted by a hatch beside any of them, and a reader checking why an entry
/// counts nothing would find the roster line bare.
#[test]
fn an_uncalled_entry_hatch_is_read_only_inside_the_roster() {
    let source = concat!(
        "const ENV_MUTATION_SEEDS: &[&str] = &[\n",
        "    // env-mutator-uncalled-ok: a hatch beside the OTHER table.\n",
        "    \"env::set_var\",\n",
        "];\n",
        "\n",
        "const ENV_MUTATORS: &[&str] = &[\n",
        "    \"env::set_var\",\n",
        "    // env-mutator-uncalled-ok: the reason a reader of the roster sees.\n",
        "    \"EnvVarGuard::set\",\n",
        "];\n"
    );
    let lines: Vec<&str> = source.lines().collect();
    assert!(
        !roster_entry_hatched(&lines, "env::set_var"),
        "a hatch on another table's line exempted the roster entry"
    );
    assert!(
        roster_entry_hatched(&lines, "EnvVarGuard::set"),
        "the hatch on the roster's own line was not read"
    );
}

/// The tests
/// [`every_in_process_test_declaring_shell_items_holds_a_test_home`]
/// classifies as declaring shell items AND running a verb in this process:
/// the population it finds today.
///
/// The floor is a minimum the population must keep, so a member cannot be
/// removed silently, and a count above it passes; [`SERIAL_FLOORS`] works the
/// same way. A member that falls out of needle reach (a fixture respelled, a
/// verb renamed) is the finding, and a floor set below the count would absorb
/// it silently. Adding a member raises this; a count that FELL is never
/// re-calibrated downward to make a failing check pass.
///
/// It stood at 4 while [`source_functions`] cut a slice at the next `fn`: the
/// file-scope consts under `module_upgrade_happy_human_json` carry a YAML
/// fixture, so its slice held a declaration the test itself does not make and
/// it counted as a member. The SLICE moved and the population stayed: the
/// three below are the tests that ever declared shell items and drove a verb.
const TEST_HOME_JUDGED_FLOOR: usize = 3;

/// The slice's logical lines, with every line that BEGAN inside a literal or a
/// block comment joined onto the line that opened it.
///
/// A YAML fixture is a raw literal spanning many physical lines, so a
/// declaration's two tells — the `env:`/`aliases:` key and its `- name:` item
/// — land on two of them, and a needle reading one line at a time cannot see
/// the shape half the fixtures in the directory are written in.
/// [`crate::test_helpers::logical_source_lines`] folds a `\`-continued literal
/// for that same reason and deliberately leaves a raw one alone, because its
/// other callers walk PRODUCTION sources where joining a literal's lines would
/// pair a needle with a hatch comment that is not its own. [`LineMask`] already
/// carries the state this walk needs, so the fold is local to the walk that
/// wants it.
///
/// The number each line comes back with is the PHYSICAL one within `slice`,
/// counted off the line the fold opened on. Counted off the folded text
/// instead, it drifts one further from the file with every line folded away —
/// and a caller reporting an offender by that number sends its reader to a
/// line holding something else, which is the whole use a line number has.
fn literal_folded_lines(slice: &str) -> Vec<(usize, String)> {
    let mut mask = LineMask::default();
    let mut folded = String::with_capacity(slice.len());
    let mut physical: Vec<usize> = Vec::new();
    for (n, line) in slice.lines().enumerate() {
        let continues = mask.masked();
        mask.advance(line);
        if n > 0 {
            folded.push(if continues { ' ' } else { '\n' });
        }
        if !continues || n == 0 {
            physical.push(n + 1);
        }
        folded.push_str(line);
    }
    crate::test_helpers::logical_source_lines(&folded)
        .into_iter()
        .map(|(at, line)| (physical.get(at - 1).copied().unwrap_or(at), line))
        .collect()
}

/// A folded line is reported at the physical line it opened on.
///
/// The fixture is the shape the walk exists for: a raw literal spanning three
/// physical lines, whose tells fold onto the line that opened it, followed by a
/// declaration the folded text has moved up by two. A number counted off the
/// folded text names that declaration's line as 3, which in the file holds the
/// middle of the fixture.
#[test]
fn the_literal_fold_reports_the_physical_line_a_logical_line_opens_on() {
    let slice = concat!(
        "fn holder() {\n",
        "    let fixture = r#\"\n",
        "aliases:\n",
        "  - name: a\n",
        "\"#;\n",
        "    let after = 1;\n",
        "}\n"
    );
    let folded = literal_folded_lines(slice);
    let at = |needle: &str| {
        folded
            .iter()
            .find(|(_, line)| line.contains(needle))
            .map(|(n, _)| *n)
    };
    assert_eq!(
        at("aliases:"),
        Some(2),
        "the line the literal opened on: {folded:?}"
    );
    assert_eq!(
        at("let after"),
        Some(6),
        "the physical line `let after` sits on: {folded:?}"
    );
}

/// An integration test that declares shell items and drives a `cmd_*` in this
/// process holds a test HOME.
///
/// The env check resolves `~` for everything it does — the managed env files,
/// their rc source lines, and the `~/` fold every path it prints passes
/// through. The reconciler's own unguarded-home fallback does not cover it:
/// `expand_tilde` has no test arm, so a test that declares `spec.env` or
/// `spec.aliases` and then calls a verb in-process reports the INVOKING USER'S
/// env surface. That is the worst split a fixture can have — a development box
/// dogfoods cfgd and already holds every planned target, so the report is the
/// declaration's and the golden passes; a CI runner's `$HOME` holds none of
/// them, so five absence rows render under a home the fold does not even
/// recognize. The local run is the one that decides whether the change ships.
///
/// Judged per test function, on its own body: the declaration is a YAML
/// env/alias list written into a fixture, the verb an in-process `cmd_*` call,
/// the guard `with_test_home_guard` or a `HOME` handed to a spawned child.
/// Its ceiling is the same body — a declaration a same-file helper writes is
/// out of reach, and is not the shape that shipped: the test that broke every
/// runner appended `aliases:` to its own profile and called the verb three
/// lines below.
///
/// The `HOME` credit is per-body for the same reason, and that direction is
/// the deliberately strict one: a spawning fixture sets `.env("HOME", …)` in a
/// shared helper, so a test that calls such a helper AND drives an in-process
/// verb over its own declaration is flagged despite the spawn — correctly,
/// because the child's `HOME` is not what the in-process check reads, and the
/// guard belongs in the body that runs the verb.
///
/// Lines are read through [`literal_folded_lines`]: a fixture written
/// as `r#"…"#` carries the declaration's two tells on two physical lines.
///
/// The verb is ANY `cmd_*`, which over-approximates on purpose: the guard is
/// one line and costs a test that resolves no path nothing, while a roster of
/// the verbs that can reach `~` is exactly the list that goes stale — a verb
/// gaining an env surface would leave every fixture below it unwatched, which
/// is the failure this walk exists to prevent.
#[test]
fn every_in_process_test_declaring_shell_items_holds_a_test_home() {
    let mut tests_seen = 0usize;
    let mut judged = Vec::new();
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        // A crate's integration tests: `src/` holds the unit tests, which reach
        // the check through the same `~` but are the library's own and are
        // covered by their crate's fixtures.
        let posix = crate::to_posix_string(&path);
        if !crate::test_helpers::is_test_source(&path) || posix.contains("/src/") {
            continue;
        }
        // unfloored-slice-ok: an integration test file is test code whole.
        let body = walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        let relative = source_label(&path);

        for (open, slice) in source_functions(&relative, &body) {
            let Some(name) = declared_fn_name(&slice) else {
                continue;
            };
            let start = attribute_block_start(&lines, open - 1);
            if !lines[start..open - 1].iter().any(|l| {
                let t = l.trim_start();
                t.starts_with("#[test]") || t.starts_with("#[tokio::test")
            }) {
                continue;
            }
            tests_seen += 1;
            let folded = literal_folded_lines(&slice);
            // A declared entry is a YAML list item under the `env:` or
            // `aliases:` key — the two keys alone would read every `env: vec![]`
            // struct field in the directory as a declaration.
            let declares = folded.iter().any(|(_, l)| {
                l.contains("- name:") && (l.contains("aliases:") || l.contains("env:"))
            });
            let in_process = folded.iter().any(|(_, l)| l.contains("cmd_"));
            if !(declares && in_process) {
                continue;
            }
            judged.push(format!("{relative}:{open}: {name}"));
            if !folded
                .iter()
                .any(|(_, l)| l.contains("with_test_home") || l.contains(".env(\"HOME\""))
            {
                offenders.push(format!("{relative}:{open}: {name}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a test declaring env vars or aliases and running a verb in this \
         process reads the invoking user's own `~` — install \
         `cfgd_core::with_test_home_guard(tmp)` before the verb and plant the \
         surface through `MergedEnvItems::managed_env_files` / \
         `managed_env_source_lines`:\n{}",
        offenders.join("\n")
    );
    assert!(
        tests_seen > 300,
        "the walk read {tests_seen} integration tests; it has stopped seeing them"
    );
    assert!(
        judged.len() >= TEST_HOME_JUDGED_FLOOR,
        "the walk classified {} tests as declaring shell items and running a \
         verb, under its floor of {TEST_HOME_JUDGED_FLOOR} — a member has \
         fallen out of needle reach:\n{}",
        judged.len(),
        judged.join("\n")
    );
}

/// Loose on purpose: the golden population grows with every new render test,
/// so this floor says only that the derived roots still hold goldens at all.
const GOLDEN_FLOOR: usize = 300;

/// The padded-header count is a minimum the goldens must keep, so a member
/// cannot be removed silently; a count above it passes.
const GOLDEN_HEADER_FLOOR: usize = 7;

/// `docs/` carries rendered tables too, and one of them keeps a padded header;
/// the floor says the docs half of the walk still reads a captured render.
const DOCS_HEADER_FLOOR: usize = 1;

/// The shapes a snapshot root holds that are NOT a rendered capture: `-o json`
/// payloads, and the `cfgd skill` installer's own artifacts (`.md`, `.mdc`,
/// `.toml`). The goldens themselves are `.txt`, so an extension named by
/// neither list is either a render captured under a new extension — which the
/// walk would skip in silence, its floor none the wiser — or a shape nobody
/// classified. Naming it is what forces the classification before it ships.
///
/// The ceiling of that: this roster CLASSIFIES, it cannot VERIFY. A rendered
/// capture saved under a listed extension passes unjudged, and widening the
/// roster silences the walk exactly as well as classifying honestly does.
/// What the roster buys is that the widening is a visible edit reviewed
/// against this sentence, where a new file under a root goes unnoticed.
const NON_GOLDEN_SNAPSHOT_EXTENSIONS: &[&str] = &["json", "md", "mdc", "toml"];

/// Every file under `dir`. `target/` is skipped: a build tree mirrors
/// captured renders the walk has already read, under paths no reader ever
/// ships.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
            panic!("{}: the walk must read every directory: {e}", dir.display())
        });
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| {
                panic!("{}: the walk must read every entry: {e}", dir.display())
            });
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// Every snapshot root the workspace holds is named in
/// [`KNOWN_GOLDEN_ROOTS`], and every named one exists.
///
/// [`crate::test_helpers::snapshot_golden_roots`] already fails on a root that
/// was renamed away; this is the other direction. A root created and left
/// unnamed is walked — the derivation is what the walks read — but nothing
/// then says which population the floors are floors OVER, so a later rename of
/// it shrinks the walk by however many goldens it held while every floor still
/// passes. The equality is what makes each root's disappearance loud on its
/// own name.
#[test]
fn every_golden_root_is_named() {
    let root = workspace_root();
    let mut derived: Vec<String> = snapshot_golden_roots()
        .iter()
        .map(|r| crate::to_posix_string(r.strip_prefix(&root).unwrap_or(r)))
        .collect();
    derived.sort();
    let mut named: Vec<String> = KNOWN_GOLDEN_ROOTS
        .iter()
        .map(|r| (*r).to_string())
        .collect();
    named.sort();
    assert_eq!(
        derived, named,
        "a snapshot root the workspace holds is not named in \
         `KNOWN_GOLDEN_ROOTS`, or a named one no longer exists; name the new \
         root there so its goldens are guarded by name as well as by the \
         population's floor"
    );
}

/// How many whitespace-terminated lines of `path` are table HEADERS, and the
/// ones that are not, named for a reader.
///
/// A header is read off the structure, ANSI-free: the next non-blank line
/// under it is the `─` rule the renderer emits immediately after it, which is
/// why nothing can be interposed between the two.
fn trailing_space_lines(path: &Path) -> (usize, Vec<String>) {
    let text = walked_file_body(path);
    // `str::lines` drops a trailing `\r` only where the line ended in `\n`, so
    // a CRLF checkout does not read every line as whitespace-terminated, and it
    // borrows each line with no second copy of the file. A file whose
    // last line ends on a lone `\r` therefore reads as whitespace-terminated by
    // design — no golden carries a `\r` byte, and `.gitattributes` pins
    // `eol=lf` on every checkout. Collected because a header is read off the
    // line AFTER the padded one.
    let lines: Vec<&str> = text.lines().collect();
    let (mut headers, mut offenders) = (0usize, Vec::new());
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() || !line.ends_with(char::is_whitespace) {
            continue;
        }
        let ruled = lines[i + 1..]
            .iter()
            .find(|n| !n.trim().is_empty())
            .is_some_and(|n| n.trim_start().starts_with('─'));
        if ruled {
            headers += 1;
        } else {
            offenders.push(format!(
                "{}:{}: {line:?}",
                crate::to_posix_string(path),
                i + 1
            ));
        }
    }
    (headers, offenders)
}

/// Every committed golden under every snapshot root, every markdown page under
/// `docs/`, and every line of one that ends in whitespace is a table HEADER.
///
/// A table pads its last column so the `──` rule spans the same width the
/// header does — cfgd tables carry no vertical borders, so the header's own
/// pad is the only thing the rule can agree with. A DATA row has nothing to
/// its right, so its pad buys only trailing bytes: invisible on a terminal,
/// but real in a pipe, in a copied selection, in a golden and in a captured
/// render pasted into the documentation. `Renderer::render_table` ends a data
/// row on its last glyph; this walk is what says so for the SHIPPED
/// population, so a golden re-blessed from a regressed renderer, or a doc
/// block pasted from one, is caught by the shipped bytes with no reader
/// having to spot it.
///
/// The roots are DERIVED — every directory under `crates/` named `snapshots`,
/// `output_snapshots` or `golden` — so a render-golden root joins the
/// population the day it is created, and every one of them is asserted by name
/// ([`every_golden_root_is_named`]), so a rename fails loudly and the walk
/// cannot shrink in silence. The derivation is
/// [`crate::test_helpers::snapshot_golden_roots`], which the `cfgd` crate's
/// own golden walks read too: the crates compile separately, and a second
/// derivation one crate over is what left eleven goldens guarded by this walk
/// and by neither of those. The goldens are the `.txt` files under those
/// roots, and the walk states the COMPLEMENT too: every other file under one
/// carries an extension [`NON_GOLDEN_SNAPSHOT_EXTENSIONS`] names, so a render
/// captured under a new one has to be classified before the walk passes.
#[test]
fn every_trailing_space_in_a_golden_belongs_to_a_table_header() {
    let root = workspace_root();
    let goldens = snapshot_goldens(&["txt"]);
    let unclassified: Vec<String> = snapshot_root_files()
        .iter()
        .filter(|f| {
            !f.extension().is_some_and(|e| {
                e == "txt"
                    || NON_GOLDEN_SNAPSHOT_EXTENSIONS
                        .iter()
                        .any(|known| e == *known)
            })
        })
        .map(crate::to_posix_string)
        .collect();
    assert!(
        unclassified.is_empty(),
        "a snapshot root holds a file under an extension this walk classifies \
         as neither a golden nor a known non-rendered shape; if it is a \
         rendered capture the walk is skipping it, and if it is not, name its \
         extension in `NON_GOLDEN_SNAPSHOT_EXTENSIONS`:\n{}",
        unclassified.join("\n")
    );
    let mut docs: Vec<PathBuf> = files_under(&root.join("docs"))
        .into_iter()
        .filter(|f| f.extension().is_some_and(|e| e == "md"))
        .collect();
    docs.sort();

    let mut offenders = Vec::new();
    let mut headers = 0usize;
    for path in &goldens {
        let (found, bad) = trailing_space_lines(path);
        headers += found;
        offenders.extend(bad);
    }
    let mut doc_headers = 0usize;
    for path in &docs {
        let (found, bad) = trailing_space_lines(path);
        doc_headers += found;
        offenders.extend(bad);
    }
    assert!(
        offenders.is_empty(),
        "a rendered line ends on its last glyph unless it is a table header, \
         whose pad is what the `──` rule spans; re-bless the golden from a \
         renderer that does not pad a data row's last cell, or re-capture the \
         documentation block from one:\n{}",
        offenders.join("\n")
    );
    assert!(
        goldens.len() >= GOLDEN_FLOOR && headers >= GOLDEN_HEADER_FLOOR,
        "the walk saw {headers} table headers across {} goldens; it has \
         stopped reaching the snapshot roots",
        goldens.len()
    );
    assert!(
        doc_headers >= DOCS_HEADER_FLOOR,
        "the walk saw {doc_headers} table headers across {} documentation \
         pages; it has stopped reading the captured renders in docs/",
        docs.len()
    );
}

/// Every label-bearing type cfgd-schema owns, with the `(canonical token,
/// display label)` pairs read off its own `ALL` — so a new VARIANT is covered
/// by construction. The type list is the only hand-written half, and
/// [`every_display_label_is_the_lowercase_of_its_canonical_token`] checks it
/// against the source.
fn labelled_schema_types() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "FileStrategy",
            cfgd_schema::FileStrategy::ALL
                .iter()
                .map(|v| (v.as_str(), v.method_label()))
                .collect(),
        ),
        (
            "ScheduleOwner",
            cfgd_schema::ScheduleOwner::ALL
                .iter()
                .map(|v| (v.as_str(), v.label()))
                .collect(),
        ),
    ]
}

/// Whether a source line declares a display label, judged on the SHAPE of the
/// name, whatever spellings exist today: a third accessor
/// called `owner_label` or `phase_label` joins the population without editing
/// this walk.
fn declares_a_display_label(line: &str) -> bool {
    crate::test_helpers::declared_fn_name(line).is_some_and(|name| name.ends_with("label"))
}

/// A display label is the ASCII-lowercase of the canonical token beside it, on
/// every label-bearing type cfgd-schema owns: a hand-written arm returning
/// anything else compiles, and one that reads `Local` where the listing prints
/// `local` would make the two spellings of one value drift.
///
/// The variant population comes from each type's `ALL`; the TYPE population is
/// read back off cfgd-schema's sources, so a third label-bearing type cannot be
/// invisible to this walk the way a hand-listed pair of enums would let it be.
/// The walk lives here because cfgd-schema is a leaf crate: it cannot reach
/// [`crate::test_helpers::production_slice`], and a bare `#[cfg(test)]` anchor
/// of its own goes blind at the first test-only import above the label fns.
#[test]
fn every_display_label_is_the_lowercase_of_its_canonical_token() {
    let table = labelled_schema_types();
    for (ty, pairs) in &table {
        assert!(!pairs.is_empty(), "{ty} states no variants");
        for (token, label) in pairs {
            assert_eq!(
                *label,
                token.to_ascii_lowercase(),
                "{ty}::{token}'s label is not its token lowercased"
            );
        }
    }

    let schema_src = workspace_root().join("crates/cfgd-schema/src");
    let sources: Vec<PathBuf> = workspace_rust_files()
        .into_iter()
        .filter(|p| p.starts_with(&schema_src))
        .collect();
    assert!(
        sources.len() >= 2,
        "the walk reached {} sources under crates/cfgd-schema/src; the crate \
         was moved or renamed",
        sources.len()
    );

    let mut sites: Vec<String> = Vec::new();
    for path in &sources {
        let production = crate::test_helpers::production_slice_of(path);
        let mut current: Option<String> = None;
        for line in production.lines() {
            if let Some(rest) = line.strip_prefix("impl ") {
                current = rest.split_whitespace().next().map(str::to_string);
            }
            if declares_a_display_label(line) {
                sites.push(
                    current
                        .clone()
                        .unwrap_or_else(|| panic!("a label fn outside an impl: {line}")),
                );
            }
        }
    }
    assert!(
        sites.len() >= 2,
        "the walk no longer reaches cfgd-schema's label fns — it found {sites:?}"
    );
    let listed: Vec<&str> = table.iter().map(|(ty, _)| *ty).collect();
    for site in &sites {
        assert!(
            listed.contains(&site.as_str()),
            "{site} states a display label no walk checks; add it to \
             `labelled_schema_types`"
        );
    }
    for ty in &listed {
        assert!(
            sites.iter().any(|s| s == ty),
            "{ty} is listed but states no display label in cfgd-schema"
        );
    }
}

/// The two hyphenated env resource types are matched on in both crates, so a
/// rename has to reach every matcher at once.
///
/// `ENV_RC_RESOURCE_TYPE` / `ENV_SESSION_RESOURCE_TYPE` exist to make that
/// true, and the promise held only while nothing spelled the word instead:
/// five production matchers had drifted to bare literals — the pending-decision
/// prune, the daemon tick's vouching list, and the drift report's own kind
/// vocabulary — each of which a rename would have left matching a type nothing
/// writes. The walk is both crates' production slices; the file DECLARING the
/// constants is the one exception, and a serde or wire spelling that must stay
/// a literal carries `// env-type-literal-ok: <why>` on its own line or the one
/// above it.
#[test]
fn no_production_site_spells_an_env_resource_type_instead_of_its_constant() {
    let declarations = Path::new("reconciler").join("types.rs");
    let mut offenders = Vec::new();
    let mut files_walked = 0usize;
    for path in workspace_rust_files() {
        let is_test_source = crate::test_helpers::is_test_source(&path);
        if path.ends_with(&declarations) || is_test_source {
            continue;
        }
        files_walked += 1;
        let production = crate::test_helpers::production_slice_of(&path);
        let mut hatched = false;
        for (i, line) in production.lines().enumerate() {
            let previous = hatched;
            hatched = carries_hatch(line, "env-type-literal-ok:");
            if previous || hatched || line.trim_start().starts_with("//") {
                continue;
            }
            for literal in ["\"env-rc\"", "\"env-session\""] {
                if line.contains(literal) {
                    offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        files_walked >= 100,
        "the walk no longer reads the workspace's sources: {files_walked} files"
    );
    assert!(
        offenders.is_empty(),
        "match on `cfgd_core::reconciler::ENV_RC_RESOURCE_TYPE` / \
         `ENV_SESSION_RESOURCE_TYPE` instead, so a rename reaches every \
         matcher:\n{}",
        offenders.join("\n")
    );
}

/// Files holding a pin of gc's failed-removal arm, and the number of pins each
/// still has to yield.
///
/// Each file's floor is a minimum the file must keep, so a pin cannot be removed
/// silently and a pin whose shape drifts out of needle reach fails here; a count
/// above the floor passes.
const GC_FAILED_REMOVAL_PINS: &[(&str, usize)] = &[
    ("crates/cfgd-core/src/backup/tests.rs", 2),
    ("crates/cfgd/tests/backup_exit_code.rs", 1),
    ("crates/cfgd/tests/backup_snapshots.rs", 1),
];

/// The calls that run a backup gc collection through a surface that names gc:
/// the orphan collector, the library entry point, the command wrapper and the
/// argv of the real binary.
///
/// Any function driving one of these is a candidate pin of gc's failed-removal
/// arm, in EVERY file, so a future one is judged on what it DOES, whatever its
/// name, and no file has to be named ahead of it. Each
/// spelling names gc and nothing else, so judging the whole workspace on them
/// costs no hatch. `orphaned_snapshots` is deliberately absent — it reads rows
/// and removes nothing, so no pin of the failed-removal arm can be driven
/// through it alone.
const GC_COLLECT_ENTRIES: &[&str] = &[
    "collect_orphans",
    "run_backup_gc",
    "cmd_backup_gc(",
    "\"backup\", \"gc\"",
];

/// The engine harness's own collect call, matched by its argument whatever
/// identifier the harness happens to be bound to (an iterator's own
/// `collect()` takes no argument). The harness is private to `backup/tests.rs`,
/// so this spelling means a gc collection only inside a file that already pins
/// the arm and is read there alone.
const GC_ENGINE_COLLECT: &str = ".collect(&";

/// The shapes that make a backup payload unremovable, read off
/// [`crate::test_helpers::hold_payload_unremovable`]'s own source, so renaming
/// the stand-in's contents or switching the Windows sharing call moves this
/// walk along with the producer.
fn unremovable_payload_tells() -> Vec<String> {
    let path = workspace_root().join("crates/cfgd-core/src/test_helpers.rs");
    let label = source_label(&path);
    let body = walked_file_body(&path);
    let slice = source_functions(&label, &body)
        .into_iter()
        .find(|(_, slice)| declared_fn_name(slice) == Some("hold_payload_unremovable"))
        .map(|(_, slice)| slice)
        .expect("the fixture must still be a free function in test_helpers.rs");
    let stand_in = slice
        .split_once("b\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(literal, _)| literal.to_string())
        .expect("the unix arm must still write a stand-in file with a literal body");
    assert!(
        slice.contains("share_mode("),
        "the Windows arm must still hold the snapshot open through share_mode"
    );
    vec![stand_in, "share_mode(".to_string()]
}

/// Every pin of gc's failed-removal arm reaches its unremovable payload through
/// the one fixture, and no such pin is gated to one operating system.
///
/// The arm is the same on every host — a removal cfgd cannot perform keeps its
/// row — but the reason a kernel refuses one is not, so a pin that hand-rolls
/// the unix shape can only ever run there and the arm goes unproven everywhere
/// else. Both halves are the finding: a stand-in written beside the fixture
/// drifts from it, and a `#[cfg(unix)]` over a pin of this arm silently takes
/// the arm out of the Windows and macOS suites. `// unix-only-gc-ok: <why>`
/// hatches a pin that genuinely asserts a unix-only fact.
#[test]
fn every_gc_failed_removal_pin_holds_its_payload_through_the_one_fixture() {
    let tells = unremovable_payload_tells();
    let mut judged: Vec<(String, String)> = Vec::new();
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        let posix = crate::to_posix_string(&path);
        // The producer states both shapes by definition, and this walk quotes
        // them to find the others.
        if posix.ends_with("cfgd-core/src/test_helpers.rs")
            || posix.ends_with("output/tests/fences.rs")
        {
            continue;
        }
        let relative = source_label(&path);
        // unfloored-slice-ok: the pins judged here are tests.
        let body = walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        let floored = GC_FAILED_REMOVAL_PINS
            .iter()
            .any(|(file, _)| posix.ends_with(file));

        for (open, slice) in source_functions(&relative, &body) {
            let Some(name) = declared_fn_name(&slice) else {
                continue;
            };
            let attributes = &lines[attribute_block_start(&lines, open - 1)..open - 1];
            let hatched = attributes
                .iter()
                .chain(slice.lines().collect::<Vec<_>>().iter())
                .any(|l| carries_hatch(l, "unix-only-gc-ok:"));
            let reaches = slice.contains("hold_payload_unremovable");
            let hand_rolled = tells.iter().any(|tell| slice.contains(tell.as_str()));
            // Anything driving a collection is a candidate pin of this arm
            // whatever it is called, so a future one cannot hide behind a name
            // no needle spells, nor behind a file no floor names yet.
            let drives_a_collection = GC_COLLECT_ENTRIES.iter().any(|call| slice.contains(call))
                || (floored && slice.contains(GC_ENGINE_COLLECT));
            if !(reaches || hand_rolled || drives_a_collection) {
                continue;
            }
            let at = format!("{relative}:{open}: {name}");
            if reaches || hand_rolled {
                judged.push((posix.clone(), at.clone()));
            }
            if hatched {
                continue;
            }
            if hand_rolled && !reaches {
                offenders.push(format!("{at}: hand-rolled unremovable payload"));
            }
            if attributes
                .iter()
                .any(|l| l.trim_start().starts_with("#[cfg(unix)]"))
            {
                offenders.push(format!("{at}: gated to unix"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a pin of gc's failed-removal arm must hold its payload through \
         `cfgd_core::test_helpers::hold_payload_unremovable`, which refuses the \
         removal the way each OS really does, and must run on every OS:\n{}",
        offenders.join("\n")
    );
    for (file, floor) in GC_FAILED_REMOVAL_PINS {
        let found = judged.iter().filter(|(p, _)| p.ends_with(file)).count();
        assert!(
            found >= *floor,
            "{file} yielded {found} pins of gc's failed-removal arm, under its \
             floor of {floor} — a member has fallen out of needle reach:\n{}",
            judged
                .iter()
                .map(|(_, at)| at.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    // A floor only guards a file it names, so the table has to be the whole
    // non-zero set: a pin landing in a fourth file is judged here and watched
    // by nothing, free to fall out of needle reach unnoticed.
    let unfloored: std::collections::BTreeSet<&str> = judged
        .iter()
        .map(|(file, _)| file.as_str())
        .filter(|file| {
            !GC_FAILED_REMOVAL_PINS
                .iter()
                .any(|(floored, _)| file.ends_with(floored))
        })
        .collect();
    assert!(
        unfloored.is_empty(),
        "a file holding a pin of gc's failed-removal arm needs a row in \
         `GC_FAILED_REMOVAL_PINS`, or its count can fall to zero unwatched:\n{}",
        unfloored.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// Every path-based chmod in the WORKSPACE says why following a symlink is safe
/// there.
///
/// The rule, the tells and the hatch grammar live in
/// [`crate::test_helpers::path_based_chmod_population`], which derives the crate
/// roots by reading `crates/` so a crate added to the workspace joins this
/// population with it.
///
/// One walk over every crate at once: the class was first swept in the
/// reconciler alone, and a chmod turned out to be as likely in the source
/// cache, the backup engine, the daemon's IPC setup, the self-upgrade, the
/// secrets backends or the device gateway. A walk reads source TEXT, so the
/// crate graph does not bound it, and a per-crate body left `cfgd-csi` (root on
/// every node), `cfgd-crd` and `cfgd-schema` judged by nobody. One population
/// also means no site is judged twice.
///
/// Each root's floor is a minimum set at what the workspace holds, so a call
/// site cannot be removed silently; an addition passes, and the assertion
/// prints the numbers it read. The floor is stated PER ROOT, because one number
/// for the whole workspace is the biggest tree's count plus the rest:
/// `crates/cfgd/src` could stop contributing entirely and `cfgd-core` alone
/// would still clear it. The roots are named as well as counted, for the reason
/// [`crate::test_helpers::KNOWN_GOLDEN_ROOTS`] is named: a count survives a
/// root renamed or moved out of `crates/` as long as some other crate appears
/// to restore it.
#[test]
fn every_path_based_chmod_in_the_workspace_says_why_the_follow_is_safe() {
    /// Every crate root the walk must still be reading, workspace-relative,
    /// with a floor under the production sources each holds today, so a tree
    /// going dark fails on its own name. Each floor is a minimum the root must
    /// keep, and a count above it passes; the two crates holding a couple of
    /// files each set theirs at that count, because a root going empty is what
    /// the assertion is for and a margin under two is no margin.
    const CHMOD_WALK_ROOTS: &[(&str, usize)] = &[
        ("crates/cfgd-core/src", 170),
        ("crates/cfgd-crd/src", 1),
        ("crates/cfgd-csi/src", 7),
        ("crates/cfgd-operator/src", 40),
        ("crates/cfgd-schema/src", 2),
        ("crates/cfgd/src", 130),
    ];
    let crates_dir = crate::test_helpers::workspace_root().join("crates");
    let population = crate::test_helpers::path_based_chmod_population(&crates_dir);
    let unread: Vec<&str> = CHMOD_WALK_ROOTS
        .iter()
        .map(|(named, _)| *named)
        .filter(|named| !population.roots.iter().any(|read| read == named))
        .collect();
    assert!(
        unread.is_empty(),
        "the walk no longer reads {unread:?}; it read {:?} — a renamed or moved \
         crate root leaves its chmods judged by nobody",
        population.roots
    );
    // A root the walk never reported on reads as zero:
    // a missing entry is the whole tree going dark, which is the state this
    // floor exists to catch.
    let short: Vec<(&str, usize, usize)> = CHMOD_WALK_ROOTS
        .iter()
        .map(|(named, floor)| {
            let read = population
                .per_root
                .iter()
                .find(|(root, ..)| root == named)
                .map_or(0, |(_, files, _)| *files);
            (*named, read, *floor)
        })
        .filter(|(_, read, floor)| read < floor)
        .collect();
    assert!(
        short.is_empty() && population.roots.len() >= 6,
        "a crate root contributed fewer production sources than it holds, so its \
         chmods are judged by nobody: {short:?} of {:?}",
        population.per_root
    );
    let chmods: usize = population.per_root.iter().map(|(.., n)| n).sum();
    assert!(
        chmods >= 31,
        "the walk read {chmods} chmods across {:?}, too few to be the population",
        population.per_root
    );
    assert!(
        population.offenders.is_empty(),
        "every path-based chmod states why it may follow a link:\n{}",
        population.offenders.join("\n")
    );
}

/// Every production start of a child process goes through the one ladder.
///
/// A child started through [`std::process::Command`] directly is one no retry
/// covers and no descriptor-limit raise precedes, and both matter for reasons
/// the call site cannot see: another thread of this process can be holding the
/// program file open (`ETXTBSY`), and concurrent spawns can crowd a soft
/// descriptor limit a host shipped at 256, which is what turned an eight-way
/// filter-script test red on macOS and nowhere else.
/// [`cfgd_core::spawn_child`](crate::spawn_child) and its two siblings
/// ([`cfgd_core::command_output`](crate::command_output),
/// [`cfgd_core::command_status`](crate::command_status)) are where both answers
/// live, so a second start path is a second policy.
///
/// `std` offers three ways to start that child and all three are judged here.
/// `status` is also the name of an HTTP response's own code, which starts
/// nothing; such a line carries [`NOT_A_CHILD`] and no spawn hatch, because
/// the two say different things about the same row.
#[test]
fn every_production_spawn_in_the_workspace_goes_through_the_one_ladder() {
    const HATCH: &str = "direct-spawn-ok:";
    /// A line matching a tell that starts no child at all: an HTTP response's
    /// `status`, which shares the method name and nothing else.
    const NOT_A_CHILD: &str = "not-a-child-ok:";
    /// The three ways a `std::process::Command` starts a child. A spawn takes
    /// no argument; every `spawn(` that does is a thread or a task, which this
    /// rule says nothing about.
    const TELLS: &[&str] = &[".spawn()", ".output()", ".status()"];
    /// The seam's own starts, which cannot route through themselves.
    const SEAM: &str = "util/process.rs";
    /// Every crate root the walk must still be reading, workspace-relative,
    /// with a floor UNDER the count each holds today, so deleting a file is
    /// free; an aggregate floor is one tree's count plus another's, which the
    /// biggest tree alone clears. Each floor is a minimum the root must keep,
    /// and a count above it passes; the three crates holding a couple of files
    /// each set theirs at that count, because a root going empty is what the
    /// assertion against `crates/` below defends.
    const SPAWN_WALK_ROOTS: &[(&str, usize)] = &[
        ("crates/cfgd-core/src", 180),
        ("crates/cfgd-crd/src", 1),
        ("crates/cfgd-csi/src", 7),
        ("crates/cfgd-operator/src", 40),
        ("crates/cfgd-schema/src", 2),
        ("crates/cfgd-test-fixtures/src", 1),
        ("crates/cfgd/src", 135),
    ];

    let root = crate::test_helpers::workspace_root();
    let mut present: Vec<String> = std::fs::read_dir(root.join("crates"))
        .expect("the workspace's crate directory is readable")
        .map(|entry| entry.expect("the walk must read every directory entry"))
        .filter(|entry| entry.path().join("src").is_dir())
        .map(|entry| format!("crates/{}/src", entry.file_name().to_string_lossy()))
        .collect();
    present.sort();
    let named: Vec<String> = SPAWN_WALK_ROOTS
        .iter()
        .map(|(named, _)| (*named).to_string())
        .collect();
    assert_eq!(
        present, named,
        "a crate joined or left the workspace, so the starts in its tree are judged by nobody"
    );

    let mut short_roots: Vec<String> = Vec::new();
    let mut seam_starts = [0usize; 3];
    let mut offenders: Vec<String> = Vec::new();
    for (named, floor) in SPAWN_WALK_ROOTS {
        let dir = root.join(named);
        let mut files = 0usize;
        for path in crate::test_helpers::rust_sources_under(&dir) {
            // A file that IS test scaffolding carries no production slice of
            // its own; the predicate is the one four sibling walks share.
            let scaffolding = crate::test_helpers::is_test_source(&path);
            if scaffolding || crate::test_helpers::is_test_only_file(&path) {
                continue;
            }
            files += 1;
            let production = crate::test_helpers::production_slice_of(&path);
            let lines = crate::test_helpers::logical_source_lines(&production);
            let is_seam = path.ends_with(SEAM);
            for (i, (n, line)) in lines.iter().enumerate() {
                let code = crate::test_helpers::code_line(line);
                let Some(tell) = TELLS.iter().position(|tell| code.contains(tell)) else {
                    continue;
                };
                if is_seam {
                    seam_starts[tell] += 1;
                    continue;
                }
                // Both markers are comments by construction, so they are read
                // off the raw rows, on the tell's own line or the one above it.
                if lines[i.saturating_sub(1)..=i]
                    .iter()
                    .any(|(_, l)| carries_hatch(l, HATCH) || carries_hatch(l, NOT_A_CHILD))
                {
                    continue;
                }
                offenders.push(format!("{}:{}: {}", path.display(), n, line.trim()));
            }
        }
        if files < *floor {
            short_roots.push(format!("{named} read {files}, under its floor of {floor}"));
        }
    }
    assert!(
        short_roots.is_empty(),
        "a root the walk reports as read contributed less than it holds, so the starts in it are \
         judged by nobody:\n{}",
        short_roots.join("\n")
    );
    assert!(
        seam_starts.iter().all(|found| *found >= 1),
        "the walk found {seam_starts:?} starts in the seam itself, one count per {TELLS:?}; a \
         zero means that tell no longer names what starting a child looks like"
    );
    assert!(
        offenders.is_empty(),
        "a child process is started through `cfgd_core::spawn_child`, `cfgd_core::command_output` \
         or `cfgd_core::command_status` (or `spawn_past_a_transient_refusal`, which also states \
         how many attempts the ladder spent), so every path gets the transient-refusal retry and \
         the descriptor-limit raise; a path that genuinely must start a child for itself carries \
         `// {HATCH} <why>`, and a row that starts no child at all carries \
         `// {NOT_A_CHILD} <why>`:\n{}",
        offenders.join("\n")
    );
}

/// A distinctness premise judges the array its fixture ASSERTS on. A second
/// set written beside that array proves nothing.
///
/// [`crate::test_helpers::assert_slots_discriminate`] can only judge the array
/// it is handed, so an array of literals typed next to an assertion that retypes
/// the same numbers guards nothing: the mis-wiring the premise exists to catch
/// edits the fixture, and the literals stay where they were. Six call sites were
/// in exactly that shape.
///
/// Two shapes are welded and both are CHECKED here, each by the thing that
/// welds it. An array BOUND to a name is welded by the assertion reading that
/// name back (`for (slot, count) in slots`, `format!` off `slots[0].1`), so the
/// name is required to appear again below the call, inside the same function
/// body and before any later `let` rebinds it: bound and then asserted against
/// retyped bytes, the binding guards nothing an array of literals does not. An
/// INLINE array is welded by every value being read off the product under test
/// (`tally.succeeded`, `class[0].1`), so every entry is parsed and a bare
/// integer is the offence, because nothing connects that integer to the bytes
/// asserted.
///
/// Both readings judge the WHOLE argument: the accumulation runs to the balanced
/// `]`, past any earlier `]` CHARACTER, since a value may hold a bracket of
/// its own, and the array's own brackets are stripped before the entries are
/// split, since a closing `]);` left on the last entry's value makes it parse as
/// no integer and reads as a product. An argument shape neither reading covers is
/// refused. `// slots-literal-ok: <why>` on the call's
/// line or the one above hatches a genuine exception.
#[test]
fn every_distinctness_premise_reads_the_values_its_fixture_asserts() {
    let mut offenders = Vec::new();
    let mut sites = 0usize;
    let mut slots = 0usize;
    let mut bound = 0usize;
    for path in workspace_rust_files() {
        let posix = crate::to_posix_string(&path);
        // The helper's own file declares it; this one quotes the call shape.
        if posix.ends_with("cfgd-core/src/test_helpers.rs")
            || posix.ends_with("cfgd-core/src/output/tests/fences.rs")
        {
            continue;
        }
        // unfloored-slice-ok: the premises judged here are tests.
        let body = walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let Some((_, after)) = line.split_once("assert_slots_discriminate(") else {
                continue;
            };
            sites += 1;
            if hatched(&lines, idx, "slots-literal-ok:") {
                continue;
            }
            let after = after.trim_start();
            let inline = after.strip_prefix('&').unwrap_or(after);
            if !inline.starts_with('[') {
                bound += 1;
                let name = inline.trim_end_matches([')', ';', ' ']);
                let indent = line.len() - line.trim_start().len();
                // The scan ends at the function's closing brace, and at a
                // REBINDING of the same name before it: a later `let slots = …`
                // answers for its own call, so counting it would let the call
                // above pass on a read-back that never reads the array it was
                // handed. A COMMENT naming the array is not a read either, so a
                // comment line is passed over.
                let rebound = format!("let {name} ");
                let rebound_mut = format!("let mut {name} ");
                let read_back = lines[idx + 1..]
                    .iter()
                    .take_while(|below| {
                        let trimmed = below.trim_start();
                        !(trimmed == "}" && below.len() - trimmed.len() < indent)
                            && !trimmed.starts_with(&rebound)
                            && !trimmed.starts_with(&rebound_mut)
                    })
                    .any(|below| !below.trim_start().starts_with("//") && below.contains(name));
                if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    offenders.push(format!(
                        "{posix}:{}: the walk cannot read `{name}` as a bound name",
                        idx + 1
                    ));
                } else if !read_back {
                    offenders.push(format!(
                        "{posix}:{}: `{name}` is never read back below the call",
                        idx + 1
                    ));
                }
                continue;
            }
            let mut arg = inline.to_string();
            let mut at = idx;
            while balanced_close(&arg).is_none() && at + 1 < lines.len() {
                at += 1;
                arg.push_str(lines[at]);
            }
            let Some(close) = balanced_close(&arg) else {
                offenders.push(format!(
                    "{posix}:{}: the walk cannot find the end of the slot array",
                    idx + 1
                ));
                continue;
            };
            for entry in arg[1..close].split("(\"").skip(1) {
                let Some((name, value)) = entry.split_once("\",") else {
                    offenders.push(format!(
                        "{posix}:{}: the walk cannot read a slot's name and value",
                        idx + 1
                    ));
                    continue;
                };
                slots += 1;
                if value
                    .trim()
                    .trim_end_matches([')', ',', ' '])
                    .parse::<usize>()
                    .is_ok()
                {
                    offenders.push(format!("{posix}:{}: `{name}`", idx + 1));
                }
            }
        }
    }
    assert!(
        sites >= 11 && slots >= 29 && bound >= 3,
        "the walk found {sites} distinctness premises, judged {slots} inline slots \
         and read back {bound} bound arrays; too few to be the population"
    );
    assert!(
        offenders.is_empty(),
        "a distinctness premise states the value the fixture asserts, read off \
         the product or bound to a name the assertion reads back:\n{}",
        offenders.join("\n")
    );
}

/// The call a walk reads an enumerated file with.
const WALK_FILE_READ: &str = "read_to_string";

/// Spellings that drop that call's failure unreported.
const SILENT_READ_TELLS: &[&str] = &["let Ok(", ".ok()", "unwrap_or_default()", "unwrap_or("];

/// A walk that cannot read a file it enumerated FAILS: reading less than its
/// floor promises would pass silently.
///
/// A walk's floor counts the population it found on disk, including members it
/// could not open, so a file whose read failed is indistinguishable from a file
/// holding nothing: no rule judges it, no offender is reported, and the walk
/// passes. [`crate::test_helpers::walked_file_body`] and
/// [`crate::test_helpers::production_slice_of`] are the two readers that refuse
/// instead, and a crate too far down the graph to reach either states the refusal
/// inline.
///
/// The population is every `.rs` source of every crate, judged over its TEST
/// region as [`crate::test_helpers::test_region_of`] reads it: a file that IS
/// scaffolding (one whose name opens on `test`, or one lying under a `tests/`
/// directory) is judged whole, and every other file over its test-only items,
/// wherever they sit, which is where the walks living in an inline test module
/// are. Six files hold such walks, `reconciler/format.rs` among them, so a
/// filename filter read a narrower population than the rule claims. A
/// scaffolding name is matched by its PREFIX because `tests.rs` is one
/// spelling of it and `tests_module.rs` is another: a
/// whole-file test module carries no anchor of its own, the parent's
/// `#[cfg(test)] mod tests_module;` being what gates it, so an exact-name test
/// dropped eleven files holding nothing but test declarations.
/// A production read whose file may legitimately be absent
/// (`/etc/os-release`, a cached credential, a target a check is asking about)
/// falls outside the REGION whatever its filename, and is never hatched one at
/// a time.
///
/// `// absent-file-ok: <why>` hatches a read inside a test region whose absence
/// is itself a legitimate state: the argv log a shim writes on its first
/// invocation, which a shim nothing ran never wrote.
#[test]
fn no_walk_silently_drops_a_file_it_enumerated() {
    let mut files = 0usize;
    let mut hatches = 0usize;
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        let posix = crate::to_posix_string(&path);
        files += 1;
        // The one scanner's test region, blanked in place: the production
        // carve-out falls out of the REGION, and a walk written in an inline
        // test module is in the population.
        let region = crate::test_helpers::test_region_of(&path);
        let lines: Vec<&str> = region.lines().collect();
        // A file holding test declarations the scanner found no test region for
        // fails here by name: `files` has already counted it, and a floor on
        // that count cannot see one file's region go missing.
        if region.trim().is_empty() {
            // unfloored-slice-ok: the whole file is searched for a test declaration.
            let body = walked_file_body(&path);
            if body.lines().any(|l| {
                let trimmed = l.trim_start();
                trimmed.starts_with("#[test]") || trimmed.starts_with("#[tokio::test")
            }) {
                offenders.push(format!(
                    "{posix}: holds test declarations the walk found no test region for"
                ));
            }
            continue;
        }
        for (idx, line) in lines.iter().enumerate() {
            if !line.contains(WALK_FILE_READ) {
                continue;
            }
            // rustfmt breaks a long read onto its own line and leaves the
            // combinator on the next one, so the tell is looked for across
            // both lines.
            let window = format!("{line}{}", lines.get(idx + 1).copied().unwrap_or_default());
            if !SILENT_READ_TELLS.iter().any(|tell| window.contains(tell)) {
                continue;
            }
            if hatched(&lines, idx, "absent-file-ok:") {
                hatches += 1;
                continue;
            }
            offenders.push(format!("{posix}:{}: {}", idx + 1, line.trim()));
        }
    }
    assert!(
        files >= 550 && hatches >= 5,
        "the walk read {files} sources and found {hatches} hatched absences, \
         too few to be the population"
    );
    assert!(
        offenders.is_empty(),
        "a walk that cannot read a file it enumerated reads less than its floor \
         promises; read it through `walked_file_body` / `production_slice_of`, or \
         say why the absence is a legitimate state with \
         `// absent-file-ok: <why>`:\n{}",
        offenders.join("\n")
    );
}

/// The words that make a substituted value a PATH, so a capture's unstable
/// spans that are not paths (a mock registry's URL, a digest, a platform
/// triple) stay outside the rule.
const SUBSTITUTED_PATH_WORDS: [&str; 7] = [
    "path",
    "dir",
    "home",
    "root",
    "display()",
    "to_str()",
    "to_string_lossy()",
];

/// The positive population: what the walk requires to still be reading tests
/// that substitute a path at all. `>=`, so a new snapshot test never trips it
/// and a wholesale loss of the helper does.
const NORMALIZER_CALL_FLOOR: usize = 50;

/// Every line holding a hand substitution: a `.replace(` whose first argument
/// names a path and whose second is an angle-bracketed label, less the ones a
/// `// hand-substitution-ok:` marker accounts for.
fn hand_substitutions(lines: &[&str]) -> Vec<usize> {
    const CALL: &str = ".replace(";
    let mut found = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let masked = code_half(line);
        // The call is located on the blanked rendering, so a `.replace(` inside
        // a string literal is prose, and read raw, because the label the pair
        // turns on is itself a literal.
        for (at, _) in masked.match_indices(CALL) {
            let call = call_argument(lines, idx, at + CALL.len());
            let Some((subject, label)) = call.split_once(',') else {
                continue;
            };
            let names_a_path = SUBSTITUTED_PATH_WORDS
                .iter()
                .any(|word| subject.contains(word));
            if !names_a_path || !label.contains("\"<") {
                continue;
            }
            if hatched(lines, idx, "hand-substitution-ok:") {
                continue;
            }
            found.push(idx);
        }
    }
    found
}

/// Every path a test substitutes for a label is substituted through
/// [`crate::normalize_for_snapshot`].
///
/// A display slot folds the home directory, so a report renders `~/…` wherever
/// its subject lies under the home — which on Windows every
/// `tempfile::tempdir()` does. A hand-written `.replace(<absolute path>,
/// "<DIR>")` then matches nothing: the capture keeps the folded path, the golden
/// keeps the label, and the test fails on that platform alone, which is how
/// three `module keys` goldens and one `doctor` row expectation came to disagree
/// with Windows. The helper substitutes BOTH spellings of every path handed to
/// it, so no verdict turns on where the host puts its temp directory.
///
/// Judged on the pair: a `.replace(` whose first argument names a path and
/// whose second is an angle-bracketed label, read to the call's balanced close
/// so a call rustfmt split over rows is judged like an inline one. The region
/// is the test one [`crate::test_helpers::test_region_of`] reads — a file under
/// a `tests/` directory whole, every other over its test-only items — because a
/// production fold substitutes a path for a marker too, and
/// `fold_home_in_text` is the one this rule is named after.
///
/// `// hand-substitution-ok: <why>` hatches a substitution the helper cannot
/// perform.
#[test]
fn every_path_a_test_substitutes_for_a_label_goes_through_the_one_normalizer() {
    let mut normalized = 0usize;
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        let posix = crate::to_posix_string(&path);
        let region = crate::test_helpers::test_region_of(&path);
        let lines: Vec<&str> = region.lines().collect();
        for line in &lines {
            // The code half of the line: a comment naming either spelling is
            // prose, and this file's own doc comment names both.
            normalized += code_half(line).matches("normalize_for_snapshot(").count();
        }
        for idx in hand_substitutions(&lines) {
            offenders.push(format!("{posix}:{}: {}", idx + 1, lines[idx].trim()));
        }
    }
    assert!(
        normalized >= NORMALIZER_CALL_FLOOR,
        "the walk read {normalized} calls to the normalizer, under its floor of \
         {NORMALIZER_CALL_FLOOR} — it has stopped reading the tests that \
         substitute a path"
    );
    assert!(
        offenders.is_empty(),
        "a path substituted by hand misses the `~/`-folded spelling a display \
         slot renders, so the expectation holds only where the temp directory \
         lies outside the home; substitute through \
         `cfgd_core::normalize_for_snapshot(captured, &[(path, \"<LABEL>\")])`, \
         or say why it cannot with `// hand-substitution-ok: <why>`:\n{}",
        offenders.join("\n")
    );
}

/// A substitution rustfmt split over rows is judged like one written inline.
///
/// `.replace(` is read to its balanced close, because rustfmt puts the label on
/// a later row as soon as the call grows, and a line-scoped read sees a call
/// with no second argument and passes it over: the walk would go blind on every
/// long substitution while reporting the short ones.
#[test]
fn a_hand_substitution_split_over_rows_is_judged_like_an_inline_one() {
    let inline = ["let s = captured.replace(dir, \"<DIR>\");"];
    let split = [
        "let s = captured.replace(",
        "    dir,",
        "    \"<DIR>\",",
        ");",
    ];
    let marked = [
        "// hand-substitution-ok: the subject is a registry name",
        "let s = captured.replace(",
        "    dir,",
        "    \"<DIR>\",",
        ");",
    ];
    assert_eq!(
        hand_substitutions(&inline),
        vec![0],
        "the inline spelling is the one the walk already reported"
    );
    assert_eq!(
        hand_substitutions(&split),
        vec![0],
        "a substitution whose label sits on a later row is judged too"
    );
    assert!(
        hand_substitutions(&marked).is_empty(),
        "a marked substitution is accounted for on either spelling"
    );
}

/// The pins whose body runs at one uid only, per suffix: floor = what the
/// workspace holds today, `>=` so an addition never trips it and a member
/// falling out of the walk's reach does.
const UID_GATED_PINS: [(&str, usize); 4] = [
    ("_as_root", 2),
    ("_as_non_root", 14),
    ("_as_linux_root", 3),
    ("_as_non_linux_root", 2),
];

/// The first statement of a function body, comments dropped and whitespace
/// collapsed, read from the line after the body's opening brace to the first
/// line that closes the statement at depth zero. A leading `use` item executes
/// nothing and is passed over, so a gate that follows the imports is still the
/// first thing the body DOES.
fn first_statement(slice: &str) -> String {
    let body = slice.split_once('{').map_or("", |(_, rest)| rest);
    let mut out = String::new();
    let mut depth = 0usize;
    let mut in_use_item = false;
    for line in body.lines() {
        let code = line.split(" //").next().unwrap_or("").trim();
        if code.is_empty() || code.starts_with("//") {
            continue;
        }
        if out.is_empty() && (in_use_item || code.starts_with("use ")) {
            in_use_item = !code.ends_with(';');
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(code);
        depth += code.matches('{').count();
        depth = depth.saturating_sub(code.matches('}').count());
        if depth == 0 && (code.ends_with(';') || code.ends_with('}')) {
            break;
        }
    }
    out
}

/// The suffix a uid gate demands of its pin's name, or `None` when the
/// statement is not an empty-bodied early return on `is_root()`.
///
/// A leading `!` names the arm that RUNS as the privileged one; a
/// `target_os = "linux"` in the condition narrows either arm to Linux, which is
/// the only platform where root owns a separate Homebrew account.
fn uid_gate_suffix(statement: &str) -> Option<&'static str> {
    let cond = statement.strip_prefix("if ")?;
    let (cond, block) = cond.split_once('{')?;
    if block.trim() != "return; }" || !cond.contains("is_root()") {
        return None;
    }
    let negated = cond.trim_start().starts_with('!');
    let linux = cond.contains("target_os = \"linux\"");
    Some(match (negated, linux) {
        (true, false) => "_as_root",
        (false, false) => "_as_non_root",
        (true, true) => "_as_linux_root",
        (false, true) => "_as_non_linux_root",
    })
}

/// Every test that returns early on `is_root()` before asserting anything names
/// the uid its body runs at, and every name claiming a uid opens on that gate.
///
/// A pin whose first statement is `if !is_root() { return; }` proves nothing
/// off root, and its green line under an unprivileged run is indistinguishable
/// from a pin that ran. The name is the only place a reader of the run can see
/// which half executed, so the suffix is mechanical: `_as_root` for the
/// privileged arm, `_as_non_root` for the unprivileged one, and the
/// `_linux_` forms when the gate also asks `cfg!(target_os = "linux")`. A pin
/// that asserts in BOTH arms (`if is_root() { assert A } else { assert B }`)
/// carries no suffix, because every run executes it. The reverse direction
/// holds too, or a suffix could outlive the gate it describes.
#[test]
fn every_pin_that_runs_at_one_uid_says_so_in_its_name() {
    let mut found: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        let relative = source_label(&path);
        // unfloored-slice-ok: the pins judged here are tests.
        let body = crate::test_helpers::walked_file_body(&path);
        let lines: Vec<&str> = body.lines().collect();
        for (open, slice) in source_functions(&relative, &body) {
            let attrs = &lines[attribute_block_start(&lines, open - 1)..open - 1];
            let is_test = attrs.iter().any(|l| {
                let t = l.trim_start();
                t.starts_with("#[test]") || t.starts_with("#[tokio::test")
            });
            if !is_test {
                continue;
            }
            let name = declared_fn_name(&slice).unwrap_or("<unnamed>");
            let at = format!("{relative}:{open}: {name}");
            let statement = first_statement(&slice);
            let gate = uid_gate_suffix(&statement);
            let claimed = UID_GATED_PINS
                .iter()
                .map(|(suffix, _)| *suffix)
                .find(|suffix| name.ends_with(suffix));
            match (gate, claimed) {
                (Some(gate), Some(claimed)) if gate == claimed => {
                    found.entry(gate).or_default().push(at);
                }
                (Some(gate), _) => offenders.push(format!(
                    "{at}: opens on `{statement}`, so its body runs only {}; the name \
                     must end `{gate}`",
                    gate.trim_start_matches("_as_").replace('_', " ")
                )),
                (None, Some(claimed)) => offenders.push(format!(
                    "{at}: is named `{claimed}` but does not open on the uid gate that \
                     suffix states (first statement: `{statement}`)"
                )),
                (None, None) => {}
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a pin that returns early on `is_root()` names the uid its body runs at, \
         and only such a pin carries that suffix:\n{}",
        offenders.join("\n")
    );
    for (suffix, floor) in UID_GATED_PINS {
        let pins = found.get(suffix).map_or(0, Vec::len);
        assert!(
            pins >= floor,
            "{pins} pins end `{suffix}`, under the floor of {floor}; a member has \
             fallen out of the walk's reach:\n{}",
            found.get(suffix).map(|v| v.join("\n")).unwrap_or_default()
        );
    }
}

/// Every production field whose type carries a declared script body, paired
/// with the production files whose parse refuses a blank one in it.
///
/// A holder reaches its validation through the file that PARSES it, which is
/// not always its own: `ProfileSpec` is refused where its layers are merged,
/// once for the solo path and once for the composed one.
const SCRIPT_BODY_HOLDERS: &[(&str, &str, &[&str])] = &[
    (
        "crates/cfgd-core/src/config/module.rs",
        "scripts",
        &["crates/cfgd-core/src/config/module.rs"],
    ),
    (
        "crates/cfgd-core/src/config/profile_spec.rs",
        "scripts",
        &[
            "crates/cfgd-core/src/config/resolve.rs",
            "crates/cfgd-core/src/composition/engine.rs",
        ],
    ),
    (
        "crates/cfgd-core/src/config/resolve.rs",
        "scripts",
        &["crates/cfgd-core/src/config/resolve.rs"],
    ),
    (
        "crates/cfgd-crd/src/lib.rs",
        "hooks",
        &["crates/cfgd-crd/src/lib.rs"],
    ),
    (
        "crates/cfgd-crd/src/lib.rs",
        "scripts",
        &["crates/cfgd-crd/src/lib.rs"],
    ),
];

/// Every type a declared script body arrives inside: the six-hook spec, and the
/// Module CRD's own scalar hook holder.
///
/// A field declaring one accepts a body cfgd will run, whatever the shape it
/// spells the body in, so both belong to the population below.
const SCRIPT_BODY_TYPES: &[&str] = &["ScriptSpec", "ModuleScripts"];

/// The field name a line declares, when the line declares a struct field whose
/// type carries a script body.
///
/// A field is the shape that DESERIALIZES one: a struct literal, a function
/// parameter and a return type all name the type without accepting YAML, so
/// only a field carrying a visibility lead and ending its own declaration
/// answers.
fn declares_a_script_body_field(line: &str) -> Option<&str> {
    let code = crate::test_helpers::code_span(line).trim();
    if !code.ends_with(',') {
        return None;
    }
    let declared = crate::test_helpers::strip_item_lead(code);
    if declared == code {
        return None;
    }
    let (name, ty) = declared.split_once(':')?;
    SCRIPT_BODY_TYPES
        .iter()
        .any(|holder| ty.contains(holder))
        .then(|| name.trim())
}

/// The call every holder's parse reaches its refusal through, named by the
/// prefix both the list form and the scalar form share.
const VALIDATE_BODY_CALL: &str = "validate_script_bod";

/// Whether `text` names `token` as a whole word, outside any longer name.
///
/// A holder field called `script` is spelled inside the call token
/// `validate_script_bod` itself, so a plain substring test lets the call vouch
/// for a field nothing passes to it.
fn names_whole_token(text: &str, token: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(token).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(ident)
            && !text[at + token.len()..].chars().next().is_some_and(ident)
    })
}

/// The line with the refusal call's own identifier cut out, so what remains is
/// only what the call site SAYS about the field it is judging.
fn without_call_name(line: &str) -> String {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let Some(at) = line.find(VALIDATE_BODY_CALL) else {
        return line.to_string();
    };
    let end = line[at..]
        .char_indices()
        .find(|(_, c)| !ident(*c))
        .map_or(line.len(), |(offset, _)| at + offset);
    format!("{} {}", &line[..at], &line[end..])
}

/// A script body a surface accepts from YAML is a body cfgd will run: a blank
/// one runs nothing and renders a row with no body in it, so every holder
/// refuses it at parse time. The Module CRD held two fields nothing checked,
/// which let a cluster admit a module the agent then refused on every machine it
/// reached, and let the pod webhook build an init container around a command
/// that was never there.
///
/// Each holder names the files that validate it, because a spec merged from
/// layers is refused where the merge lands.
/// The file declaring the field never sees it. The validating call must NAME
/// the holder's field in its subject or its argument: a file already refusing
/// one field would otherwise vouch for a second holder nothing reads.
#[test]
fn every_deserialized_script_body_is_refused_an_empty_run() {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut read = 0usize;
    for path in workspace_rust_files() {
        // This file spells the holders' declarations in its own table.
        if path.ends_with(Path::new("output/tests/fences.rs"))
            || crate::test_helpers::is_test_source(&path)
        {
            continue;
        }
        read += 1;
        let label = source_label(&path);
        for line in crate::test_helpers::production_slice_of(&path).lines() {
            let code = code_half(line);
            if let Some(field) = declares_a_script_body_field(&code) {
                found.push((label.to_string(), field.to_string()));
            }
        }
    }
    assert!(
        read >= 380,
        "the walk read {read} production files; it is looking at the wrong root"
    );
    let listed: std::collections::BTreeSet<(&str, &str)> = SCRIPT_BODY_HOLDERS
        .iter()
        .map(|(file, field, _)| (*file, *field))
        .collect();
    let unlisted: Vec<String> = found
        .iter()
        .filter(|(file, field)| !listed.contains(&(file.as_str(), field.as_str())))
        .map(|(file, field)| format!("{file}: {field}"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "a field accepting a declared script body from YAML joins `SCRIPT_BODY_HOLDERS` \
         with the file that refuses a blank one in it:\n{}",
        unlisted.join("\n")
    );
    let stale: Vec<String> = listed
        .iter()
        .filter(|row| !found.iter().any(|(f, n)| (f.as_str(), n.as_str()) == **row))
        .map(|(file, field)| format!("{file}: {field}"))
        .collect();
    assert!(
        stale.is_empty(),
        "these rows name no field any more, so the table describes code that moved:\n{}",
        stale.join("\n")
    );
    let mut unvalidated = Vec::new();
    for (file, field, validators) in SCRIPT_BODY_HOLDERS {
        for validator in *validators {
            // unfloored-slice-ok: the validator is searched for one call, wherever it stands.
            let body = walked_file_body(&workspace_root().join(validator));
            let lines: Vec<&str> = body.lines().collect();
            let names_the_field = lines.iter().enumerate().any(|(i, line)| {
                line.contains(VALIDATE_BODY_CALL)
                    && lines[i.saturating_sub(2)..(i + 3).min(lines.len())]
                        .iter()
                        .any(|near| names_whole_token(&without_call_name(near), field))
            });
            if !names_the_field {
                unvalidated.push(format!("{file}: {field}: {validator}"));
            }
        }
    }
    assert!(
        unvalidated.is_empty(),
        "the file that parses a declared script body calls `cfgd_schema::validate_script_bod*` \
         over it, naming the field in the call's subject or argument, so one surface cannot \
         accept a body another refuses:\n{}",
        unvalidated.join("\n")
    );
}

/// The field matcher reads the declaration shape. A struct literal, a parameter
/// and a return type mention a script-body type without accepting one from
/// YAML, and a private field accepts one as much as a `pub` one does only where
/// serde can reach it.
#[test]
fn the_script_body_field_matcher_reads_a_declaration_and_nothing_else() {
    for line in [
        "    pub hooks: Option<cfgd_schema::ScriptSpec>,",
        "    pub(crate) scripts: ScriptSpec,",
        "    pub scripts: Option<ModuleScripts>,",
        "    pub scripts: ModuleScripts,",
    ] {
        assert!(
            declares_a_script_body_field(line).is_some(),
            "a field declaration must be matched: {line}"
        );
    }
    for line in [
        "            scripts: crate::config::ScriptSpec::default(),",
        "        scripts: &ScriptSpec,",
        "    pub fn declared_scripts(&self) -> crate::config::ScriptSpec {",
        "            scripts: ModuleScripts::default(),",
    ] {
        assert!(
            declares_a_script_body_field(line).is_none(),
            "only a field declaration is matched: {line}"
        );
    }
    assert_eq!(
        declares_a_script_body_field("    pub hooks: Option<cfgd_schema::ScriptSpec>,"),
        Some("hooks"),
        "the matcher names the field, which is how a holder is found in the table"
    );
}

/// The per-holder tell asks what the CALL SITE says about the field, and the
/// call's own name is not part of that answer.
///
/// A holder field named `script` is spelled inside `validate_script_bod`, so a
/// substring test over the lines around the call would let the call vouch for a
/// field nothing is ever passed for. The tell reads whole tokens and cuts the
/// call's identifier out of the line first, leaving the arguments and the
/// statement that selected the field.
#[test]
fn the_validator_tell_reads_a_whole_field_token_outside_the_call_name() {
    let call = "            && let Err(e) = cfgd_schema::validate_script_body(";
    assert!(
        call.contains("script"),
        "the call token holds the name a `script` field would carry, which is the \
         confusion the tell has to survive"
    );
    assert!(
        !names_whole_token(&without_call_name(call), "script"),
        "the call's own name vouches for no field: {call}"
    );
    assert!(
        !names_whole_token("    pub scripts_spec: ScriptSpec,", "scripts"),
        "a longer identifier is a different field"
    );
    for line in [
        "        cfgd_schema::validate_script_bodies(\"profile\", &merged.scripts)",
        "        if let Some(ref post_apply) = self.scripts.post_apply",
        "                &format!(\"scripts.{}\", cfgd_schema::POST_APPLY_HOOK),",
    ] {
        assert!(
            names_whole_token(&without_call_name(line), "scripts"),
            "a call site naming the field it judges answers the tell: {line}"
        );
    }
}

/// Every file the plan-file format is spread across, and how many optional
/// fields each holds today.
///
/// The count is a per-file floor: one aggregate number stays
/// satisfied while a whole file is renamed out of the walk. Each file is also
/// floored on the readers it declares, which is what says the walk still reaches
/// the types it names in a file still holding them.
///
/// `cfgd-schema` is on the list because the serialized action graph reaches into
/// it: `PatchSpec` rides in `FileAction::{Create,Update}`, `ScriptEntry` and
/// `ScriptCommand` in `ScriptAction::Run` and `ModuleActionKind::RunScript`,
/// `EncryptionSpec` in `ResolvedFile`. An optional field of any of those refuses
/// a plan file exactly as one declared in `reconciler/types.rs` does.
///
/// `util/config_inputs.rs` is on it for the same reason: `ConfigInput` is the
/// wire form of what a derivation read, written so a plan file can carry the
/// set beside the actions it produced.
const PLAN_FORMAT_FILES: [(&str, usize); 5] = [
    ("crates/cfgd-core/src/reconciler/types.rs", 5),
    ("crates/cfgd-core/src/providers/mod.rs", 3),
    ("crates/cfgd-core/src/modules/mod.rs", 2),
    ("crates/cfgd-core/src/util/config_inputs.rs", 1),
    ("crates/cfgd-schema/src/lib.rs", 16),
];

/// Whether an attribute declares `#[serde(skip)]` — the field serde writes on no
/// wire and reads back as [`Default`].
///
/// `skip_serializing`, `skip_serializing_if` and `skip_deserializing` all open on
/// the same six letters and are different rules, so each is cut before the token
/// is looked for — the first cut takes `skip_serializing_if` with it — and the
/// attribute's string literals are blanked first so a `rename = "skipped"`
/// spells no rule at all.
fn declares_serde_skip(attribute: &str) -> bool {
    let code = blank_string_literals(attribute);
    code.contains("serde(")
        && code
            .replace("skip_serializing", "")
            .replace("skip_deserializing", "")
            .contains("skip")
}

/// Every attribute one source declares, as `(line number, whole attribute)`.
///
/// An attribute is folded back onto one string because rustfmt breaks a long
/// `#[serde(...)]` across lines, and a walk reading a physical line would then
/// see `skip_serializing_if` and `default` as two unrelated facts. Brackets are
/// counted on the line with its string literals blanked, so a bracket inside a
/// `skip_serializing_if = "..."` path closes nothing.
fn declared_attributes(body: &str) -> Vec<(usize, String)> {
    let mut attributes = Vec::new();
    let mut open: Option<(usize, String, i32)> = None;
    for (nth, line) in body.lines().enumerate() {
        let code = blank_string_literals(line);
        let depth: i32 = code
            .chars()
            .map(|c| i32::from(c == '[') - i32::from(c == ']'))
            .sum();
        match open.take() {
            Some((start, mut text, pending)) => {
                text.push(' ');
                text.push_str(line.trim());
                let pending = pending + depth;
                if pending > 0 {
                    open = Some((start, text, pending));
                } else {
                    attributes.push((start, text));
                }
            }
            None if line.trim_start().starts_with("#[") => {
                if depth > 0 {
                    open = Some((nth + 1, line.trim().to_string(), depth));
                } else {
                    attributes.push((nth + 1, line.trim().to_string()));
                }
            }
            None => {}
        }
    }
    attributes
}

/// Every optional field of the plan-file format deserializes from its ABSENCE,
/// and every skipped one says what a plan file reads back in its place.
///
/// `#[serde(skip_serializing_if = "Option::is_none")]` drops the key from the
/// wire when it is empty, and serde's derive then REFUSES a file that omits it
/// unless the field also carries `#[serde(default)]`. Dropping the skip instead
/// is not the alternative: the hash `applies.plan_hash` stores is a
/// serialization of the actions (`Plan::to_hash_string`), so a key that starts
/// being written rewrites every hash already in every state store.
///
/// `#[serde(skip)]` is the same rule's other half. Such a field reaches no wire
/// at all, so the round trip is byte-identical and value-different, and the same
/// hash argument rules out simply serializing it. It therefore carries a
/// `// plan-skip-ok:` line saying what a plan file reads back in its place and
/// what reads that value, so "cfgd can read this back" is never taken to mean
/// more than the bytes, and a fifth skipped field cannot be added by reflex.
#[test]
fn every_optional_field_of_the_plan_format_deserializes_from_its_absence() {
    const HATCH: &str = "plan-skip-ok:";
    let mut offenders = Vec::new();
    for (rel, floor) in PLAN_FORMAT_FILES {
        let body = crate::test_helpers::production_slice_of(&workspace_root().join(rel));
        let lines: Vec<&str> = body.lines().collect();
        let mut deserializes = false;
        let mut readers = 0usize;
        let mut checked = 0usize;
        for (nth, attribute) in declared_attributes(&body) {
            if attribute.starts_with("#[derive(") {
                deserializes = attribute.contains("Deserialize");
                readers += usize::from(deserializes);
            }
            if !deserializes {
                continue;
            }
            if declares_serde_skip(&attribute) {
                checked += 1;
                // The marker sits in the comment block above the attribute —
                // the run of plain `//` lines rustfmt leaves where they were
                // written.
                let hatched = lines[..nth.saturating_sub(1)]
                    .iter()
                    .rev()
                    .take_while(|above| is_plain_line_comment(above))
                    .any(|above| carries_hatch(above, HATCH));
                if !hatched {
                    offenders.push(format!(
                        "{rel}:{nth}: {attribute} — no `{HATCH}` line above it"
                    ));
                }
                continue;
            }
            if !attribute.contains("skip_serializing_if") {
                continue;
            }
            checked += 1;
            if !attribute.contains("default") {
                offenders.push(format!("{rel}:{nth}: {attribute} — no `default`"));
            }
        }
        assert!(
            readers > 0,
            "{rel}: the walk found no type that reads a plan file back; \
             the plan format no longer lives where this walk looks"
        );
        assert!(
            checked >= floor,
            "{rel}: the walk judged {checked} optional fields, below this file's floor of {floor}"
        );
    }
    assert!(
        offenders.is_empty(),
        "an optional field of the plan format either carries no `default`, so a plan file \
         omitting its key fails to read, or is skipped with nothing saying what a plan file \
         reads back in its place:\n{}",
        offenders.join("\n")
    );
}

/// The walk above reads `#[serde(skip)]` and the three rules that open on it.
///
/// `skip_serializing_if` is the rule the walk's other half judges, and both
/// `skip_serializing` and `skip_deserializing` are one-way halves of neither —
/// a substring test for `skip` would file all three under the marker rule.
#[test]
fn only_a_whole_serde_skip_reaches_the_plan_format_walks_marker_rule() {
    for (attribute, skips) in [
        ("#[serde(skip)]", true),
        ("#[serde(default, skip)]", true),
        ("#[serde(skip, default)]", true),
        (
            "#[serde(default, skip_serializing_if = \"Option::is_none\")]",
            false,
        ),
        ("#[serde(skip_serializing)]", false),
        ("#[serde(skip_deserializing)]", false),
        ("#[serde(rename = \"skipped\")]", false),
        ("#[derive(Deserialize)]", false),
    ] {
        assert_eq!(
            declares_serde_skip(attribute),
            skips,
            "the walk reads `{attribute}` as {}skipped",
            if skips { "not " } else { "" }
        );
    }
}

/// Only a plain `//` line carries the walk's `plan-skip-ok:` hatch.
///
/// The reason a skipped field gives is maintainer text, and the rustdoc block
/// above it is user text: on `PatchSpec`, a `JsonSchema` type, that block IS the
/// published schema's description. A lookup reading both would accept the reason
/// in the one place the repo forbids writing it, and would let a `///` paragraph
/// naming the marker hatch whatever sits below it.
#[test]
fn only_a_plain_line_comment_carries_the_plan_format_walks_hatch() {
    for (line, plain) in [
        ("// plan-skip-ok: reason", true),
        ("    // plan-skip-ok: reason", true),
        ("//", true),
        ("/// plan-skip-ok: reason", false),
        ("    /// plan-skip-ok: reason", false),
        ("//! plan-skip-ok: reason", false),
        ("    pub blocked_by: Option<String>,", false),
        ("", false),
    ] {
        assert_eq!(
            is_plain_line_comment(line),
            plain,
            "the hatch lookup reads `{line}` as {}a plain line comment",
            if plain { "not " } else { "" }
        );
    }
}

/// A method is reached through `.name(` beside its owner's name, or through
/// the path call `Owner::name(`; the same path call on another type reaches
/// nothing.
#[test]
fn an_associated_function_is_reached_through_its_path_call() {
    use crate::test_helpers::reaches_fn;
    assert!(reaches_fn(
        "let c = Compliance::snapshot(&p);",
        "snapshot",
        Some("Compliance")
    ));
    assert!(reaches_fn(
        "let c: Compliance = x.snapshot();",
        "snapshot",
        Some("Compliance")
    ));
    assert!(!reaches_fn(
        "let c = Other::snapshot(&p);",
        "snapshot",
        Some("Compliance")
    ));
    assert!(!reaches_fn(
        "let c = x.snapshot();",
        "snapshot",
        Some("Compliance")
    ));
    assert!(!reaches_fn(
        "let t = InlineTable::new();",
        "new",
        Some("Table")
    ));
    assert!(!reaches_fn(
        "let t: InlineTable = x.new();",
        "new",
        Some("Table")
    ));
    let owned = |ty: &str| Some(ty.to_string());
    let declarations = vec![
        (
            "path".to_string(),
            owned("Compliance"),
            "fn path() { Self::snapshot(&p) }".to_string(),
        ),
        (
            "method".to_string(),
            owned("Compliance"),
            "fn method(&self) { self.snapshot() }".to_string(),
        ),
        (
            "elsewhere".to_string(),
            owned("Other"),
            "fn elsewhere() { Self::snapshot(&p) }".to_string(),
        ),
    ];
    let derived = crate::test_helpers::callers_reaching(
        &declarations,
        &[("snapshot".to_string(), owned("Compliance"))],
    );
    assert!(derived.contains(&("path".to_string(), owned("Compliance"))));
    assert!(derived.contains(&("method".to_string(), owned("Compliance"))));
    assert!(!derived.contains(&("elsewhere".to_string(), owned("Other"))));
}

/// A hatch is a marker in a comment: one a string literal holds is data, and a
/// trailing comment after a literal still hatches its line.
#[test]
fn a_marker_inside_a_string_literal_is_no_hatch() {
    const MARKER: &str = "// host-manager-ok:";
    for (line, hatch) in [
        ("    let s = \"// host-manager-ok: x\";", false),
        ("    let s = r#\"// host-manager-ok: x\"#;", false),
        ("    let s = \"a\"; // host-manager-ok: planted", true),
        ("    // host-manager-ok: planted", true),
        ("    /// // host-manager-ok: quoted in rustdoc", false),
    ] {
        assert_eq!(
            carries_hatch(line, MARKER),
            hatch,
            "`{line}` reads as {}a hatch",
            if hatch { "not " } else { "" }
        );
    }
}

/// A wrapped attribute reaches the walk above as one attribute.
///
/// rustfmt breaks a `#[serde(...)]` that outgrows the line, and the two facts
/// the walk pairs — the skip and the `default` that makes its key optional —
/// then sit on different physical lines. The shape below is the one rustfmt
/// already writes in `cfgd-schema`.
#[test]
fn a_wrapped_attribute_is_folded_before_the_plan_format_walk_reads_it() {
    let source = "\
#[derive(Deserialize)]
struct Wire {
    #[serde(
        default,
        skip_serializing_if = \"Option::is_none\",
        rename = \"idleTimeout\"
    )]
    idle_timeout: Option<String>,
}
";
    let folded = declared_attributes(source);
    let serde_attr = folded
        .iter()
        .find(|(_, text)| text.contains("skip_serializing_if"))
        .expect("the wrapped attribute is one of the attributes the walk reads");
    assert_eq!(
        serde_attr.0, 3,
        "the attribute is reported at the line it opens on"
    );
    assert!(
        serde_attr.1.contains("default"),
        "both halves reach the walk on one string: {}",
        serde_attr.1
    );
    assert_eq!(
        folded.len(),
        2,
        "the derive and the wrapped attribute, and no fragment of either: {folded:?}"
    );
}

/// The marker that exempts one literal from the fence below.
const SPACE_RUN_HATCH: &str = "space-run-ok:";

/// No string literal carries a mid-sentence run of three or more spaces.
///
/// A hand-wrapped assertion or panic message joined back onto one line keeps
/// the indentation its continuation row carried, and the run prints verbatim
/// the moment the assertion fires: `…so no caller              treats the
/// manager as provisionable`. `cargo fmt` never touches a literal body, so
/// nothing else catches it.
///
/// The run has to sit between two letters and be three or more spaces wide,
/// because both narrower readings name something this workspace prints on
/// purpose: two spaces is the kv renderer's own column separator, and a pad
/// after a value (`Repository      : extra`) is a fixture reproducing another
/// tool's columns. A column whose left cell is a WORD still reads as a
/// sentence here, so those carry `// space-run-ok: <why>` on the line or the
/// line above.
///
/// A literal spanning source rows is read as the one literal it is
/// ([`crate::test_helpers::folded_literal_lines`]), reported at the row it
/// opened on and hatched there or on the row above; a line-based read matched
/// no quote pair below that row and skipped the whole body, which is where
/// every `\`-continued column fixture sat. The break itself still ends a
/// line, so the indentation FOLLOWING one is the next line's indent by the
/// same rule the escaped `\n` takes below: every literal in this workspace
/// that spans rows is a document (a table definition, a captured listing, an
/// embedded YAML body), and reading its rows as one sentence would make each
/// of them a run.
#[test]
fn no_string_literal_carries_a_mid_sentence_space_run() {
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    for path in workspace_rust_files() {
        scanned += 1;
        // unfloored-slice-ok: a literal anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        let raw: Vec<&str> = body.lines().collect();
        for (n, line) in folded_literal_lines(&body) {
            let trimmed = line.trim_start();
            if is_plain_line_comment(&line)
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!")
            {
                continue;
            }
            if carries_hatch(&line, SPACE_RUN_HATCH)
                || (n > 1 && carries_hatch(raw[n - 2], SPACE_RUN_HATCH))
            {
                continue;
            }
            // The blanked line keeps every quote at its own byte offset, so a
            // span found there indexes the raw line exactly.
            let code = crate::test_helpers::code_line(&line);
            let bytes = code.as_bytes();
            let mut at = 0usize;
            while let Some(open) = bytes[at..].iter().position(|b| *b == b'"').map(|p| p + at) {
                let Some(close) = bytes[open + 1..]
                    .iter()
                    .position(|b| *b == b'"')
                    .map(|p| p + open + 1)
                else {
                    break;
                };
                if let Some(literal) = line.get(open + 1..close)
                    && let Some(run) = mid_sentence_space_run(literal)
                {
                    offenders.push(format!("{}:{n}: {run}", path.display()));
                }
                at = close + 1;
            }
        }
    }
    assert!(
        scanned >= 500,
        "the walk read {scanned} sources; it has stopped seeing them"
    );
    assert!(
        offenders.is_empty(),
        concat!(
            "a wrapped message keeps the indentation of the row it was wrapped onto; join it ",
            "with `concat!` or hatch the padding with `// space-run-ok: <why>`:\n{}"
        ),
        offenders.join("\n")
    );
}

/// The offending fragment of a literal body, or `None` when its spacing is a
/// leading indent, a trailing pad or a column; only a break inside a sentence
/// is an offence.
///
/// A run following an escaped line break is the next line's INDENT, and an
/// embedded YAML fixture is nothing but those, so the two bytes `\n` end a
/// line here exactly as a real one would.
fn mid_sentence_space_run(literal: &str) -> Option<String> {
    let bytes = literal.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] != b' ' {
            at += 1;
            continue;
        }
        let end = at + bytes[at..].iter().take_while(|b| **b == b' ').count();
        // The escape's own letter would otherwise read as the word a sentence
        // broke after, and an embedded YAML fixture is nothing but those.
        let after_break =
            at >= 2 && bytes[at - 2] == b'\\' && matches!(bytes[at - 1], b'n' | b'r' | b't');
        if end - at >= 3
            && at > 0
            && !after_break
            && end < bytes.len()
            && bytes[at - 1].is_ascii_alphabetic()
            && bytes[end].is_ascii_alphabetic()
            && literal.is_char_boundary(at)
            && literal.is_char_boundary(end)
        {
            let from = literal[..at]
                .char_indices()
                .rev()
                .nth(24)
                .map_or(0, |(i, _)| i);
            let to = literal[end..]
                .char_indices()
                .nth(24)
                .map_or(literal.len(), |(i, _)| end + i);
            return Some(literal[from..to].to_string());
        }
        at = end;
    }
    None
}

/// The crate roots the floor-sentence walk reads, workspace-relative, each
/// with a floor under the production sources it holds today, so a tree going
/// dark fails on its own name.
const FLOOR_SENTENCE_ROOTS: &[(&str, usize)] = &[
    ("crates/cfgd-core/src", 150),
    ("crates/cfgd-crd/src", 1),
    ("crates/cfgd-csi/src", 5),
    ("crates/cfgd-operator/src", 30),
    ("crates/cfgd-schema/src", 2),
    ("crates/cfgd/src", 100),
];

/// The hatch for a line whose `offers` sentence is about something other than
/// a version short of a declared floor.
const FLOOR_SENTENCE_HATCH: &str = "floor-sentence-ok:";

/// Which floor-sentence span one source line composes, if any.
///
/// `below the declared minVersion` is the tail every such sentence ends on and
/// belongs to nobody but the constant. ` offers ` is the offer clause's own
/// verb, read as the bare literal: a respell that hard-codes its operands
/// (`"apt offers cargo 1.75, too old"`) carries no interpolation to look for,
/// and it is exactly the sentence the rule exists to refuse. An `offers` about
/// something else is ordinary English and says so with the hatch.
fn composes_a_floor_sentence(line: &str) -> Option<&'static str> {
    let trimmed = line.trim_start();
    if is_plain_line_comment(line) || trimmed.starts_with("///") || trimmed.starts_with("//!") {
        return None;
    }
    let code = crate::test_helpers::code_span(line);
    if code.contains(crate::modules::FloorBootstrap::BELOW_DECLARED_FLOOR) {
        return Some(crate::modules::FloorBootstrap::BELOW_DECLARED_FLOOR);
    }
    code.contains(" offers ").then_some(" offers ")
}

/// One wording for one shortfall, held across both crates.
///
/// [`crate::modules::FloorBootstrap::offer_clause`] states what a host offers
/// and how far short it falls, and
/// [`crate::modules::FloorBootstrap::BELOW_DECLARED_FLOOR`] is the tail it
/// shares with the failure an install settles with, so a second sentence about
/// the same shortfall can be composed only by spelling one of them out. The
/// pin behind the clause holds its TEXT; this holds the population, which is
/// what "nothing else composes that sentence" actually claims.
///
/// `modules/resolve.rs` declares both and is the one exemption. A line whose
/// `offers` is about something else carries `// floor-sentence-ok: <why>`, on
/// itself or on the line above; a literal spanning rows is judged on the row it
/// opens on, which is the row a hatch above it covers.
#[test]
fn no_production_site_outside_the_resolver_composes_a_floor_shortfall_sentence() {
    let declarations = Path::new("modules").join("resolve.rs");
    let mut offenders = Vec::new();
    let mut per_root: Vec<(&str, usize)> =
        FLOOR_SENTENCE_ROOTS.iter().map(|(r, _)| (*r, 0)).collect();
    for path in workspace_rust_files() {
        let is_test_source = crate::test_helpers::is_test_source(&path);
        if path.ends_with(&declarations) || is_test_source {
            continue;
        }
        let relative = crate::to_posix_string(path.strip_prefix(workspace_root()).unwrap_or(&path));
        if let Some((_, read)) = per_root
            .iter_mut()
            .find(|(root, _)| relative.starts_with(root))
        {
            *read += 1;
        }
        let production = crate::test_helpers::production_slice_of(&path);
        let rows: Vec<&str> = production.lines().collect();
        // A literal spanning rows is read as the one line it opens on: a row
        // in the middle of one can carry no comment, so a hatch would be
        // unreachable there, and a clause wrapped over two rows would be
        // unreadable to the matcher.
        for (n, line) in folded_literal_lines(&production) {
            if carries_hatch(&line, FLOOR_SENTENCE_HATCH)
                || (n > 1 && carries_hatch(rows[n - 2], FLOOR_SENTENCE_HATCH))
            {
                continue;
            }
            if let Some(tell) = composes_a_floor_sentence(&line) {
                offenders.push(format!(
                    "{}:{n}: composes `{tell}` — {}",
                    path.display(),
                    line.trim()
                ));
            }
        }
    }
    let short: Vec<&(&str, usize)> = per_root
        .iter()
        .zip(FLOOR_SENTENCE_ROOTS)
        .filter(|((_, read), (_, floor))| read < floor)
        .map(|(read, _)| read)
        .collect();
    assert!(
        short.is_empty(),
        "a crate root contributed fewer production sources than it holds, so its \
         sentences are judged by nobody: {short:?} of {FLOOR_SENTENCE_ROOTS:?}"
    );
    assert!(
        offenders.is_empty(),
        "read `FloorBootstrap::offer_clause()` for the whole clause, or compose \
         from `FloorBootstrap::BELOW_DECLARED_FLOOR` for a sentence that only \
         shares its tail:\n{}",
        offenders.join("\n")
    );
}

/// The fields of a `Provision` node that belong to the manager LEADING it:
/// the route that manager's own module declared, and the floor its own
/// confirmation asked for. Written onto a node another manager now leads, each
/// makes the run act on one manager's facts in another's name.
const LEADER_SCOPED_PROVISION_FIELDS: &[&str] = &["*manager =", "*declared =", "*floor ="];

/// Whether `code` ASSIGNS through one of those tells; a comparison through one
/// answers false. A match arm reading `if *manager == route.package` carries
/// the assignment's own bytes as a prefix, and a reader that stops at the first
/// `=` calls a guard a re-lead.
fn assigns_leader_scoped_field(code: &str, field: &str) -> bool {
    code.match_indices(field)
        .any(|(at, _)| !code[at + field.len()..].starts_with('='))
}

/// Every production site that hands a provision node to a different leader
/// goes through [`crate::reconciler::ManagerAction::provision_led_by`].
///
/// Two of them exist: the prune that drops a batch member nothing installs any
/// more, and the `--phase`/`--skip`/`--only` rebuild. Both used to overwrite
/// `manager` in place and let every other field ride a `..`, which handed npm's
/// declared route and npm's confirmed floor to pipx: the run then installed
/// `nodejs` in pipx's name and checked what it delivered against a version
/// nobody asked of pipx.
///
/// The helper destructures exhaustively, so a field added to the variant stops
/// the build until it is classified as the leader's or the node's. This holds
/// the other half: that a third site cannot re-lead a node without reaching it.
#[test]
fn every_production_site_re_leading_a_provision_goes_through_the_one_helper() {
    let helper = Path::new("reconciler").join("types.rs");
    let workspace =
        crate::test_helpers::workspace_declarations(crate::test_helpers::WORKSPACE_CRATES);
    let mut offenders = Vec::new();
    let mut read: std::collections::BTreeSet<&Path> = std::collections::BTreeSet::new();
    let mut callers = 0usize;
    // A file's rows are contiguous, so the file-wide question is asked once
    // when its first row arrives and carried over the rest.
    let mut judged: Option<(&Path, bool)> = None;
    for (at, (&(_, path, production), (name, owner, _))) in
        workspace.sites.iter().zip(&workspace.rows).enumerate()
    {
        let wanted = match judged {
            Some((file, wanted)) if file == path => wanted,
            _ => {
                let wanted =
                    !path.ends_with(&helper) && production.contains("ManagerAction::Provision");
                judged = Some((path, wanted));
                wanted
            }
        };
        if !wanted {
            continue;
        }
        read.insert(path);
        let code = workspace.code_of(at);
        if crate::test_helpers::calls_free_fn(&code, "provision_led_by")
            || code.contains(".provision_led_by(")
        {
            callers += 1;
            continue;
        }
        for field in LEADER_SCOPED_PROVISION_FIELDS {
            if assigns_leader_scoped_field(&code, field) {
                offenders.push(format!(
                    "{}: {owner}{name} writes `{field}` itself",
                    path.display(),
                    owner = owner.as_deref().map_or(String::new(), |o| format!("{o}::"))
                ));
            }
        }
    }
    let read = read.len();
    assert!(
        read >= 6,
        "fewer files mention a provision node than the workspace holds, so the \
         walk judged a population smaller than it claims: {read}"
    );
    assert!(
        offenders.is_empty(),
        "call `ManagerAction::provision_led_by(leader, batched)`, which settles \
         every manager-scoped field for the new leader:\n{}",
        offenders.join("\n")
    );
    assert!(
        callers >= 3,
        "the two narrowing sites and the CLI rebuild reach the helper; \
         {callers} declarations call it"
    );
}

/// The floor-sentence matcher reads a composed sentence and nothing else.
///
/// One case per placement the tells can take: inside a literal, inside a line
/// comment, after a statement that ends in one, and on the row a multi-row
/// literal closes on. Each was written from the matcher's own tells before the
/// walk above was believed.
#[test]
fn the_floor_sentence_matcher_reads_a_literal_and_not_a_comment() {
    assert_eq!(
        composes_a_floor_sentence(
            r#"    let s = format!("{mgr} offers {pkg} {found}, below the declared minVersion {floor}");"#
        ),
        Some("below the declared minVersion"),
        "a composed sentence in a literal is the whole point of the walk"
    );
    assert_eq!(
        composes_a_floor_sentence("    // the tail reads below the declared minVersion <floor>"),
        None,
        "a comment explains the rule and breaks nothing"
    );
    assert_eq!(
        composes_a_floor_sentence("    let n = 1; // {mgr} offers {pkg} is the clause"),
        None,
        "a trailing comment is cut before the line is judged"
    );
    assert_eq!(
        composes_a_floor_sentence(r#"        "{} offers {} {}","#),
        Some(" offers "),
        "the row a multi-row literal opens its clause on is read like any other"
    );
    assert_eq!(
        composes_a_floor_sentence(r#"             below the declared minVersion {floor}","#),
        Some("below the declared minVersion"),
        "the row a multi-row literal closes on is read like any other"
    );
    assert_eq!(
        composes_a_floor_sentence(r#"    let s = "apt offers cargo 1.75, too old";"#),
        Some(" offers "),
        "a respell that hard-codes its operands and rewords the tail is still the clause"
    );
    assert_eq!(
        composes_a_floor_sentence(r#"    let s = "the form offers a sample of every kind";"#),
        Some(" offers "),
        "the bare verb is the tell; an `offers` about something else takes the hatch"
    );
}

/// Where test code builds a child with a raw `Command::new(`, the function
/// doing it holds the `PATH` gate.
///
/// A raw spawn resolves its program against the process-global `PATH` at
/// `spawn()`, and every test that empties or shims `PATH` holds the write
/// half of `test_helpers::PATH_ENV_LOCK` for its window. A raw spawn outside
/// the gate runs inside a sibling's window whenever the two overlap, and the
/// child it meant to start is either not found or is the sibling's shim; the
/// `git` spawn in `cmd_workflow_generate_with_git_repo` failed that way. So
/// the function enclosing each raw `Command::new(` in a test region binds
/// `path_env_read_guard()` (or `path_env_mutation_guard()` where it also
/// holds a `PATH` or tool-seam window), or reaches git through `git_cmd_local()`, which resolves
/// the program under the read guard and is no raw spawn. `std::process::`,
/// `process::` and `tokio::process::` spellings all end in the one tell.
///
/// The test regions are the partition the other test-text walks read: a
/// scaffolding file (`tests.rs`, anything under a `tests` directory, a file
/// `is_test_only_file` names) whole, and the `#[cfg(test)]` items of every other
/// file; `every_production_spawn_in_the_workspace_goes_through_the_one_ladder`
/// judges the rest.
///
/// `// raw-spawn-ok: <why>` on the line or the line above exempts a site no
/// `PATH` window can reach: a spawn under a `PATH` the test pins itself, or a
/// builder whose every caller spawns through a seam that takes the read
/// guard. The count of those is held at a ceiling, and each file's judged
/// spawns at a floor, so neither side drifts unseen.
#[test]
fn every_raw_spawn_in_test_code_holds_the_path_gate() {
    const HATCH: &str = "raw-spawn-ok:";
    const HATCH_CEILING: usize = 2;
    const FLOORS: [(&str, usize); 15] = [
        ("crates/cfgd/src/cli/init/tests.rs", 3),
        ("crates/cfgd/src/cli/tests.rs", 3),
        ("crates/cfgd/src/cli/upgrade.rs", 1),
        ("crates/cfgd/src/packages/shared/tests.rs", 30),
        ("crates/cfgd/src/system/gpg_keys/tests.rs", 2),
        ("crates/cfgd/src/system/tests.rs", 5),
        ("crates/cfgd-core/src/daemon/health_ipc.rs", 1),
        ("crates/cfgd-core/src/oci/sign/tests.rs", 9),
        ("crates/cfgd-core/src/output/printer.rs", 1),
        ("crates/cfgd-core/src/output/process.rs", 2),
        ("crates/cfgd-core/src/output/section_guard.rs", 2),
        ("crates/cfgd-core/src/test_helpers.rs", 3),
        ("crates/cfgd-core/src/util/process.rs", 13),
        ("crates/cfgd-core/tests/path_layer_order.rs", 1),
        ("crates/cfgd-operator/src/gateway/api/tests.rs", 13),
    ];

    for (form, code) in [
        ("bare", "fn t() {\n    let c = Command::new(\"git\");\n}\n"),
        (
            "process-qualified",
            "fn t() {\n    let c = process::Command::new(\"git\");\n}\n",
        ),
        (
            "std-qualified",
            "fn t() {\n    let c = std::process::Command::new(\"git\");\n}\n",
        ),
        (
            "dropped guard",
            "fn t() {\n    let _ = path_env_read_guard();\n    let c = Command::new(\"git\");\n}\n",
        ),
        (
            "guard in a sibling",
            "fn g() {\n    let _p = path_env_read_guard();\n}\nfn t() {\n    Command::new(\"git\");\n}\n",
        ),
        (
            "guard after the spawn",
            "fn t() {\n    Command::new(\"git\");\n    let _p = path_env_read_guard();\n}\n",
        ),
        (
            "guard after the spawn in a later function",
            "fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn t() {\n    Command::new(\"git\");\n    let _p = path_env_read_guard();\n}\n",
        ),
        (
            "guard in a closed block",
            "fn t() {\n    {\n        let _p = path_env_read_guard();\n    }\n    Command::new(\"git\");\n}\n",
        ),
    ] {
        let (unguarded, _, _) = raw_spawns(&FIXTURE_SOURCE, code, HATCH);
        assert_eq!(
            unguarded.len(),
            1,
            "the walk misses the {form} spawn in `{code}`"
        );
    }
    for (form, code) in [
        (
            "read guard",
            "fn t() {\n    let _p = crate::test_helpers::path_env_read_guard();\n    Command::new(\"git\");\n}\n",
        ),
        (
            "wrapped write guard",
            "fn t() {\n    let _p =\n        path_env_mutation_guard();\n    Command::new(\"sh\");\n}\n",
        ),
        (
            "literal",
            "fn t() {\n    let s = \"Command::new(\\\"git\\\")\";\n}\n",
        ),
        ("longer name", "fn t() {\n    MyCommand::new(\"x\");\n}\n"),
        (
            "spawn in a nested block",
            "fn t() {\n    let _p = path_env_read_guard();\n    for _ in 0..2 {\n        if true {\n            Command::new(\"git\");\n        }\n    }\n}\n",
        ),
        (
            "brace in a literal or comment between",
            "fn t() {\n    let _p = path_env_read_guard();\n    let s = \"}\";\n    let c = '}';\n    // }\n    Command::new(\"git\");\n}\n",
        ),
        (
            "same line",
            "fn t() {\n    let _p = path_env_read_guard(); Command::new(\"git\");\n}\n",
        ),
    ] {
        let (unguarded, _, _) = raw_spawns(&FIXTURE_SOURCE, code, HATCH);
        assert!(
            unguarded.is_empty(),
            "the walk flags the {form} shape `{code}`: {unguarded:?}"
        );
    }
    let hatched =
        "fn t() {\n    // raw-spawn-ok: PATH is the test's own\n    Command::new(\"x\");\n}\n";
    assert_eq!(raw_spawns(&FIXTURE_SOURCE, hatched, HATCH), (vec![], 1, 1));

    let mut offenders = Vec::new();
    let mut hatches = Vec::new();
    let mut per_file: Vec<(String, usize)> = Vec::new();
    for path in workspace_rust_files() {
        let label = source_label(&path);
        let region = crate::test_helpers::test_region_of(&path);
        let (unguarded, sites, hatched) = raw_spawns(&label, &region, HATCH);
        offenders.extend(unguarded);
        if hatched > 0 {
            hatches.push((label.to_string(), hatched));
        }
        if sites > 0 {
            per_file.push((label.to_string(), sites));
        }
    }
    assert!(
        offenders.is_empty(),
        "a test spawns through a raw `Command::new(` outside the PATH gate, so a \
         sibling test's PATH window can decide what it starts; bind \
         `cfgd_core::test_helpers::path_env_read_guard()` in the function first (or \
         `path_env_mutation_guard()` if it also changes PATH), reach git through \
         `git_cmd_local()`, or say why the PATH is the test's own with \
         `// raw-spawn-ok: <why>`:\n{}",
        offenders.join("\n")
    );
    let hatch_total: usize = hatches.iter().map(|(_, n)| n).sum();
    assert!(
        hatch_total <= HATCH_CEILING,
        "{hatch_total} raw spawns carry `{HATCH}`, over the ceiling of {HATCH_CEILING}: {hatches:?}"
    );
    for (file, floor) in FLOORS {
        let sites = per_file
            .iter()
            .find(|(f, _)| f == file)
            .map_or(0, |(_, n)| *n);
        assert!(
            sites >= floor,
            "{file} holds {sites} raw spawns in its test region, under its floor of {floor}; \
             a walk reading fewer than it did is going blind"
        );
    }
}

/// The raw `Command::new(` sites in `region` whose enclosing function holds no
/// `PATH` guard and carries no hatch, with the count of every site and of the
/// hatched ones.
fn raw_spawns(source: &SourceLabel, region: &str, hatch: &str) -> (Vec<String>, usize, usize) {
    const TELL: &str = "Command::new(";
    const GUARDS: [&str; 2] = ["path_env_read_guard()", "path_env_mutation_guard()"];
    let code = crate::test_helpers::blank_non_code(region);
    let lines: Vec<&str> = region.lines().collect();
    let functions = source_functions(source, region);
    // Line starts of `code`, so a function's opening LINE (1-based, from
    // `source_functions`) resolves to the byte offset a spawn's own `at` is
    // comparable against: a guard bound must sit BEFORE the spawn it protects.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let binds_a_guard = |body: &str, site_offset: usize| {
        let body = crate::test_helpers::blank_non_code(body);
        GUARDS.iter().any(|guard| {
            body.match_indices(guard).any(|(at, _)| {
                if at >= site_offset {
                    return false;
                }
                let statement = body[..at].rsplit([';', '{', '}']).next().unwrap_or("");
                let named_let = statement
                    .trim_start()
                    .strip_prefix("let ")
                    .and_then(|rest| rest.split_once('='))
                    .is_some_and(|(name, _)| !matches!(name.trim(), "_" | ""));
                if !named_let {
                    return false;
                }
                // A guard dropped at the end of its own block is not held at the
                // spawn, so a brace opened after the binding must still be open
                // when the spawn is reached.
                let mut depth = 0i32;
                body[at..site_offset].chars().all(|c| {
                    depth += match c {
                        '{' => 1,
                        '}' => -1,
                        _ => 0,
                    };
                    depth >= 0
                })
            })
        })
    };
    let mut unguarded = Vec::new();
    let (mut sites, mut hatched_sites) = (0, 0);
    for (at, _) in code.match_indices(TELL) {
        if code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        sites += 1;
        let row = code[..at].matches('\n').count();
        if hatched(&lines, row, hatch) {
            hatched_sites += 1;
            continue;
        }
        let enclosing = functions
            .iter()
            .filter(|(open, body)| (open - 1..open - 1 + body.lines().count()).contains(&row))
            .max_by_key(|(open, _)| *open);
        let guarded = enclosing.is_some_and(|(open, body)| {
            let func_start = line_starts[open - 1];
            binds_a_guard(body, at - func_start)
        });
        if !guarded {
            unguarded.push(format!("{source}:{}: {}", row + 1, lines[row].trim()));
        }
    }
    (unguarded, sites, hatched_sites)
}

/// `is_test_source` names each file shape that holds tests alone and nothing
/// that merely sits beside them.
#[test]
fn is_test_source_names_every_test_only_file_shape_and_nothing_else() {
    for held in [
        "crates/cfgd/src/cli/tests.rs",
        "crates/cfgd/src/system/tests_snapshot_bridge.rs",
        "crates/cfgd-operator/src/gateway/api/tests_foo.rs",
        "crates/cfgd-core/src/output/tests/fences.rs",
        "crates/cfgd/tests/apply_plan_file.rs",
    ] {
        assert!(
            crate::test_helpers::is_test_source(Path::new(held)),
            "{held} holds tests alone"
        );
    }
    for production in [
        "crates/cfgd/src/cli/status.rs",
        "crates/cfgd-core/src/test_helpers.rs",
        "crates/cfgd/src/cli/test_support.rs",
        "crates/cfgd/src/cli/foo.rs",
        "crates/cfgd/src/cli/tests_notes.md",
    ] {
        assert!(
            !crate::test_helpers::is_test_source(Path::new(production)),
            "{production} is not a test-only source by its name"
        );
    }
}

/// The workspace's files built only for tests, gated from outside themselves,
/// are derived from the declarations and manifests; a production path sharing
/// a helper's file name is not one, and a walk's `..` path still is.
#[test]
fn is_test_only_file_names_the_workspace_files_built_only_for_tests() {
    let derived = crate::test_helpers::test_only_files_below(&workspace_root());
    let derived: Vec<String> = derived.iter().map(crate::to_posix_string).collect();
    assert_eq!(
        derived,
        [
            "crates/cfgd/src/cli/test_support.rs",
            "crates/cfgd-core/src/bin/fake_cosign.rs",
            "crates/cfgd-core/src/output/test_capture.rs",
            "crates/cfgd-core/src/test_helpers.rs",
            "crates/cfgd-operator/src/controllers/test_fixtures.rs",
            "crates/cfgd-operator/src/controllers/test_kube_harness.rs",
            "crates/cfgd-operator/src/gateway/test_state.rs",
            "crates/cfgd-operator/src/test_helpers.rs",
            "crates/cfgd-operator/src/webhook/test_router.rs",
        ],
        "the files the workspace builds only for tests"
    );
    let root = workspace_root();
    let is = crate::test_helpers::is_test_only_file;
    assert!(is(&root.join("crates/cfgd-core/src/test_helpers.rs")));
    assert!(is(
        &root.join("crates/cfgd/../cfgd-core/src/bin/fake_cosign.rs")
    ));
    assert!(!is(&root.join("crates/cfgd-csi/src/test_helpers.rs")));
    assert!(!is(&root.join("crates/cfgd/src/bin/fake_cosign.rs")));

    // Neither kind of test-only file has a production region to judge.
    let slice = crate::test_helpers::production_slice_of;
    for held in [
        "crates/cfgd-core/src/test_helpers.rs",
        "crates/cfgd-core/src/output/tests/fences.rs",
    ] {
        assert_eq!(slice(&root.join(held)), "", "{held} slices to nothing");
    }
    assert!(!slice(&root.join("crates/cfgd-core/src/lib.rs")).is_empty());
}

/// The shell scans (`.claude/scripts/audit.sh` and the review lenses) cannot
/// ask [`crate::test_helpers::is_test_only_file`], so they read its set from a
/// checked-in list, one workspace-relative posix path per line in sorted
/// order. The list goes stale the moment the derivation moves, and this pin is
/// what says so. `task test-only-files:bless` rewrites it.
#[test]
fn test_only_files_list_matches_the_derivation() {
    let list = workspace_root().join(".claude/scripts/test-only-files.txt");
    let mut derived: Vec<String> = crate::test_helpers::test_only_files_below(&workspace_root())
        .iter()
        .map(crate::to_posix_string)
        .collect();
    derived.sort();
    let current: String = derived.iter().map(|p| format!("{p}\n")).collect();
    if std::env::var("CFGD_BLESS_TEST_ONLY_FILES").is_ok() {
        std::fs::write(&list, &current)
            .unwrap_or_else(|e| panic!("{}: cannot write the list: {e}", list.display()));
        return;
    }
    let committed = std::fs::read_to_string(&list).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run `task test-only-files:bless` to write it",
            list.display()
        )
    });
    assert_eq!(
        committed,
        current,
        "{} is stale against the derived test-only files; run `task test-only-files:bless`",
        list.display()
    );
}

/// Each way a file comes to be built only for tests is read: a declaration
/// under each test gate spelling in a crate root, in a `mod.rs`, and in a plain
/// module file (whose children live in the directory named after it), one
/// moved by `#[path]`, a file gated by its own inner attribute, a module a
/// test-only file declares, and a `[[bin]]` requiring `test-helpers` with and
/// without a `path`. An ungated declaration, a gate some shipped build meets,
/// and a file `is_test_source` already names are left out.
#[test]
fn test_only_files_are_derived_from_gated_declarations_and_manifests() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = tmp.path();
    let files = [
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\n\n[[bin]]\nname = \"fixture\"\npath = \"src/bin/fixture.rs\"\n\
             required-features = [\"test-helpers\"]\n\n[[bin]]\nname = \"real\"\npath = \"src/bin/real.rs\"\n\
             \n[[bin]]\nname = \"implied\"\nrequired-features = [\"test-helpers\"]\n",
        ),
        (
            "crates/a/src/lib.rs",
            "#[cfg(test)]\nmod helper;\n#[cfg(any(test, feature = \"test-helpers\"))]\npub mod shared;\n\
             pub mod open;\n#[cfg(unix)]\nmod unix_only;\n#[cfg(test)]\nmod tests;\npub mod nested;\n\
             #[cfg(feature = \"test-helpers\")]\npub mod feature_only;\n#[cfg(all(test, unix))]\n\
             mod all_test;\n\
             #[cfg(all(unix, feature = \"test-helpers\"))]\nmod all_feature;\n\
             #[cfg(any(windows, test))]\nmod any_shipped;\n#[cfg(not(test))]\nmod not_test;\n\
             pub mod inner;\n#[cfg(test)]\n#[path = \"elsewhere/moved.rs\"]\nmod moved;\n",
        ),
        ("crates/a/src/helper.rs", "mod child;\n"),
        ("crates/a/src/helper/child.rs", "fn c() {}\n"),
        ("crates/a/src/shared.rs", "fn s() {}\n"),
        ("crates/a/src/open.rs", "#[cfg(test)]\nmod fixture;\n"),
        ("crates/a/src/open/fixture.rs", "fn f() {}\n"),
        ("crates/a/src/unix_only.rs", "fn u() {}\n"),
        ("crates/a/src/tests.rs", "fn t() {}\n"),
        ("crates/a/src/nested/mod.rs", "#[cfg(test)]\nmod deep;\n"),
        ("crates/a/src/nested/deep/mod.rs", "fn d() {}\n"),
        ("crates/a/src/feature_only.rs", "fn g() {}\n"),
        ("crates/a/src/all_test.rs", "fn g() {}\n"),
        ("crates/a/src/all_feature.rs", "fn g() {}\n"),
        ("crates/a/src/any_shipped.rs", "fn g() {}\n"),
        ("crates/a/src/not_test.rs", "fn g() {}\n"),
        (
            "crates/a/src/inner.rs",
            "//! Held for tests.\n#![cfg(test)]\nfn i() {}\n",
        ),
        ("crates/a/src/elsewhere/moved.rs", "fn m() {}\n"),
        ("crates/a/src/bin/fixture.rs", "fn main() {}\n"),
        ("crates/a/src/bin/real.rs", "fn main() {}\n"),
        ("crates/a/src/bin/implied/main.rs", "fn main() {}\n"),
    ];
    for (rel, body) in files {
        let file = ws.join(rel);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, body).expect("write fixture");
    }
    let derived: Vec<String> = crate::test_helpers::test_only_files_below(ws)
        .iter()
        .map(crate::to_posix_string)
        .collect();
    assert_eq!(
        derived,
        [
            "crates/a/src/all_feature.rs",
            "crates/a/src/all_test.rs",
            "crates/a/src/bin/fixture.rs",
            "crates/a/src/bin/implied/main.rs",
            "crates/a/src/elsewhere/moved.rs",
            "crates/a/src/feature_only.rs",
            "crates/a/src/helper/child.rs",
            "crates/a/src/helper.rs",
            "crates/a/src/inner.rs",
            "crates/a/src/nested/deep/mod.rs",
            "crates/a/src/open/fixture.rs",
            "crates/a/src/shared.rs",
        ]
    );
}

/// Each `cfg` predicate shape the workspace spells is judged by whether a
/// shipped build can meet it.
#[test]
fn cfg_requires_test_holds_only_for_predicates_no_shipped_build_meets() {
    let judge = crate::test_helpers::cfg_requires_test;
    for held in [
        "test",
        "feature = \"test-helpers\"",
        "any(test, feature = \"test-helpers\")",
        "all(test, unix)",
        "all(unix, feature = \"test-helpers\")",
        "all(test, not(windows))",
        "all(test, feature = \"crd\")",
        "any(all(test, unix), test)",
    ] {
        assert!(judge(held), "cfg({held}) is met only by a test build");
    }
    for open in [
        "unix",
        "feature = \"crd\"",
        "any(windows, test)",
        "any(target_os = \"macos\", test)",
        "not(test)",
        "not(any(test, feature = \"test-helpers\"))",
        "any()",
    ] {
        assert!(!judge(open), "cfg({open}) is met by some shipped build");
    }
}

/// Every gate spelling the workspace writes, read by the one attribute parser
/// the scan, the module walk and the file walk share: an outer and an inner
/// `cfg(test)`, a `test-helpers` gate beside `test` and alone, and a platform
/// gate every shipped build can meet. An outer spelling gates the item below
/// it in the line scan too; an inner one gates its whole file, which the line
/// scan leaves to [`crate::test_helpers::is_test_only_file`].
#[test]
fn every_gate_spelling_is_read_by_the_one_attribute_parser() {
    use crate::test_helpers::{Gate, attribute_gate, line_gates};
    let spellings = [
        ("#[cfg(test)]", Some(Gate::Test), true),
        ("#![cfg(test)]", Some(Gate::Test), false),
        (
            "#[cfg(any(test, feature = \"test-helpers\"))]",
            Some(Gate::TestHelpers),
            true,
        ),
        (
            "#[cfg(feature = \"test-helpers\")]",
            Some(Gate::TestHelpers),
            true,
        ),
        ("#[cfg(target_os = \"linux\")]", None, true),
    ];
    for (attr, gate, outer) in spellings {
        assert_eq!(attribute_gate(attr), gate, "{attr}");
        assert_eq!(
            attribute_gate(&format!("    {attr} // trailing note")),
            gate,
            "{attr}, indented and followed by a comment"
        );
        let scanned = line_gates(&format!("{attr}\nfn item() {{}}\n"));
        let expected = if outer { gate } else { None };
        assert_eq!(scanned, [expected, expected], "{attr} over one item");
    }
    assert_eq!(
        attribute_gate("// #[cfg(test)]"),
        None,
        "a commented-out gate"
    );
    assert_eq!(
        attribute_gate("    let s = \"#[cfg(test)]\";"),
        None,
        "a gate spelled inside a literal"
    );
}

/// A checkout that sits under a directory named `tests` classifies its files
/// the same as any other: only the components below the workspace root count.
#[test]
fn is_test_source_judges_only_components_below_the_workspace_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = tmp.path().join("tests").join("ws");
    let production = ws.join("crates/cfgd/src/cli/status.rs");
    let held = ws.join("crates/cfgd/tests/a.rs");
    for file in [&production, &held] {
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(file, "fn f() {}\n").expect("write source");
    }
    let below = crate::test_helpers::is_test_source_below;
    assert!(
        !below(&ws, &production),
        "{} is production",
        production.display()
    );
    assert!(below(&ws, &held), "{} holds tests alone", held.display());

    let listed = rust_sources_under(&ws.join("crates"));
    let (tests, sources): (Vec<_>, Vec<_>) = listed.iter().partition(|p| below(&ws, p));
    assert_eq!(sources, vec![&production], "the listed production sources");
    assert_eq!(tests, vec![&held], "the listed test-only sources");

    // Cargo keeps the logical path it was given, so a root reached through a
    // link is judged under the link; canonicalizing a file resolves it past the
    // link, out from under that root, where the whole path is judged.
    #[cfg(unix)]
    {
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&ws, &link).expect("symlink the workspace");
        let logical = link.join("crates/cfgd/src/cli/status.rs");
        let listed = rust_sources_under(&link.join("crates"));
        assert!(
            listed.contains(&logical),
            "the listing keeps the logical path"
        );
        assert!(
            !below(&link, &logical),
            "{} is production",
            logical.display()
        );
        let physical = logical.canonicalize().expect("canonicalize");
        assert!(
            below(&link, &physical),
            "{} sits outside the link root, so its `tests` ancestor is judged",
            physical.display()
        );
    }
}

/// A scan skipping the files built only for tests asks
/// `is_test_only_file(path)`. A bare `"test_helpers.rs"` skips every file of
/// that name in any crate, production ones included, and misses the fixture
/// binary and any module gated the same way. A copy of
/// either name in any source fails here; a full path naming one specific file
/// is a different question and passes.
#[test]
fn no_scan_hand_copies_the_test_only_file_rule() {
    // Per crate root: the `.rs` files below `crates/<root>` (`find … -name '*.rs'`)
    // and the lines calling `is_test_only_file(` there, its definition in
    // cfgd-core's test_helpers.rs left out, both counted when the rule last moved.
    const FLOORS: [(&str, usize, usize); 7] = [
        ("cfgd", 259, 18),
        ("cfgd-core", 243, 16),
        ("cfgd-crd", 2, 0),
        ("cfgd-csi", 11, 0),
        ("cfgd-operator", 64, 1),
        ("cfgd-schema", 2, 0),
        ("cfgd-test-fixtures", 1, 0),
    ];
    // The calls across every root, counted the same way: a call moving from one
    // crate to another keeps each crate's floor while the workspace loses one.
    const TOTAL_ASKS: usize = 35;
    // Built from pieces so this file's own needles are not read as copies.
    let tells = [
        concat!("\"test_", "helpers.rs\""),
        concat!("\"fake_", "cosign.rs\""),
    ];
    let crates = workspace_root().join("crates");
    let mut counted: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        let root = path
            .strip_prefix(&crates)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let entry = counted.entry(root).or_default();
        entry.0 += 1;
        // unfloored-slice-ok: a hand copy anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        for (row, line) in body.lines().enumerate() {
            let code = crate::test_helpers::code_span(line);
            let stripped = crate::test_helpers::code_line(line);
            if !crate::test_helpers::strip_item_lead(&stripped).starts_with("fn is_test_only_file(")
            {
                entry.1 += stripped.matches("is_test_only_file(").count();
            }
            if tells.iter().any(|tell| code.contains(tell)) {
                offenders.push(format!("{}:{}: {}", path.display(), row + 1, line.trim()));
            }
        }
    }
    let short: Vec<String> = FLOORS
        .iter()
        .filter_map(|&(root, files, asks)| {
            let (read, asked) = counted.get(root).copied().unwrap_or_default();
            (read < files || asked < asks).then(|| {
                format!("{root}: {read} sources (floor {files}), {asked} calls (floor {asks})")
            })
        })
        .collect();
    assert!(
        short.is_empty(),
        "the scan read fewer sources, or found fewer calls routing a scan through \
         is_test_only_file, than each crate root holds:\n{}\ncounted: {counted:?}",
        short.join("\n")
    );
    let asked: usize = counted.values().map(|(_, asked)| asked).sum();
    assert!(
        asked >= TOTAL_ASKS,
        "the workspace holds {asked} calls routing a scan through is_test_only_file, \
         fewer than its {TOTAL_ASKS}: {counted:?}"
    );
    let unfloored: Vec<&String> = counted
        .keys()
        .filter(|root| !FLOORS.iter().any(|(r, _, _)| r == root))
        .collect();
    assert!(
        unfloored.is_empty(),
        "crate roots with no floor above: {unfloored:?}"
    );
    assert!(
        offenders.is_empty(),
        "these lines skip a test-only file by its name; ask \
         `cfgd_core::test_helpers::is_test_only_file(path)` instead:\n{}",
        offenders.join("\n")
    );
}

/// Every scan that skips test-only files asks `is_test_source` which ones
/// those are; a second hand-written copy of the naming rule is how scans came
/// to disagree about `tests_*.rs` files and read their fixtures as production.
///
/// The tells are the rule's own spellings, read on each line's code with its
/// trailing comment cut: the `tests.rs` name, a `tests` prefix or component
/// compared against a path, and a `/tests/` substring. The helper's own file is
/// the one place allowed to spell them. No site is hatched: a narrower
/// question is the helper's answer plus a condition of its own, as the
/// integration-tests-alone scan asks `is_test_source(p) && !p.contains("/src/")`.
///
/// Both floors equal the population they were counted from, so a scan that stops
/// reading files, or a routed site that stops asking, fails here. The `asks`
/// count leaves out the tests that define the rule, whose calls check the
/// helper itself and route no scan through it, and reads each line with its
/// string literals blanked, so a message quoting the call counts for nothing.
#[test]
fn no_scan_hand_copies_the_test_source_naming_rule() {
    const FILES: usize = 581;
    const ASKS: usize = 81;
    // Built from pieces so this file's own needles are not read as copies.
    let tells = [
        concat!("\"tests", ".rs\""),
        concat!("starts_with(\"", "tests"),
        concat!("ends_with(\"", "tests\")"),
        concat!("== \"", "tests\""),
        concat!("OsStr::new(\"", "tests\")"),
        concat!("Some(\"", "tests"),
        concat!("contains(\"/", "tests/\")"),
        concat!("\"test", "s_"),
    ];
    let helper_home = Path::new("cfgd-core/src/test_helpers.rs");
    let mut files = 0usize;
    let mut asks = 0usize;
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(helper_home) {
            continue;
        }
        files += 1;
        // unfloored-slice-ok: a hand copy anywhere, tests included, is the subject.
        let body = walked_file_body(&path);
        let mut in_rule_test = false;
        for (row, line) in body.lines().enumerate() {
            let code = crate::test_helpers::code_span(line);
            if code.starts_with("fn is_test_source_") || code.starts_with("fn no_scan_hand_copies_")
            {
                in_rule_test = true;
            } else if in_rule_test && line == "}" {
                in_rule_test = false;
            }
            if !in_rule_test {
                asks += crate::test_helpers::code_line(line)
                    .matches("is_test_source(")
                    .count();
            }
            if tells.iter().any(|tell| code.contains(tell)) {
                offenders.push(format!("{}:{}: {}", path.display(), row + 1, line.trim()));
            }
        }
    }
    assert!(
        files >= FILES,
        "the scan read {files} sources under crates/, fewer than the {FILES} the workspace holds"
    );
    assert!(
        asks >= ASKS,
        "{asks} sites ask is_test_source, fewer than the {ASKS} that route a scan through it"
    );
    assert!(
        offenders.is_empty(),
        "these lines restate which files hold tests alone; ask \
         `cfgd_core::test_helpers::is_test_source(path)` instead:\n{}",
        offenders.join("\n")
    );
}

/// The methods that substitute a value for a `None` they are handed, whether
/// they return the value (`unwrap_or`), a filled `Option` (`or`) or write it
/// into the `Option` in place (`get_or_insert`).
const DEFAULTING_METHODS: [&str; 11] = [
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "map_or",
    "map_or_else",
    "is_some_and",
    "is_none_or",
    "or",
    "or_else",
    "get_or_insert",
    "get_or_insert_with",
];

/// The methods an `Option` passes through on its way to one of
/// [`DEFAULTING_METHODS`]: a field read just ahead of one of them is the
/// `Option` that call defaults.
const OPTION_STEPS: [&str; 9] = [
    "as_ref", "as_deref", "as_mut", "clone", "cloned", "copied", "and_then", "map", "filter",
];

/// The mark a defaulting read carries when the field it names belongs to a
/// struct outside `config/` that shares a section's field name.
const SECTION_HATCH: &str = "option-section-ok:";

/// The private deserialization mirror of `ConfigSpec`, moved into it field for
/// field by `parse_config`, so its `Option<…Config>` fields are no sections.
const DESERIALIZATION_MIRRORS: [&str; 1] = ["RawConfigSpec"];

/// Whether `attrs` carry a `cfg` that holds only in a test build.
fn test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .meta
                .require_list()
                .is_ok_and(|list| crate::test_helpers::cfg_requires_test(&list.tokens.to_string()))
    })
}

/// The last segment of an `impl` block's self type.
fn impl_owner(item: &syn::ItemImpl) -> Option<String> {
    match &*item.self_ty {
        syn::Type::Path(owner) => owner.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// What the files under `crates/cfgd-core/src/config/` declare about their
/// `Option<…Config>` sections.
#[derive(Default)]
struct ConfigSections {
    /// Every `Option<…Config>` field, as `(struct, field)`.
    fields: std::collections::BTreeSet<(String, String)>,
    /// Every struct's named fields with their types, by struct.
    structs: std::collections::BTreeMap<String, Vec<(String, syn::Type)>>,
    /// Every `<field>_effective` method, as `(impl type, field)`.
    accessors: std::collections::BTreeSet<(String, String)>,
    /// The `<field>_effective` methods `ConfigSpec::effective` calls, by field.
    filled: std::collections::BTreeSet<String>,
    /// Every omitted value an accessor spells out in its own body, as
    /// `impl type::accessor: what`.
    hand_written: Vec<String>,
}

/// The values a `<field>_effective` accessor spells itself: a struct literal,
/// a literal, or a `static` built by anything other than
/// `LazyLock::new(<type>::default)`. The omitted value is the one the type's
/// `Default` gives, or for a leaf a named constant the release code reads as
/// well, which is also what the schema publishes and what a declared empty
/// block holds, so a second spelling of it can only drift.
#[derive(Default)]
struct HandWrittenDefaults(Vec<String>);

impl<'ast> syn::visit::Visit<'ast> for HandWrittenDefaults {
    fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
        let at = row_of(syn::spanned::Spanned::span(expr)) + 1;
        self.0.push(format!("a struct literal on line {at}"));
    }

    fn visit_expr_lit(&mut self, expr: &'ast syn::ExprLit) {
        let at = row_of(syn::spanned::Spanned::span(expr)) + 1;
        self.0.push(format!("a literal on line {at}"));
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        let by_default = match peel(&item.expr) {
            syn::Expr::Call(call) => {
                let lazy = matches!(peel(&call.func), syn::Expr::Path(p)
                    if p.path.segments.len() >= 2
                        && p.path.segments[p.path.segments.len() - 2].ident == "LazyLock"
                        && p.path.segments.last().is_some_and(|s| s.ident == "new"));
                lazy && call.args.len() == 1
                    && matches!(peel(&call.args[0]), syn::Expr::Path(p)
                        if p.path.segments.last().is_some_and(|s| s.ident == "default"))
            }
            _ => false,
        };
        if !by_default {
            let at = row_of(syn::spanned::Spanned::span(&item.expr)) + 1;
            self.0.push(format!("a static built by hand on line {at}"));
        }
        syn::visit::visit_item_static(self, item);
    }
}

impl ConfigSections {
    /// Every `Option` field reached from `ConfigSpec` whose type is no struct
    /// under `config/`, as `(struct, field)`. The walk steps through a field
    /// typed as a struct or an `Option` of one; a `Vec` or a map holds
    /// entries a document lists, which are no part of its own shape.
    fn leaves(&self) -> std::collections::BTreeSet<(String, String)> {
        let mut leaves = std::collections::BTreeSet::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = vec!["ConfigSpec".to_string()];
        while let Some(owner) = queue.pop() {
            if !seen.insert(owner.clone()) {
                continue;
            }
            for (field, ty) in self.structs.get(&owner).into_iter().flatten() {
                let inner = option_inner(ty);
                match plain_type_name(inner.unwrap_or(ty)) {
                    Some(name) if self.structs.contains_key(&name) => queue.push(name),
                    _ if inner.is_some() => {
                        leaves.insert((owner.clone(), field.clone()));
                    }
                    _ => {}
                }
            }
        }
        leaves
    }
}

/// The `T` of an `Option<T>`.
fn option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let last = path.path.segments.last()?;
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    match args.args.first()? {
        syn::GenericArgument::Type(inner) if last.ident == "Option" => Some(inner),
        _ => None,
    }
}

/// The name of a type written as a bare path with no generic arguments.
fn plain_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let last = path.path.segments.last()?;
    last.arguments.is_none().then(|| last.ident.to_string())
}

/// The `<field>_effective` methods an expression calls, by field.
struct AccessorCalls<'s>(&'s mut std::collections::BTreeSet<String>);

impl<'ast> syn::visit::Visit<'ast> for AccessorCalls<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if let Some(field) = call.method.to_string().strip_suffix("_effective") {
            self.0.insert(field.to_string());
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

impl<'ast> syn::visit::Visit<'ast> for ConfigSections {
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        if test_gated(&item.attrs) {
            return;
        }
        self.structs.insert(
            item.ident.to_string(),
            item.fields
                .iter()
                .filter_map(|f| Some((f.ident.as_ref()?.to_string(), f.ty.clone())))
                .collect(),
        );
        for field in &item.fields {
            let (Some(ident), syn::Type::Path(ty)) = (&field.ident, &field.ty) else {
                continue;
            };
            let Some(outer) = ty.path.segments.last() else {
                continue;
            };
            let syn::PathArguments::AngleBracketed(args) = &outer.arguments else {
                continue;
            };
            let section = args.args.iter().any(|arg| match arg {
                syn::GenericArgument::Type(syn::Type::Path(inner)) => inner
                    .path
                    .segments
                    .last()
                    .is_some_and(|s| s.ident.to_string().ends_with("Config")),
                _ => false,
            });
            if outer.ident == "Option" && section {
                self.fields
                    .insert((item.ident.to_string(), ident.to_string()));
            }
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if test_gated(&item.attrs) {
            return;
        }
        let Some(owner) = impl_owner(item) else {
            return;
        };
        for inner in &item.items {
            let syn::ImplItem::Fn(func) = inner else {
                continue;
            };
            let name = func.sig.ident.to_string();
            if let Some(field) = name.strip_suffix("_effective") {
                self.accessors.insert((owner.clone(), field.to_string()));
                let mut spelled = HandWrittenDefaults::default();
                syn::visit::Visit::visit_block(&mut spelled, &func.block);
                self.hand_written.extend(
                    spelled
                        .0
                        .into_iter()
                        .map(|what| format!("{owner}::{name}: {what}")),
                );
            }
            if owner == "ConfigSpec" && name == "effective" {
                syn::visit::Visit::visit_block(&mut AccessorCalls(&mut self.filled), &func.block);
            }
        }
    }
}

/// The macros whose expansion puts no value in a `None`'s place: a panic,
/// an early `bail!`, or a log line.
const VALUELESS_MACROS: [&str; 14] = [
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "bail",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "print",
    "println",
    "eprint",
    "eprintln",
];

/// Whether `mac` is one of [`VALUELESS_MACROS`].
fn valueless_macro(mac: &syn::Macro) -> bool {
    mac.path
        .segments
        .last()
        .is_some_and(|s| VALUELESS_MACROS.contains(&s.ident.to_string().as_str()))
}

/// Whether a `return` carries no value of the section's own: nothing, an
/// `Err(..)`, a `None`, or `Ok(())`.
fn valueless_return(expr: &syn::Expr) -> bool {
    match peel(expr) {
        syn::Expr::Path(path) => path.path.is_ident("None"),
        syn::Expr::Call(call) => match peel(&call.func) {
            syn::Expr::Path(func) if func.path.is_ident("Err") => true,
            syn::Expr::Path(func) if func.path.is_ident("Ok") => {
                call.args.len() == 1
                    && matches!(peel(&call.args[0]), syn::Expr::Tuple(t) if t.elems.is_empty())
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether the branch that runs for a `None` puts a value in its place: its
/// tail is a value, it assigns one, or it returns one. An empty branch, a
/// log line, a panic, `continue`, a valueless `break`, and a `return` of an
/// error, a `None` or `Ok(())` put none there.
fn substitutes(expr: &syn::Expr) -> bool {
    match peel(expr) {
        syn::Expr::Block(block) => block_substitutes(&block.block),
        syn::Expr::Return(ret) => ret.expr.as_deref().is_some_and(|e| !valueless_return(e)),
        syn::Expr::Continue(_) => false,
        syn::Expr::Path(path) if path.path.is_ident("None") => false,
        syn::Expr::Break(brk) => brk.expr.is_some(),
        syn::Expr::Tuple(tuple) => !tuple.elems.is_empty(),
        syn::Expr::Macro(mac) => !valueless_macro(&mac.mac),
        _ => true,
    }
}

/// [`substitutes`] for a block: an assignment anywhere in it, or its tail.
fn block_substitutes(block: &syn::Block) -> bool {
    let assigns = block
        .stmts
        .iter()
        .any(|stmt| matches!(stmt, syn::Stmt::Expr(syn::Expr::Assign(_), _)));
    assigns
        || match block.stmts.last() {
            Some(syn::Stmt::Expr(tail, None)) => substitutes(tail),
            Some(syn::Stmt::Expr(tail @ syn::Expr::Return(_), Some(_))) => substitutes(tail),
            Some(syn::Stmt::Macro(mac)) if mac.semi_token.is_none() => !valueless_macro(&mac.mac),
            _ => false,
        }
}

/// Whether a `match` arm's pattern takes the `None`: `None` itself or `_`.
fn takes_none(pat: &syn::Pat) -> bool {
    match pat {
        syn::Pat::Wild(_) => true,
        syn::Pat::Ident(ident) => ident.ident == "None" && ident.subpat.is_none(),
        syn::Pat::Path(path) => path.path.is_ident("None"),
        syn::Pat::Or(or) => or.cases.iter().any(takes_none),
        _ => false,
    }
}

/// The `let` conditions of an `if` condition, through its `&&` chain.
fn let_conditions(cond: &syn::Expr) -> Vec<&syn::ExprLet> {
    match cond {
        syn::Expr::Let(cond) => vec![cond],
        syn::Expr::Binary(bin) if matches!(bin.op, syn::BinOp::And(_)) => {
            let mut lets = let_conditions(&bin.left);
            lets.extend(let_conditions(&bin.right));
            lets
        }
        syn::Expr::Paren(paren) => let_conditions(&paren.expr),
        _ => Vec::new(),
    }
}

/// The value a `None` test asks about and whether the test is true for a
/// `None`: `x.is_none()`, `x.is_some()`, `matches!(x, None)` or
/// `matches!(x, Some(..))`, each possibly behind `!`.
fn none_test(cond: &syn::Expr) -> Option<(syn::Expr, bool)> {
    match peel(cond) {
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Not(_)) => {
            none_test(&unary.expr).map(|(tested, on_none)| (tested, !on_none))
        }
        syn::Expr::MethodCall(call) if call.args.is_empty() => {
            match call.method.to_string().as_str() {
                "is_none" => Some(((*call.receiver).clone(), true)),
                "is_some" => Some(((*call.receiver).clone(), false)),
                _ => None,
            }
        }
        syn::Expr::Macro(mac) if mac.mac.path.is_ident("matches") => {
            let (tested, pat) = mac
                .mac
                .parse_body_with(|input: syn::parse::ParseStream<'_>| {
                    let tested: syn::Expr = input.parse()?;
                    input.parse::<syn::Token![,]>()?;
                    let pat = syn::Pat::parse_multi_with_leading_vert(input)?;
                    input.parse::<proc_macro2::TokenStream>()?;
                    Ok((tested, pat))
                })
                .ok()?;
            Some((tested, takes_none(&pat)))
        }
        _ => None,
    }
}

/// One read that defaults a section's `None`: the field it names, the 0-based
/// row of the defaulting call, and the `impl` type and function it sits in.
struct DefaultingRead {
    field: String,
    row: usize,
    owner: Option<String>,
    function: Option<String>,
}

/// Every read in one source that hands a section field's `Option` to one of
/// [`DEFAULTING_METHODS`]: a field read (or a no-argument method named like
/// the field, `spec.theme()`) directly ahead of the defaulting call or of the
/// [`OPTION_STEPS`] leading to it, inside an `and_then` closure along that
/// chain, or through a `let` binding that holds such a read.
struct DefaultingReads<'f> {
    fields: &'f std::collections::BTreeSet<String>,
    owner: Option<String>,
    function: Option<String>,
    bindings: Vec<std::collections::HashMap<String, Vec<String>>>,
    found: Vec<DefaultingRead>,
}

impl DefaultingReads<'_> {
    /// The section fields `expr` yields as its `Option`, where `counted` says
    /// the value `expr` produces is the `Option` being defaulted.
    fn chain(&self, expr: &syn::Expr, counted: bool, out: &mut Vec<String>) {
        match peel(expr) {
            syn::Expr::MethodCall(call) => {
                let method = call.method.to_string();
                if counted && call.args.is_empty() && self.fields.contains(&method) {
                    out.push(method.clone());
                }
                if counted && method == "and_then" {
                    for arg in &call.args {
                        if let syn::Expr::Closure(closure) = peel(arg) {
                            let body = match &*closure.body {
                                syn::Expr::Block(block) => match block.block.stmts.last() {
                                    Some(syn::Stmt::Expr(tail, None)) => tail,
                                    _ => continue,
                                },
                                body => body,
                            };
                            self.chain(body, true, out);
                        }
                    }
                }
                let passes = OPTION_STEPS.contains(&method.as_str())
                    || DEFAULTING_METHODS.contains(&method.as_str());
                self.chain(&call.receiver, counted && passes, out);
            }
            syn::Expr::Field(field) => {
                if let (true, syn::Member::Named(name)) = (counted, &field.member) {
                    out.push(name.to_string());
                }
                self.chain(&field.base, false, out);
            }
            syn::Expr::Path(path) if counted => {
                if let Some(name) = path.path.get_ident().map(ToString::to_string)
                    && let Some(held) = self.bindings.iter().rev().find_map(|s| s.get(&name))
                {
                    out.extend(held.iter().cloned());
                }
            }
            syn::Expr::Try(tried) => self.chain(&tried.expr, counted, out),
            _ => {}
        }
    }

    /// Record every section field `expr` yields as a read defaulted at `span`.
    fn record(&mut self, expr: &syn::Expr, span: proc_macro2::Span) {
        let mut read = Vec::new();
        self.chain(expr, true, &mut read);
        read.sort();
        read.dedup();
        for field in read.into_iter().filter(|f| self.fields.contains(f)) {
            self.found.push(DefaultingRead {
                field,
                row: row_of(span),
                owner: self.owner.clone(),
                function: self.function.clone(),
            });
        }
    }

    fn in_function(&mut self, name: String, visit: impl FnOnce(&mut Self)) {
        let outer = self.function.replace(name);
        self.bindings.push(Default::default());
        visit(self);
        self.bindings.pop();
        self.function = outer;
    }
}

impl<'ast> syn::visit::Visit<'ast> for DefaultingReads<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !test_gated(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if test_gated(&item.attrs) {
            return;
        }
        let outer = std::mem::replace(&mut self.owner, impl_owner(item));
        syn::visit::visit_item_impl(self, item);
        self.owner = outer;
    }

    fn visit_impl_item_fn(&mut self, func: &'ast syn::ImplItemFn) {
        if !test_gated(&func.attrs) {
            self.in_function(func.sig.ident.to_string(), |walk| {
                syn::visit::visit_impl_item_fn(walk, func);
            });
        }
    }

    fn visit_item_fn(&mut self, func: &'ast syn::ItemFn) {
        if test_gated(&func.attrs) {
            return;
        }
        let outer = self.owner.take();
        self.in_function(func.sig.ident.to_string(), |walk| {
            syn::visit::visit_item_fn(walk, func);
        });
        self.owner = outer;
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init
            && let Some((_, otherwise)) = &init.diverge
            && substitutes(otherwise)
        {
            self.record(&init.expr, local.let_token.span);
        }
        let pat = match &local.pat {
            syn::Pat::Type(typed) => &*typed.pat,
            pat => pat,
        };
        if let (syn::Pat::Ident(name), Some(init)) = (pat, &local.init) {
            let mut held = Vec::new();
            self.chain(&init.expr, true, &mut held);
            if !held.is_empty()
                && let Some(scope) = self.bindings.last_mut()
            {
                scope.insert(name.ident.to_string(), held);
            }
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if DEFAULTING_METHODS.contains(&call.method.to_string().as_str()) {
            self.record(&call.receiver, call.method.span());
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        let otherwise = expr.else_branch.as_ref().map(|(_, e)| &**e);
        let lets = let_conditions(&expr.cond);
        if !lets.is_empty() {
            if otherwise.is_some_and(substitutes) {
                for cond in lets {
                    self.record(&cond.expr, expr.if_token.span);
                }
            }
        } else if let Some((tested, on_none)) = none_test(&expr.cond) {
            let branch = if on_none {
                block_substitutes(&expr.then_branch)
            } else {
                otherwise.is_some_and(substitutes)
            };
            if branch {
                self.record(&tested, expr.if_token.span);
            }
        }
        syn::visit::visit_expr_if(self, expr);
    }

    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        if expr
            .arms
            .iter()
            .any(|arm| takes_none(&arm.pat) && substitutes(&arm.body))
        {
            self.record(&expr.expr, expr.match_token.span);
        }
        syn::visit::visit_expr_match(self, expr);
    }
}

/// Every `Option<…Config>` section of a config document, and every `Option`
/// leaf reached from `ConfigSpec` through them, means one of two things when
/// a document omits it, and the build reads it one way for each. A field
/// production substitutes a value for (defaults apply) is read through ONE
/// `<field>_effective` accessor on the struct declaring it, and
/// `ConfigSpec::effective`, which `cfgd config get` answers from, fills it in
/// through that accessor; a section whose omission turns its feature off, or
/// a leaf whose absence means nothing is set, has no accessor, and no
/// production read substitutes a value for it.
/// [`crate::test_helpers::OMITTED_FIELDS`] records which is which, with the
/// readers and the omitted value of each, and this walk holds the tree to it:
///
/// - every `Option<…Config>` field a struct under `config/` declares, and
///   every `Option` leaf reached from `ConfigSpec` through struct fields
///   (never through a `Vec` or a map), is a row, and every row is one;
/// - a row with an omitted value is a field with an accessor, and a row
///   without one is a field without;
/// - a defaulting read of a row's field (a field read handed to
///   `unwrap_or`, `unwrap_or_else`, `unwrap_or_default`, `map_or`,
///   `map_or_else`, `is_some_and` or `is_none_or`, directly, through the
///   `Option` steps between, inside an `and_then` closure, or through a
///   `let` binding; an `if let`, a `let … else` or a `match` whose `None`
///   branch puts a value in its place; or an `is_none`, `is_some` or
///   `matches!` test whose `None` branch does) sits inside that field's
///   accessor, and every accessor holds one. A branch that is empty, logs,
///   panics, continues, or returns an error, a `None` or `Ok(())` puts no
///   value there;
/// - every section accessor's omitted value is
///   `LazyLock::new(<type>::default)` and every leaf accessor's is its type's
///   `Default` or a named constant, so no accessor body holds a struct
///   literal, a literal or another `static`;
/// - `ConfigSpec::effective` calls every accessor.
///
/// Fields are matched by name, so a read of a same-named field on a struct
/// outside `config/` carries `// option-section-ok: <why>` on its line or the
/// one above. Every production source of every crate is parsed whole with
/// its test-gated items skipped, a source `syn` cannot parse fails the walk,
/// and each crate's count of parsed sources has a floor.
#[test]
fn every_option_config_section_is_read_through_its_effective_accessor_or_never_defaulted() {
    const CONFIG_FILES_FLOOR: usize = 21;
    const WALK_ROOTS: &[(&str, usize)] = &[
        ("cfgd", 145),
        ("cfgd-core", 193),
        ("cfgd-crd", 1),
        ("cfgd-csi", 8),
        ("cfgd-operator", 42),
        ("cfgd-schema", 2),
        ("cfgd-test-fixtures", 1),
    ];
    let mut sections = ConfigSections::default();
    let mut unparsed = Vec::new();
    let mut parsed: Vec<(&str, &Path, syn::File)> = Vec::new();
    let mut config_files = 0;
    for (root, files) in
        crate::test_helpers::production_sources_per_root(crate::test_helpers::WORKSPACE_CRATES)
    {
        for (path, production) in files {
            if production.trim().is_empty() {
                continue;
            }
            // unfloored-slice-ok: syn parses whole items; test-gated ones are skipped by attribute.
            match syn::parse_file(&walked_file_body(path)) {
                Ok(file) => {
                    if crate::to_posix_string(path).contains("crates/cfgd-core/src/config/") {
                        config_files += 1;
                        syn::visit::Visit::visit_file(&mut sections, &file);
                    }
                    parsed.push((root, path.as_path(), file));
                }
                Err(e) => unparsed.push(format!("{}: {e}", source_label(path))),
            }
        }
    }
    assert!(
        unparsed.is_empty(),
        "a source syn cannot parse is a source this walk cannot judge:\n{}",
        unparsed.join("\n")
    );
    assert!(
        config_files >= CONFIG_FILES_FLOOR,
        "the walk read {config_files} sources under config/, below the floor of {CONFIG_FILES_FLOOR}"
    );
    let short: Vec<String> = WALK_ROOTS
        .iter()
        .filter_map(|&(root, floor)| {
            let read = parsed.iter().filter(|(r, ..)| *r == root).count();
            (read < floor).then(|| format!("{root}: {read} sources parsed, floor {floor}"))
        })
        .collect();
    assert!(
        short.is_empty(),
        "the walk stopped reading sources:\n{}",
        short.join("\n")
    );

    let fields: std::collections::BTreeSet<(String, String)> = sections
        .fields
        .iter()
        .filter(|(owner, _)| !DESERIALIZATION_MIRRORS.contains(&owner.as_str()))
        .cloned()
        .chain(sections.leaves())
        .collect();
    let rows: std::collections::BTreeSet<(String, String)> = crate::test_helpers::OMITTED_FIELDS
        .iter()
        .map(|row| (row.owner.to_string(), row.field.to_string()))
        .collect();
    assert_eq!(
        fields, rows,
        "every Option<…Config> field under config/ and every Option leaf reached from \
         ConfigSpec is a row of OMITTED_FIELDS, classified by what production does when a \
         document omits it"
    );
    let defaulted: std::collections::BTreeSet<(String, String)> =
        crate::test_helpers::OMITTED_FIELDS
            .iter()
            .filter(|row| row.omitted.is_some())
            .map(|row| (row.owner.to_string(), row.field.to_string()))
            .collect();
    assert_eq!(
        sections.accessors, defaulted,
        "a field whose omission production reads as a value has a `<field>_effective` \
         accessor on its struct, and a feature-off section has none"
    );
    assert!(
        sections.hand_written.is_empty(),
        "an accessor's omitted value is its type's `Default` (through \
         `LazyLock::new(<type>::default)` for a section) or a named constant, the value \
         the schema publishes and a declared empty block holds; a value the accessor \
         spells itself is a second copy of that default:\n{}",
        sections.hand_written.join("\n")
    );
    let accessor_fields: std::collections::BTreeSet<String> = sections
        .accessors
        .iter()
        .map(|(_, field)| field.clone())
        .collect();
    assert_eq!(
        sections.filled, accessor_fields,
        "ConfigSpec::effective fills every field in through its accessor"
    );

    let names: std::collections::BTreeSet<String> =
        fields.iter().map(|(_, field)| field.clone()).collect();
    let mut offenders = Vec::new();
    let mut held: std::collections::BTreeSet<(String, String)> = Default::default();
    for (_, path, file) in &parsed {
        let mut walk = DefaultingReads {
            fields: &names,
            owner: None,
            function: None,
            bindings: Vec::new(),
            found: Vec::new(),
        };
        syn::visit::Visit::visit_file(&mut walk, file);
        if walk.found.is_empty() {
            continue;
        }
        // unfloored-slice-ok: rows are judged against the file syn parsed.
        let body = walked_file_body(path);
        let lines: Vec<&str> = body.lines().collect();
        for read in walk.found {
            let accessor = read
                .function
                .as_deref()
                .and_then(|f| f.strip_suffix("_effective"));
            let owner = read.owner.as_deref().unwrap_or_default();
            if accessor == Some(read.field.as_str())
                && sections
                    .accessors
                    .contains(&(owner.to_string(), read.field.clone()))
            {
                held.insert((owner.to_string(), read.field));
                continue;
            }
            let above = read
                .row
                .checked_sub(1)
                .map(|i| lines[i])
                .unwrap_or_default();
            if carries_hatch(lines[read.row], SECTION_HATCH) || carries_hatch(above, SECTION_HATCH)
            {
                continue;
            }
            offenders.push(format!(
                "{}:{}: `{}` defaulted in {}",
                source_label(path),
                read.row + 1,
                read.field,
                read.function
                    .as_deref()
                    .unwrap_or("an item outside any function"),
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "an omitted field's value is read through its `<field>_effective` accessor, and a \
         feature-off section or a leaf whose absence means nothing is set is never \
         defaulted; a same-named field on another struct \
         carries `// {SECTION_HATCH} <why>`:\n{}",
        offenders.join("\n")
    );
    assert_eq!(
        held, sections.accessors,
        "every accessor substitutes its field's omitted value itself"
    );
}

/// Every shape that puts a value in a section's `None` place is a defaulting
/// read of that field, and the same shape that puts none there is not: each
/// row is a function body over a `cfg` whose `daemon` is a section, with the
/// number of defaulting reads the walk must find in it.
#[test]
fn every_defaulting_shape_of_a_section_read_is_found_and_no_other() {
    const SHAPES: &[(&str, &str, usize)] = &[
        ("unwrap_or", "cfg.daemon.as_ref().unwrap_or(&D)", 1),
        (
            "unwrap_or_else",
            "cfg.daemon.clone().unwrap_or_else(make)",
            1,
        ),
        (
            "unwrap_or_default",
            "cfg.daemon.clone().unwrap_or_default()",
            1,
        ),
        (
            "cloned then unwrap_or_default",
            "cfg.daemon.as_ref().cloned().unwrap_or_default()",
            1,
        ),
        (
            "as_deref then unwrap_or",
            "cfg.daemon.as_deref().unwrap_or(\"x\")",
            1,
        ),
        ("map_or", "cfg.daemon.as_ref().map_or(5, |d| d.n)", 1),
        (
            "map_or_else",
            "cfg.daemon.as_ref().map_or_else(make, |d| d.n)",
            1,
        ),
        (
            "is_some_and",
            "cfg.daemon.as_ref().is_some_and(|d| d.on)",
            1,
        ),
        ("or", "cfg.daemon.as_ref().or(Some(&D))", 1),
        ("or on another field", "cfg.name.as_ref().or(Some(&N))", 0),
        (
            "or_else",
            "cfg.daemon.clone().or_else(|| Some(D::default()))",
            1,
        ),
        (
            "or_else on another field",
            "cfg.name.clone().or_else(|| Some(N::default()))",
            0,
        ),
        ("get_or_insert", "cfg.daemon.get_or_insert(D::default())", 1),
        (
            "get_or_insert on another field",
            "cfg.name.get_or_insert(N::default())",
            0,
        ),
        (
            "get_or_insert_with",
            "cfg.daemon.get_or_insert_with(D::default)",
            1,
        ),
        (
            "get_or_insert_with on another field",
            "cfg.name.get_or_insert_with(N::default)",
            0,
        ),
        ("map alone", "cfg.daemon.as_ref().map(|d| d.n)", 0),
        (
            "match passing None through",
            "match cfg.daemon.as_ref() { Some(d) => Some(d.n), None => None }",
            0,
        ),
        (
            "if let with a value in else",
            "if let Some(d) = cfg.daemon.as_ref() { d.n } else { 5 }",
            1,
        ),
        (
            "if let with an assignment in else",
            "let mut n = 0; if let Some(d) = &cfg.daemon { n = d.n; } else { n = 5; } n",
            1,
        ),
        (
            "if let with no else",
            "if let Some(d) = &cfg.daemon { run(d); }",
            0,
        ),
        (
            "if let with an error in else",
            "if let Some(d) = &cfg.daemon { d.n } else { return Err(e); }",
            0,
        ),
        (
            "if let with a log line in else",
            "if let Some(d) = &cfg.daemon { run(d); } else { tracing::warn!(\"none\"); }",
            0,
        ),
        (
            "if let chain with a value in else",
            "if let Some(d) = &cfg.daemon && d.on { d.n } else { 5 }",
            1,
        ),
        (
            "match with a value for None",
            "match &cfg.daemon { Some(d) => d.n, None => 5 }",
            1,
        ),
        (
            "match with a value for _",
            "match cfg.daemon { Some(d) => d.n, _ => 5 }",
            1,
        ),
        (
            "match with nothing for None",
            "match &cfg.daemon { Some(d) => run(d), None => {} }",
            0,
        ),
        (
            "match that returns an error for None",
            "match &cfg.daemon { Some(d) => d.n, None => return Err(e) }",
            0,
        ),
        (
            "match that continues for None",
            "for c in cs { match &c.daemon { Some(d) => run(d), None => continue } }",
            0,
        ),
        (
            "let else returning a value",
            "let Some(d) = &cfg.daemon else { return 5; }; d.n",
            1,
        ),
        (
            "let else returning an error",
            "let Some(d) = &cfg.daemon else { return Err(e); }; d.n",
            0,
        ),
        (
            "let else continuing",
            "for c in cs { let Some(d) = &c.daemon else { continue; }; run(d); }",
            0,
        ),
        (
            "is_none picking a value",
            "if cfg.daemon.is_none() { 5 } else { 6 }",
            1,
        ),
        (
            "is_none refusing",
            "if cfg.daemon.is_none() { return Err(e); } 6",
            0,
        ),
        (
            "is_some with a value in else",
            "if cfg.daemon.is_some() { 6 } else { 5 }",
            1,
        ),
        (
            "negated is_some picking a value",
            "if !cfg.daemon.is_some() { 5 } else { run(); }",
            1,
        ),
        (
            "negated is_none refusing",
            "if !cfg.daemon.is_none() { 6 } else { return Err(e) }",
            0,
        ),
        (
            "matches None picking a value",
            "if matches!(cfg.daemon, None) { 5 } else { 6 }",
            1,
        ),
        (
            "matches None refusing",
            "if matches!(cfg.daemon, None) { bail!(\"no daemon\") } 6",
            0,
        ),
        (
            "matches None as a plain bool",
            "let off = matches!(cfg.daemon, None); off",
            0,
        ),
        (
            "is_none as a plain bool",
            "let off = cfg.daemon.is_none(); off",
            0,
        ),
        (
            "a let binding defaulted later",
            "let d = cfg.daemon.as_ref(); d.unwrap_or(&D)",
            1,
        ),
        (
            "inside a closure",
            "cs.iter().map(|c| c.daemon.clone().unwrap_or_default())",
            1,
        ),
        (
            "another field",
            "if let Some(s) = &cfg.secrets { s.n } else { 5 }",
            0,
        ),
    ];
    let fields: std::collections::BTreeSet<String> =
        std::iter::once("daemon".to_string()).collect();
    let mut wrong = Vec::new();
    for (shape, body, expected) in SHAPES {
        let source = format!("fn f(cfg: &C) -> T {{ {body} }}");
        let file = syn::parse_file(&source).unwrap_or_else(|e| panic!("{shape}: {e}"));
        let mut walk = DefaultingReads {
            fields: &fields,
            owner: None,
            function: None,
            bindings: Vec::new(),
            found: Vec::new(),
        };
        syn::visit::Visit::visit_file(&mut walk, &file);
        if walk.found.len() != *expected {
            wrong.push(format!(
                "{shape}: found {}, expected {expected}",
                walk.found.len()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the defaulting walk misreads these shapes:\n{}",
        wrong.join("\n")
    );
}

/// The marker that exempts one profile read whose literal fallback names a real
/// profile to resolve, which no placeholder constant stands for.
const PROFILE_FALLBACK_HATCH: &str = "profile-fallback-ok:";

/// The constants a profile read may fall back to: the name a run reports when
/// it cannot derive one, and the word a surface prints for a document that
/// names none.
const PROFILE_PLACEHOLDERS: &[&str] = &["UNKNOWN_PROFILE", "NO_PROFILE_LABEL"];

/// Reads of a profile name that fall back to a string literal, and reads that
/// fall back to one of [`PROFILE_PLACEHOLDERS`], across one source.
#[derive(Default)]
struct ProfileFallbacks {
    literals: Vec<usize>,
    readers: usize,
}

impl ProfileFallbacks {
    /// Whether `expr` reads a field, method or binding whose name carries
    /// `profile`, along its receiver chain or inside a closure that chain
    /// passes (`.and_then(|c| c.active_profile().ok())`).
    fn reads_profile(expr: &syn::Expr) -> bool {
        match peel(expr) {
            syn::Expr::MethodCall(call) => {
                call.method.to_string().contains("profile")
                    || Self::reads_profile(&call.receiver)
                    || call.args.iter().any(|arg| match peel(arg) {
                        syn::Expr::Closure(closure) => Self::reads_profile(&closure.body),
                        _ => false,
                    })
            }
            syn::Expr::Field(field) => {
                matches!(&field.member, syn::Member::Named(name) if name.to_string().contains("profile"))
                    || Self::reads_profile(&field.base)
            }
            syn::Expr::Path(path) => path
                .path
                .segments
                .last()
                .is_some_and(|seg| seg.ident.to_string().contains("profile")),
            syn::Expr::Try(tried) => Self::reads_profile(&tried.expr),
            _ => false,
        }
    }

    /// The fallback value past a closure and any `.to_string()` /
    /// `.to_owned()` / `.into()` that turns it into an owned string.
    fn fallback_value(expr: &syn::Expr) -> &syn::Expr {
        match peel(expr) {
            syn::Expr::Closure(closure) => Self::fallback_value(&closure.body),
            syn::Expr::MethodCall(call)
                if call.args.is_empty()
                    && ["to_string", "to_owned", "into"]
                        .contains(&call.method.to_string().as_str()) =>
            {
                Self::fallback_value(&call.receiver)
            }
            other => other,
        }
    }
}

impl<'ast> syn::visit::Visit<'ast> for ProfileFallbacks {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !test_gated(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if !test_gated(&item.attrs) {
            syn::visit::visit_item_impl(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, func: &'ast syn::ImplItemFn) {
        if !test_gated(&func.attrs) {
            syn::visit::visit_impl_item_fn(self, func);
        }
    }

    fn visit_item_fn(&mut self, func: &'ast syn::ItemFn) {
        if !test_gated(&func.attrs) {
            syn::visit::visit_item_fn(self, func);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let takes_fallback = matches!(
            call.method.to_string().as_str(),
            "unwrap_or" | "unwrap_or_else" | "map_or" | "map_or_else"
        );
        if takes_fallback
            && let Some(fallback) = call.args.first().map(Self::fallback_value)
            && Self::reads_profile(&call.receiver)
        {
            match fallback {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(_),
                    ..
                }) => self.literals.push(row_of(call.method.span())),
                syn::Expr::Path(path)
                    if path.path.segments.last().is_some_and(|seg| {
                        PROFILE_PLACEHOLDERS.contains(&seg.ident.to_string().as_str())
                    }) =>
                {
                    self.readers += 1;
                }
                _ => {}
            }
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

/// With no profile configured, one compliance snapshot was labelled `default`
/// by `cfgd compliance` and `unknown` by the check-in, and four reconciler
/// paths spelled `ResolvedProfile::profile_name`'s placeholder out by hand.
/// Every production read of a profile name that falls back to a word takes it
/// from one of [`PROFILE_PLACEHOLDERS`]. A fallback that names a real profile
/// to resolve says so with `// profile-fallback-ok: <why>` on its line or the
/// one above.
#[test]
fn every_absent_profile_is_spelled_by_a_named_placeholder() {
    const READERS_FLOOR: usize = 4;
    let mut literals = Vec::new();
    let mut readers = 0;
    for (_, files) in
        crate::test_helpers::production_sources_per_root(crate::test_helpers::WORKSPACE_CRATES)
    {
        for (path, production) in files {
            if production.trim().is_empty() {
                continue;
            }
            // unfloored-slice-ok: syn parses whole items; the walk skips test-gated ones.
            let body = walked_file_body(path);
            let file =
                syn::parse_file(&body).unwrap_or_else(|e| panic!("{}: {e}", source_label(path)));
            let lines: Vec<&str> = body.lines().collect();
            let mut walk = ProfileFallbacks::default();
            syn::visit::Visit::visit_file(&mut walk, &file);
            readers += walk.readers;
            literals.extend(
                walk.literals
                    .into_iter()
                    .filter(|&row| !hatched(&lines, row, PROFILE_FALLBACK_HATCH))
                    .map(|row| format!("{}:{}", source_label(path), row + 1)),
            );
        }
    }
    assert!(
        literals.is_empty(),
        "a profile read falls back to a string literal; use UNKNOWN_PROFILE or NO_PROFILE_LABEL:\n{}",
        literals.join("\n")
    );
    assert!(
        readers >= READERS_FLOOR,
        "the walk found {readers} fallbacks to a named placeholder, below the floor of \
         {READERS_FLOOR}"
    );
}

#[test]
fn every_profile_fallback_shape_is_found_and_no_other() {
    const SHAPES: &[(&str, &str, (usize, usize))] = &[
        (
            "method literal",
            r#"cfg.active_profile().unwrap_or("x")"#,
            (1, 0),
        ),
        (
            "field literal",
            r#"cfg.spec.profile.as_deref().unwrap_or("x")"#,
            (1, 0),
        ),
        (
            "closure literal",
            r#"cli.profile.as_deref().unwrap_or_else(|| "x")"#,
            (1, 0),
        ),
        (
            "owned literal",
            r#"profile.unwrap_or_else(|| "x".to_string())"#,
            (1, 0),
        ),
        ("map_or literal", r#"profile.map_or("x", |p| p)"#, (1, 0)),
        (
            "read inside a closure",
            r#"c.ok().and_then(|c| c.active_profile().ok()).unwrap_or("x")"#,
            (1, 0),
        ),
        (
            "layer field in a closure",
            r#"r.layers.last().map(|l| l.profile_name.as_str()).unwrap_or("x")"#,
            (1, 0),
        ),
        (
            "label",
            "cfg.profile.as_deref().unwrap_or(NO_PROFILE_LABEL)",
            (0, 1),
        ),
        (
            "qualified",
            "profile.unwrap_or(config::UNKNOWN_PROFILE)",
            (0, 1),
        ),
        (
            "owned label",
            "profile.unwrap_or(UNKNOWN_PROFILE).to_string()",
            (0, 1),
        ),
        (
            "another field",
            r#"cfg.name.as_deref().unwrap_or("x")"#,
            (0, 0),
        ),
        ("another constant", "profile.unwrap_or(OTHER)", (0, 0)),
        (
            "empty default",
            "cfg.spec.profile.unwrap_or_default()",
            (0, 0),
        ),
    ];
    let mut wrong = Vec::new();
    for (shape, body, expected) in SHAPES {
        let source = format!("fn f() {{ {body}; }}");
        let file = syn::parse_file(&source).unwrap_or_else(|e| panic!("{shape}: {e}"));
        let mut walk = ProfileFallbacks::default();
        syn::visit::Visit::visit_file(&mut walk, &file);
        let found = (walk.literals.len(), walk.readers);
        if found != *expected {
            wrong.push(format!("{shape}: found {found:?}, expected {expected:?}"));
        }
    }
    // The gated sources are split at the gate so this file spells no test gate whole.
    const ITEMS: &[(&str, &str, (usize, usize))] = &[
        (
            "production module",
            r#"mod t { fn f() { profile.unwrap_or("x"); } }"#,
            (1, 0),
        ),
        (
            "test module",
            concat!(
                "#[cfg",
                r#"(test)] mod t { fn f() { profile.unwrap_or("x"); } }"#
            ),
            (0, 0),
        ),
        (
            "test module reading a placeholder",
            concat!(
                "#[cfg",
                "(test)] mod t { fn f() { profile.unwrap_or(UNKNOWN_PROFILE); } }"
            ),
            (0, 0),
        ),
        (
            "test fn",
            concat!("#[cfg", r#"(test)] fn f() { profile.unwrap_or("x"); }"#),
            (0, 0),
        ),
        (
            "test impl",
            concat!(
                "#[cfg",
                r#"(test)] impl T { fn f() { profile.unwrap_or("x"); } }"#
            ),
            (0, 0),
        ),
        (
            "test method",
            concat!(
                "impl T { #[cfg",
                r#"(test)] fn f() { profile.unwrap_or("x"); } }"#
            ),
            (0, 0),
        ),
    ];
    for (shape, source, expected) in ITEMS {
        let file = syn::parse_file(source).unwrap_or_else(|e| panic!("{shape}: {e}"));
        let mut walk = ProfileFallbacks::default();
        syn::visit::Visit::visit_file(&mut walk, &file);
        let found = (walk.literals.len(), walk.readers);
        if found != *expected {
            wrong.push(format!("{shape}: found {found:?}, expected {expected:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "the profile fallback walk misreads these shapes:\n{}",
        wrong.join("\n")
    );
}
