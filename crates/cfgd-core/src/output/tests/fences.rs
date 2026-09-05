//! Source-shaped fences: invariants that no runtime assertion can hold,
//! because each is about code that must not exist.

use std::path::{Path, PathBuf};

/// Workspace root. `CARGO_MANIFEST_DIR` = `<root>/crates/cfgd-core`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Every `.rs` file under every crate's `src/`.
fn workspace_rust_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![workspace_root().join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    assert!(!out.is_empty(), "found no sources under crates/");
    out
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
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
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
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
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
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
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

/// Blank the bodies of string and char literals on one line, byte-for-byte
/// (each literal-interior byte becomes a space, quotes stay), so byte
/// positions found on the blanked line index the raw line exactly. Handles
/// `"…"` with escapes, `r"…"`/`r#"…"#` raw strings, and char literals —
/// discriminated from lifetimes by closing-quote proximity, the same test
/// `audit.sh`'s `strip_strings` uses. Line-scoped by construction: a literal
/// that spans lines has only its first line blanked, and its interior lines
/// are read as code — the same bound every fence in this file already lives
/// with.
fn blank_string_literals(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = bytes.to_vec();
    let is_ident = |b: u8| b == b'_' || b.is_ascii_alphanumeric();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        out[j] = b' ';
                        out[j + 1] = b' ';
                        j += 2;
                    } else {
                        out[j] = b' ';
                        j += 1;
                    }
                }
                i = j + 1;
            }
            b'r' if i == 0 || !is_ident(bytes[i - 1]) => {
                let mut hashes = 0;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    let mut k = j + 1;
                    while k < bytes.len() {
                        if bytes[k] == b'"'
                            && bytes[k + 1..].len() >= hashes
                            && bytes[k + 1..k + 1 + hashes].iter().all(|&b| b == b'#')
                        {
                            break;
                        }
                        out[k] = b' ';
                        k += 1;
                    }
                    i = (k + 1 + hashes).min(bytes.len());
                } else {
                    i += 1;
                }
            }
            b'\'' => {
                // A char literal's body is one escape or exactly one char —
                // a single ASCII byte, or 2-4 non-ASCII bytes — and a
                // lifetime never closes. Requiring that shape (not mere
                // closing-quote proximity) keeps `<'a>('x')` from blanking
                // the paren between two quotes. Escapes scan a bounded
                // window so `'\u{2764}'` still blanks.
                let close = if bytes.get(i + 1) == Some(&b'\\') {
                    (i + 3..bytes.len().min(i + 13)).find(|&k| bytes[k] == b'\'')
                } else {
                    (i + 2..bytes.len().min(i + 6))
                        .find(|&k| bytes[k] == b'\'')
                        .filter(|&k| k == i + 2 || bytes[i + 1..k].iter().all(|&b| b >= 0x80))
                };
                match close {
                    Some(k) => {
                        for b in &mut out[i + 1..k] {
                            *b = b' ';
                        }
                        i = k + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    // Every replaced byte is ASCII space and quote/escape bytes are ASCII, so
    // the buffer is valid UTF-8 by construction.
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}

/// The code half of a line: everything before a comment-opening `//`, judged
/// on the literal-blanked line so a `//` inside a string (a URL in an
/// argument) cannot truncate the code half, and so parens or the word
/// `stderr` inside a literal cannot join a call's argument text.
fn code_half(line: &str) -> String {
    let blanked = blank_string_literals(line);
    match blanked.find("//") {
        Some(pos) => blanked[..pos].to_string(),
        None => blanked,
    }
}

/// Whether the construction on `lines[at]` is exempted by a `// <marker> <why>`
/// comment on its own line or the line above, with a reason written after it. The comment start is located on the
/// literal-blanked line and the marker read from the true comment, so a line
/// cannot claim the hatch by carrying the marker inside a string literal.
fn hatched(lines: &[&str], at: usize, marker: &str) -> bool {
    let marked = |line: &str| {
        blank_string_literals(line)
            .find("//")
            .and_then(|pos| line[pos + 2..].split_once(marker))
            .is_some_and(|(_, why)| !why.trim().is_empty())
    };
    marked(lines[at]) || (at > 0 && marked(lines[at - 1]))
}

/// The argument text of the `with_writer(` opened at `from` on `lines[at]`, up
/// to its matching close paren — across lines, because rustfmt splits a long
/// call and a line-scoped read would see `with_writer(` and `std::io::stderr`
/// as two unrelated lines. Bounded at a few lines so a stray unbalanced paren
/// cannot swallow the rest of the file and pair the call with an unrelated
/// `stderr` far below it.
fn writer_argument(lines: &[&str], at: usize, from: usize) -> String {
    const MAX_LINES: usize = 6;
    let mut depth = 1usize;
    let mut arg = String::new();
    for (offset, line) in lines[at..].iter().take(MAX_LINES).enumerate() {
        let code = code_half(line);
        let chars = if offset == 0 {
            // `from` was found on the same literal-blanked rendering this
            // call re-derives, and blanking is byte-length preserving, so the
            // index lands where it was found.
            &code[from.min(code.len())..]
        } else {
            code.as_str()
        };
        for c in chars.chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return arg;
                    }
                }
                _ => {}
            }
            arg.push(c);
        }
    }
    arg
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
/// drain unable to re-enter `write_line` — they hold `&mut RenderState`, not a
/// sink and not the lock. A collector that regained either would deadlock or
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
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
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
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
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
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !reads_a_summary_field(line) {
                continue;
            }
            let hatched = line.contains("decision-summary-ok:")
                || i.checked_sub(1)
                    .is_some_and(|p| lines[p].contains("decision-summary-ok:"));
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
    let body = std::fs::read_to_string(&pending).unwrap_or_else(|e| panic!("{pending:?}: {e}"));
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
/// watching, and its report belongs on the terminal.
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
    let mut offenders = Vec::new();
    let mut seen = 0usize;
    for path in workspace_rust_files() {
        if !path.components().any(|c| c.as_os_str() == "daemon")
            || path.ends_with(Path::new("tests.rs"))
        {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        // `service/` installs and uninstalls the unit from a one-shot command
        // the user is watching, so those DO report through the printer. The
        // loop itself has no terminal to report to.
        if !path.components().any(|c| c.as_os_str() == "service") {
            for (n, line) in body.lines().enumerate() {
                if let Some(call) = PRINTER_LINES.iter().find(|c| line.contains(**c)) {
                    offenders.push(format!(
                        "{}:{}: `{call}` — the reconcile loop speaks through \
                         `tracing`, whose events carry the timestamp and level \
                         a journal reader reads",
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

/// Whether a source line opens a function item, whatever qualifiers stand
/// between its indent and `fn` (`pub(super) async fn`, `const unsafe extern
/// "C" fn`, …).
///
/// The cost of a missed opener is silent: the function's body folds into the
/// PRECEDING slice, so it is judged under another function's exemptions and
/// reported, if at all, at that function's line. An enumerated list of
/// qualifier orderings is how `pub(super) async fn` was missed, so this
/// consumes qualifiers one at a time instead and accepts any order — a
/// superset of the grammar, which for a recognizer only errs toward opening
/// a slice too eagerly.
fn opens_function(line: &str) -> bool {
    let mut t = line.trim_start();
    loop {
        if t.starts_with("fn ") {
            return true;
        }
        if let Some(scope) = t.strip_prefix("pub(") {
            let Some((_, tail)) = scope.split_once(')') else {
                return false;
            };
            t = tail.trim_start();
        } else if let Some(abi) = t.strip_prefix("extern ").map(str::trim_start) {
            t = match abi.strip_prefix('"').and_then(|a| a.split_once('"')) {
                Some((_, tail)) => tail.trim_start(),
                None => abi,
            };
        } else if let Some(rest) = ["pub ", "default ", "const ", "async ", "unsafe "]
            .iter()
            .find_map(|q| t.strip_prefix(q))
        {
            t = rest.trim_start();
        } else {
            return false;
        }
    }
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

/// The walk's masking state across physical lines: whatever a line sits
/// inside of that makes its text NOT source — a raw literal, an ordinary
/// `"…"` literal (a `\`-continued one included: the escape arm keeps
/// `in_plain` latched across the break), or a block comment.
#[derive(Default)]
struct LineMask {
    raw_hashes: Option<usize>,
    in_plain: bool,
    comment_depth: usize,
    /// Byte offset at which the line just advanced across stopped being
    /// masked, when it began masked and ended one of its states.
    resumed_at: Option<usize>,
}

impl LineMask {
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

    /// Whether the NEXT line begins inside a literal or comment.
    fn masked(&self) -> bool {
        self.raw_hashes.is_some() || self.in_plain || self.comment_depth > 0
    }

    /// Advance across one physical line: raw literals by the fold layer's own
    /// open/close arithmetic, ordinary literals escape-aware (`\"` does not
    /// close one, `\\` does not escape what follows), char literals whole
    /// (`'"'` must not open plain-string state, while a lifetime's lone `'`
    /// is left alone), `//` cutting the line and `/* … */` nesting across
    /// lines.
    fn advance(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut i = 0;
        self.resumed_at = None;
        while i < bytes.len() {
            if let Some(open) = self.raw_hashes {
                if crate::test_helpers::raw_string_closes(bytes, i, open) {
                    self.raw_hashes = None;
                    i += 1 + open;
                    self.resumed_at.get_or_insert(i);
                } else {
                    i += 1;
                }
                continue;
            }
            if self.in_plain {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        self.in_plain = false;
                        i += 1;
                        self.resumed_at.get_or_insert(i);
                    }
                    _ => i += 1,
                }
                continue;
            }
            if self.comment_depth > 0 {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    self.comment_depth -= 1;
                    i += 2;
                    if self.comment_depth == 0 {
                        self.resumed_at.get_or_insert(i);
                    }
                } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    self.comment_depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            match bytes[i] {
                b'"' => {
                    self.in_plain = true;
                    i += 1;
                }
                b'\'' => {
                    if bytes.get(i + 1) == Some(&b'\\') {
                        // The escaped byte sits at i + 2, so the closing-quote
                        // search starts past it: searched from i + 2, an
                        // escaped quote (`'\''`) is its own first hit and the
                        // scan lands on the escaped byte instead of past the
                        // literal.
                        let after_escape = (i + 3).min(bytes.len());
                        i = bytes[after_escape..]
                            .iter()
                            .position(|b| *b == b'\'')
                            .map_or(bytes.len(), |p| after_escape + p + 1);
                    } else if bytes.get(i + 2) == Some(&b'\'') {
                        i += 3;
                    } else {
                        i += 1;
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => return,
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    self.comment_depth += 1;
                    i += 2;
                }
                _ => {
                    if let Some(open) = crate::test_helpers::raw_string_open(bytes, i) {
                        self.raw_hashes = Some(open);
                        i += 2 + open;
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }
}

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
fn source_functions(body: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut mask = LineMask::default();
    let mut opens: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let began_masked = mask.masked();
        let rest = mask.source_remainder(line);
        // On the line that CLOSES a literal, what precedes the declaration is
        // the tail of the statement the literal belonged to (`"#;`), so the
        // declaration does not start the span. It is the FIRST terminator that
        // ends that tail: a later one belongs to the declaration's own body,
        // and looking past it would hide the declaration it introduces. Only a
        // closing line looks past anything at all.
        let decl = match rest.find(';') {
            Some(at) if began_masked => rest.get(at + 1..).unwrap_or(""),
            _ => rest,
        };
        if opens_function(decl) {
            opens.push(i);
        }
    }
    opens
        .iter()
        .zip(opens.iter().skip(1).chain(std::iter::once(&lines.len())))
        .map(|(a, b)| (a + 1, lines[*a..*b].join("\n")))
        .collect()
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
/// FUNCTION-scoped, because cfgd-core holds two whole populations for which a
/// literal is correct and no per-site hatch should be needed: a fixture
/// exercising the generator NAMES it (an explicit `EnvPlatform`, a
/// `generate_*` call, `env_targets`), and one naming a path production writes
/// VERBATIM names that too (`WriteEnvFile`, `plan_env_with_home`,
/// `ScriptShell` — where the shell, not the host, picks the dialect). A
/// function that spells a tell and names none of them is hand-spelling.
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
        "generate_env_file_content",
        "generate_fish_env_content",
        "generate_powershell_env_content",
        "generate_environment_d_content",
    ];
    let core_src = workspace_root().join("crates/cfgd-core/src");
    let mut offenders = Vec::new();
    let mut checked = 0usize;
    for path in workspace_rust_files() {
        // This file spells every tell in order to hunt for it.
        if !path.starts_with(&core_src) || path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        // The dialect-alone tells' one path-based hatch: the file that OWNS
        // the dialect (`env_engine.rs`, home of `path_line`/`fold_path_line`)
        // pins the raw assignment syntax through helpers with no `generate_*`
        // name of their own to match on.
        let is_env_engine_owner = path.ends_with(Path::new("reconciler/env_engine.rs"));
        for (open, func) in source_functions(&body) {
            checked += 1;
            let names_producer = names_a_producer.iter().any(|n| names_identifier(&func, n));
            let calls_generator =
                is_env_engine_owner || generator_calls.iter().any(|n| names_identifier(&func, n));
            if names_producer && calls_generator {
                continue;
            }
            let folded = crate::test_helpers::logical_source_lines(&func);
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
                    "{}:{}: {}",
                    path.display(),
                    open,
                    body.lines().nth(open - 1).unwrap_or("").trim()
                ));
            }
        }
    }
    assert!(
        checked > 2000,
        "the walk no longer reaches cfgd-core's functions — it read {checked}"
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
    let funcs = source_functions(body);
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
    let funcs = source_functions(&body);
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
    let funcs = source_functions(body);
    assert_eq!(funcs.len(), 2, "{funcs:?}");
    assert_eq!(funcs[0].0, 1);
    assert!(
        funcs[0].1.contains("masked_by_the_open_string")
            && funcs[0].1.contains("masked_by_the_block_comment"),
        "every masked line stays inside the real function's slice: {funcs:?}"
    );
    assert_eq!(funcs[1].0, 12, "the sibling after the masks still opens");
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
    "EditorGuard::set",
    "ProbePath::containing",
    "install_named_path_shim",
    "with_test_env_var",
    "ToolShim::install",
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
/// counted on [`code_half`], so one inside a literal or a trailing comment
/// does not move the depth.
///
/// The scan runs to the real close with no line ceiling. A ceiling returns a
/// slice that merely LOOKS like a function, and every tell below the cut is
/// then invisible to a walk that believes it read the whole body — the silent
/// direction. A scan that instead runs off the end of the file has desynced
/// (a brace inside a multi-line raw literal is the shape that does it), so it
/// panics naming the declaration it could not close.
fn function_source(lines: &[&str], open: usize) -> String {
    let mut mask = LineMask::default();
    let mut depth = 0i32;
    let mut opened = false;
    for (offset, line) in lines[open..].iter().enumerate() {
        // The declaration's own line is never inside a literal or comment —
        // `source_functions` masks before it opens a slice — so a mask started
        // here reads the rest of the body correctly, and a brace inside a
        // multi-line raw literal moves no depth.
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
        "the brace scan reached the end of the file without closing the \
         declaration opened at line {}: {}",
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
/// A floor sits AT the count rather than under it, so a file losing one
/// reacher fails here instead of drifting silently — the failure mode a
/// blinded needle takes. A re-calibration REPLACES a row with what the walk
/// now finds; it never lowers one to make a red walk green, because a count
/// that fell is the finding.
const SERIAL_FLOORS: &[(&str, usize)] = &[
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
    ("crates/cfgd-core/src/output/printer.rs", 5),
    ("crates/cfgd-core/src/output/render_doc.rs", 3),
    ("crates/cfgd-core/src/output/tests/color_gate.rs", 1),
    ("crates/cfgd-core/src/output/tests/hyperlinks.rs", 5),
    ("crates/cfgd-core/src/output/tests/themes_raw.rs", 4),
    ("crates/cfgd-core/src/output/theme.rs", 11),
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
    ("crates/cfgd-core/src/upgrade/tests.rs", 43),
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
    ("crates/cfgd/src/cli/profile/tests.rs", 3),
    ("crates/cfgd/src/cli/registry.rs", 1),
    ("crates/cfgd/src/cli/status.rs", 5),
    ("crates/cfgd/src/cli/tests.rs", 85),
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
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !body.contains("#[test]") && !body.contains("#[tokio::test") {
            continue;
        }
        files_read += 1;
        let lines: Vec<&str> = body.lines().collect();

        // Every declaration in the file, by name: its sources (a name can be
        // declared more than once, in sibling modules or impl blocks), whether
        // any of them mutates, and whether it is reachable by a bare call.
        let mut sources: std::collections::BTreeMap<String, Vec<(usize, String)>> =
            std::collections::BTreeMap::new();
        let mut free: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
        for (open, slice) in source_functions(&body) {
            let Some(name) = declared_fn_name(&slice) else {
                continue;
            };
            let name = name.to_string();
            let source = function_source(&lines, open - 1);
            free.entry(name.clone())
                .and_modify(|f| *f &= !takes_a_receiver(&slice))
                .or_insert(!takes_a_receiver(&slice));
            sources.entry(name).or_default().push((open, source));
        }
        // A roster entry earns its place by COUNTING the source lines that CALL
        // it — inside the declarations this walk cut, never the file's own
        // `use` block, which names a helper without reaching it.
        // `test_helpers.rs` is where the helpers live, so its mentions prove
        // nothing.
        //
        // The fold does not depend on the entry, so it happens once for the
        // file's declarations rather than once per entry per declaration.
        if !path.ends_with(Path::new("test_helpers.rs")) {
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

        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
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
                    *per_file.entry(relative.clone()).or_default() += 1;
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
    let own = std::fs::read_to_string(&own_path).unwrap_or_else(|e| panic!("{own_path:?}: {e}"));
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
/// Scoped to `test_helpers.rs`, the workspace's only `test-helpers`-gated
/// module. Run over every source instead, the same derivation adds exactly
/// one public reacher — CLI startup code that writes `XDG_CONFIG_HOME` before
/// any thread exists — which is a production concern with its own safety
/// argument, not a helper a test calls.
///
/// `// env-mutator-ok: <why>` exempts a helper that writes no environment
/// variable of its own, or whose every call site another roster entry already
/// counts.
#[test]
fn every_env_mutating_test_helper_is_named_in_the_mutator_roster() {
    let root = workspace_root();
    let mut derived: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut files_read = 0usize;
    let mut offenders = Vec::new();

    for path in workspace_rust_files() {
        if path.file_name() != Some(std::ffi::OsStr::new("test_helpers.rs")) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        files_read += 1;
        // The trailing test module exercises the helpers, so its own tests
        // reach every seed and would be derived as helpers themselves.
        let body = crate::test_helpers::production_slice(&raw);
        let lines: Vec<&str> = body.lines().collect();
        let owners = impl_owners(&lines);

        let mut names: Vec<String> = Vec::new();
        let mut qualified: Vec<String> = Vec::new();
        let mut opens: Vec<usize> = Vec::new();
        let mut sources: Vec<String> = Vec::new();
        let mut exported: Vec<bool> = Vec::new();
        for (open, slice) in source_functions(&body) {
            let Some(name) = declared_fn_name(&slice) else {
                continue;
            };
            let owner = owners[open - 1].clone();
            qualified.push(match &owner {
                Some(ty) => format!("{ty}::{name}"),
                None => name.to_string(),
            });
            names.push(name.to_string());
            exported.push(lines[open - 1].trim_start().starts_with("pub"));
            sources.push(function_source(&lines, open - 1));
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

        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
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
    let source = function_source(&lines, 0);
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
    let cut = source_functions(body);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 5],
        "a declaration inside the literal opened a slice, or the one after \
         its close did not: {opens:?}"
    );

    let closing = concat!(
        "fn holder() {\n",
        "    let banner = r#\"\n",
        "\"#; fn sibling() { let x = 1; }\n",
        "fn after() {}\n"
    );
    let cut = source_functions(closing);
    let opens: Vec<usize> = cut.iter().map(|(open, _)| *open).collect();
    assert_eq!(
        opens,
        vec![1, 3, 4],
        "the declaration on the literal's closing line opened no slice: {opens:?}"
    );
    assert!(
        cut[1].1.contains("fn sibling"),
        "the slice opened past the declaration's own terminator: {:?}",
        cut[1].1
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
