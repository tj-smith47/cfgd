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

/// A tracing subscriber wired straight to `std::io::stderr` writes past the
/// live region: every event lands on the stream indicatif is repainting, and
/// the last paint of whatever bar was on screen is left stranded behind it.
/// `output::LiveTracingWriter` is the writer every subscriber that writes a
/// plain-text line takes, because it routes each event through the printer's
/// `MultiProgress` and folds it on the way (the one exception is cfgd-csi's
/// JSON log line, whose serializer is its own sanitizer — see the fence
/// below).
///
/// Hatch, read like every sibling gate's (`tracing-ok:`, `native-ok:`,
/// `spawn-blocking-ok:`): mark the call line or the line above it with
/// `// stderr-writer-ok: <why>`. A capture writer legitimately NAMED for stderr
/// is the shape that needs it, and a gate with no hatch is one somebody
/// eventually silences by widening the writer's name instead.
#[test]
fn no_subscriber_writes_straight_to_stderr() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line_no in stderr_writer_offenders(&body) {
            let line = body.lines().nth(line_no).unwrap_or_default();
            offenders.push(format!(
                "{}:{}: {}",
                path.display(),
                line_no + 1,
                line.trim()
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "a subscriber writing straight to stderr strands the live region; take \
         output::LiveTracingWriter instead (or mark the line \
         `// stderr-writer-ok: <why>`):\n{}",
        offenders.join("\n")
    );
}

/// A `tracing_subscriber::fmt` subscriber that names no writer takes the
/// builder's default, which is the raw process stream — and the raw stream is
/// a terminal writer that passes no renderer, so every event lands on it
/// unfolded. That is how two server binaries kept writing module names, device
/// hostnames and remote error text straight at a terminal while the fence
/// above (which only reads `with_writer` arguments) saw nothing to judge.
///
/// Hatch: `// default-writer-ok: <why>` on the construction line or the line
/// above. The shape that needs it is a formatter whose own serializer is the
/// sanitizer — a JSON log line, where folding would emit `\xNN` inside a
/// string and cost the consumer a parseable payload.
#[test]
fn no_subscriber_takes_the_default_writer() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line_no in default_writer_offenders(&body) {
            let line = body.lines().nth(line_no).unwrap_or_default();
            offenders.push(format!(
                "{}:{}: {}",
                path.display(),
                line_no + 1,
                line.trim()
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "a subscriber with no writer takes the raw process stream unfolded; \
         name output::LiveTracingWriter (or mark the line \
         `// default-writer-ok: <why>`):\n{}",
        offenders.join("\n")
    );
}

/// Line numbers (0-based) of every `tracing_subscriber::fmt` construction in
/// `source` whose statement names no writer. The statement is read to its
/// terminating `;`, because rustfmt splits a builder chain across lines and a
/// line-scoped read would judge the constructor alone.
fn default_writer_offenders(source: &str) -> Vec<usize> {
    const MAX_LINES: usize = 16;
    let lines: Vec<&str> = source.lines().collect();
    let mut offenders = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let code = code_half(line);
        if !code.contains("tracing_subscriber::fmt()")
            && !code.contains("tracing_subscriber::fmt::layer()")
        {
            continue;
        }
        let mut names_writer = false;
        for candidate in lines[i..].iter().take(MAX_LINES) {
            let chain = code_half(candidate);
            if chain.contains("with_writer(") || chain.contains("with_test_writer(") {
                names_writer = true;
                break;
            }
            if chain.contains(';') {
                break;
            }
        }
        if !names_writer && !hatched(&lines, i, "default-writer-ok:") {
            offenders.push(i);
        }
    }
    offenders
}

/// The same obligation the stderr fence carries: a predicate that recognizes
/// one spelling is a fence somebody walks around. Both constructors, the
/// split-chain read, the hatch, and the wirings that are already correct.
#[test]
fn the_default_writer_fence_recognizes_every_spelling() {
    for offender in [
        "    tracing_subscriber::fmt().json().init();",
        "    let l = tracing_subscriber::fmt::layer();",
        // A chain split over lines, with the writer named nowhere in it.
        "    tracing_subscriber::fmt()\n        .with_target(false)\n        .without_time()\n        .init();",
        // A later statement's writer does not cover this one.
        "    tracing_subscriber::fmt().init();\n    other.with_writer(x);",
        // A marker with no reason after it is not a hatch.
        "    // default-writer-ok:\n    tracing_subscriber::fmt().init();",
    ] {
        assert!(
            !default_writer_offenders(offender).is_empty(),
            "the fence must recognize this wiring: {offender:?}"
        );
    }
    for allowed in [
        "    tracing_subscriber::fmt()\n        .with_writer(tracing_writer.clone())\n        .init();",
        "    let l = tracing_subscriber::fmt::layer()\n        .with_writer(LiveTracingWriter::new());",
        "    tracing_subscriber::fmt().with_test_writer().finish();",
        "    // default-writer-ok: the JSON serializer is the sanitizer here\n    tracing_subscriber::fmt().json().init();",
        // The constructor named inside a string literal is a name, not a call.
        "    let s = \"tracing_subscriber::fmt()\";",
    ] {
        assert!(
            default_writer_offenders(allowed).is_empty(),
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

/// Line numbers (0-based) of every `with_writer(` in `source` handed the raw
/// stderr stream. Any spelling reaches it: a path (`std::io::stderr`), an
/// import (`use std::io::stderr;` then bare `stderr`), or a closure around
/// either — the first cut pinned two fully-qualified forms and let the rest
/// through.
fn stderr_writer_offenders(source: &str) -> Vec<usize> {
    let lines: Vec<&str> = source.lines().collect();
    let mut offenders = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let code = code_half(line);
        let Some(pos) = code.find("with_writer(") else {
            continue;
        };
        let arg = writer_argument(&lines, i, pos + "with_writer(".len());
        let squashed: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
        if squashed.contains("stderr") && !hatched(&lines, i, "stderr-writer-ok:") {
            offenders.push(i);
        }
    }
    offenders
}

/// The fence above is only as wide as its predicate, and a predicate that
/// recognizes one spelling of a mistake is a fence somebody walks around
/// without noticing. Every way the workspace could reach the raw stream is
/// pinned here, together with the wirings that are correct and the hatch.
#[test]
fn the_stderr_fence_recognizes_every_spelling() {
    for offender in [
        "        .with_writer(std::io::stderr)",
        "        .with_writer(io::stderr)",
        "        .with_writer(stderr)",
        "        .with_writer(|| std::io::stderr())",
        "        .with_writer(|| io::stderr()).with_ansi(false)",
        // rustfmt splits a long call; the argument is still the raw stream.
        "        .with_writer(\n            std::io::stderr,\n        )",
        // A `//` inside a string literal is not a comment: neither a URL
        // before the call nor one inside its arguments hides the wiring.
        "        connect(\"https://x.test//hook\").with_writer(io::stderr)",
        "        .with_writer(tee(\"https://x.test//hook\", io::stderr))",
        // The marker inside a string literal is not a hatch — on the call
        // line or on the line above it.
        "        .with_writer(io::stderr).named(\"// stderr-writer-ok: no\")",
        "        let m = \"// stderr-writer-ok: no\";\n        .with_writer(io::stderr)",
    ] {
        assert!(
            !stderr_writer_offenders(offender).is_empty(),
            "the fence must recognize this wiring: {offender:?}"
        );
    }
    for allowed in [
        "        .with_writer(tracing_writer.clone())",
        "        .with_writer(LiveTracingWriter::new())",
        "/// Never wire a subscriber to `with_writer` at stderr again.",
        "        let mut err = io::stderr().lock();",
        // The argument ends at its own close paren: a later, unrelated stderr
        // read is not this call's.
        "        .with_writer(writer.clone())\n        let e = io::stderr();",
        // The hatch, on the call line and on the line above it.
        "        .with_writer(stderr_capture.clone()) // stderr-writer-ok: test capture, not the stream",
        "        // stderr-writer-ok: test capture, not the stream\n        .with_writer(stderr_capture.clone())",
        // `stderr` inside a string literal is a name, not the stream.
        "        .with_writer(file_writer(\"stderr.log\"))",
        "        .with_writer(rotating(r\"logs\\stderr\", writer.clone()))",
    ] {
        assert!(
            stderr_writer_offenders(allowed).is_empty(),
            "the fence must not flag this: {allowed:?}"
        );
    }
    // A marker with no reason after it is not a hatch.
    assert!(
        !stderr_writer_offenders("        .with_writer(io::stderr) // stderr-writer-ok:")
            .is_empty(),
        "a marker with no reason must not exempt the call"
    );
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
