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

/// The functions a source's `fn` lines cut it into, each paired with its
/// opening line number.
fn source_functions(body: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let opens: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| opens_function(l))
        .map(|(i, _)| i)
        .collect();
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
    // Every producer is named in FULL: the crate also holds `generate_`
    // functions for systemd units, launchd plists, SLSA provenance and device
    // ids, and a bare prefix would exempt any fixture that calls one of those
    // beside a hand-spelled tell.
    let names_a_producer = [
        "EnvPlatform",
        "primary_env_file",
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
        for (open, func) in source_functions(&body) {
            checked += 1;
            if names_a_producer.iter().any(|n| func.contains(n)) {
                continue;
            }
            let folded = crate::test_helpers::logical_source_lines(&func);
            let spells_a_name = folded
                .iter()
                .any(|(_, l)| joins.iter().any(|j| l.contains(j.as_str())));
            let spells_a_line = folded.iter().any(|(_, l)| {
                dialects.iter().any(|d| l.contains(d))
                    && owner_comments.iter().any(|c| l.contains(c.as_str()))
            });
            if spells_a_name || spells_a_line {
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
