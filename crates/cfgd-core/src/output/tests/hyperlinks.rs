//! The OSC 8 hyperlink slot: what the terminal is asked, what the theme
//! stamps, and what the kv renderer emits for a linked value.

use serial_test::serial;

use crate::output::{
    HYPERLINK_DIRECT_VARS, HYPERLINK_MIN_VTE_VERSION, HYPERLINK_TERM_PROGRAMS,
    HYPERLINK_TERMINAL_VARS, KvPair, Printer, Theme, Verbosity, terminal_supports_hyperlinks,
};
use crate::test_helpers::{EnvVarGuard, captured_text};

/// Clear every variable the detection reads, so a positive case is the one
/// variable the test set and the negative case is a terminal that named itself
/// not at all — including under a suite invoked from a hyperlink-capable
/// terminal of the developer's own, or from inside tmux.
fn cleared() -> Vec<EnvVarGuard> {
    HYPERLINK_DIRECT_VARS
        .iter()
        .chain(HYPERLINK_TERMINAL_VARS)
        .map(|v| EnvVarGuard::unset(v))
        .collect()
}

/// The clearing list and the predicate are one population, checked against the
/// predicate's OWN SOURCE rather than against a second hand-written list: a
/// direct read added to the function (`TERM`, `TERMINAL_EMULATOR`) and to
/// neither table would leave the negative case below asserting against the
/// developer's own terminal — the exact failure that pin exists to prevent.
///
/// Two halves, because a read the predicate DELEGATES is as unclearable as one
/// it names. The scanned scope is the predicate plus, transitively, every
/// helper it calls by name, so a read extracted into a sibling function is
/// still seen while an unrelated read elsewhere in the module stays none of
/// this table's business; and the predicate's own body may name no path but
/// `std::env::var`/`var_os` and a turbofish, so a helper moved OUT of the file
/// — the one shape the scan cannot follow — fails there instead.
#[test]
fn every_variable_the_detection_reads_is_one_a_test_can_clear() {
    const PREDICATE: &str = "terminal_supports_hyperlinks";
    const READS: [&str; 2] = ["std::env::var(\"", "std::env::var_os(\""];
    // A turbofish names a TYPE, not a module that could read the environment.
    const REACHABLE_PATHS: [&str; 3] = ["std::env::var(", "std::env::var_os(", "parse::<"];
    // A prelude constructor names no body to follow, so it is not a call that
    // could carry a read out of this scan's reach.
    const PRELUDE_CALLS: [&str; 4] = ["Some", "Ok", "Err", "None"];

    // A trailing comment is prose, not a path or a read; split on the SPACE
    // before it so a `://` inside a literal survives.
    fn code(line: &str) -> &str {
        let head = line.split(" //").next().unwrap_or_default().trim_start();
        if head.starts_with("//") { "" } else { head }
    }

    fn fn_body(src: &str, name: &str) -> Option<(usize, usize)> {
        let at = src.find(&format!("fn {name}("))?;
        let end = src[at..].find("\n}\n").map_or(src.len(), |off| at + off);
        Some((at, end))
    }

    fn calls_in(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in src.lines() {
            let line = code(line);
            for (at, _) in line.match_indices('(') {
                let head = &line[..at];
                let from = head
                    .rfind(|c: char| !c.is_alphanumeric() && c != '_')
                    .map_or(0, |i| i + 1);
                let name = &head[from..];
                if name.is_empty() || matches!(head[..from].chars().next_back(), Some('.' | ':')) {
                    continue;
                }
                out.push(name.to_string());
            }
        }
        out
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/output/mod.rs");
    let body = std::fs::read_to_string(&src).unwrap_or_default();

    let mut scopes = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut unfollowable = Vec::new();
    let mut queue = vec![PREDICATE.to_string()];
    while let Some(name) = queue.pop() {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let Some((start, end)) = fn_body(&body, &name) else {
            if !PRELUDE_CALLS.contains(&name.as_str()) {
                unfollowable.push(name);
            }
            continue;
        };
        queue.extend(calls_in(&body[start..end]));
        scopes.push((start, end));
    }
    assert!(
        !scopes.is_empty(),
        "the predicate is no longer at {}",
        src.display()
    );
    assert!(
        unfollowable.is_empty(),
        "every function the predicate calls must have its body at {}, so an environment \
         read cannot hide in a helper this scan cannot follow; a helper imported from \
         elsewhere belongs beside the predicate, and a prelude constructor in \
         PRELUDE_CALLS:\n{}",
        src.display(),
        unfollowable.join("\n")
    );

    let mut unclearable = Vec::new();
    for (start, end) in &scopes {
        let first = body[..*start].matches('\n').count();
        for (n, line) in body[*start..*end].lines().enumerate() {
            let line = code(line);
            for call in READS {
                let Some(at) = line.find(call) else {
                    continue;
                };
                let name = line[at + call.len()..]
                    .split('"')
                    .next()
                    .unwrap_or_default();
                if !HYPERLINK_DIRECT_VARS.contains(&name) {
                    unclearable.push(format!("{}:{}: {name}", src.display(), first + n + 1));
                }
            }
        }
    }
    assert!(
        unclearable.is_empty(),
        "every variable the detection reads by name belongs in HYPERLINK_DIRECT_VARS, \
         so a test can clear it:\n{}",
        unclearable.join("\n")
    );

    let Some((start, end)) = fn_body(&body, PREDICATE) else {
        panic!("the predicate is no longer at {}", src.display());
    };
    let mut unreachable_paths = Vec::new();
    for line in body[start..end].lines() {
        let line = code(line);
        for (at, _) in line.match_indices("::") {
            let from = line[..at].rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != ':');
            let path = &line[from.map_or(0, |i| i + 1)..];
            if !REACHABLE_PATHS.iter().any(|ok| path.starts_with(ok)) {
                unreachable_paths.push(format!(
                    "{}: {line}",
                    path.split('(').next().unwrap_or(path)
                ));
            }
        }
    }
    assert!(
        unreachable_paths.is_empty(),
        "the predicate may name no path but `std::env::var`, `std::env::var_os` and a \
         turbofish, so no environment read can hide behind one in a module this scan \
         cannot follow; a genuinely inert new path belongs in REACHABLE_PATHS:\n{}",
        unreachable_paths.join("\n")
    );

    assert!(
        !HYPERLINK_DIRECT_VARS
            .iter()
            .any(|v| HYPERLINK_TERMINAL_VARS.contains(v)),
        "the two tables partition the population; neither repeats the other"
    );
}

#[test]
#[serial]
fn a_term_program_that_names_a_hyperlink_terminal_is_detected() {
    let _cleared = cleared();
    for program in HYPERLINK_TERM_PROGRAMS {
        let _set = EnvVarGuard::set("TERM_PROGRAM", program);
        assert!(
            terminal_supports_hyperlinks(),
            "TERM_PROGRAM={program} names an OSC 8 terminal"
        );
    }
}

#[test]
#[serial]
fn a_terminal_naming_itself_by_its_own_variable_is_detected() {
    let _cleared = cleared();
    for var in HYPERLINK_TERMINAL_VARS {
        let _set = EnvVarGuard::set(var, "1");
        assert!(
            terminal_supports_hyperlinks(),
            "{var} names an OSC 8 terminal"
        );
    }
}

#[test]
#[serial]
fn vte_renders_hyperlinks_from_its_first_supporting_release() {
    let _cleared = cleared();
    {
        let _at = EnvVarGuard::set("VTE_VERSION", &HYPERLINK_MIN_VTE_VERSION.to_string());
        assert!(terminal_supports_hyperlinks(), "VTE 0.50 renders OSC 8");
    }
    let _below = EnvVarGuard::set("VTE_VERSION", &(HYPERLINK_MIN_VTE_VERSION - 1).to_string());
    assert!(
        !terminal_supports_hyperlinks(),
        "the release below the first supporting one does not"
    );
}

#[test]
#[serial]
fn a_terminal_that_names_itself_not_at_all_gets_no_hyperlink() {
    let _cleared = cleared();
    assert!(
        !terminal_supports_hyperlinks(),
        "an unidentified terminal reads the plain URL instead"
    );
}

/// A multiplexer's panes inherit the outer terminal's identification, and an
/// old tmux (or any `screen`) swallows the escape rather than forwarding it —
/// leaving the reader neither a link nor a URL. The plain URL is the answer
/// whatever the terminal underneath claims.
#[test]
#[serial]
fn a_multiplexed_session_gets_the_plain_url_whatever_the_outer_terminal_is() {
    for mux in ["TMUX", "STY"] {
        let _cleared = cleared();
        // The outer terminal's own identification, inherited by the pane.
        let _outer = EnvVarGuard::set("TERM_PROGRAM", "iTerm.app");
        let _inner = EnvVarGuard::set("WT_SESSION", "1");
        assert!(
            terminal_supports_hyperlinks(),
            "{mux} unset, the outer terminal answers for itself"
        );
        let _in_mux = EnvVarGuard::set(mux, "/tmp/session,1,0");
        assert!(
            !terminal_supports_hyperlinks(),
            "{mux} set withholds the escape whatever the pane inherited"
        );
    }
}

/// A hyperlink is an escape sequence, so the colour decision governs it: a
/// printer that may not emit colour may not emit one either.
#[test]
fn a_colourless_theme_cannot_be_stamped_with_hyperlinks() {
    assert!(
        !Theme::default()
            .with_colors(false)
            .with_hyperlinks(true)
            .hyperlinks(),
        "colour off withholds the escape"
    );
    assert!(
        Theme::default()
            .with_colors(true)
            .with_hyperlinks(true)
            .hyperlinks()
    );
    // And in the other order: a stamped theme that later loses colour loses
    // the escape with it, so no call order can leave the two disagreeing.
    assert!(
        !Theme::default()
            .with_colors(true)
            .with_hyperlinks(true)
            .with_colors(false)
            .hyperlinks(),
        "colour withdrawn withdraws the escape"
    );
}

fn linked_row(theme: Theme) -> String {
    let (printer, buf) = Printer::for_test_with_theme_colored(theme, Verbosity::Normal);
    printer.kv_rows([KvPair::linked(
        "Docs",
        "docs/spec/module.md#fields",
        "https://example.test/docs/spec/module.md#fields",
    )]);
    printer.flush();
    // raw-capture-ok: the claim IS the OSC 8 escape — captured_text strips it
    buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[test]
fn a_linked_value_under_a_hyperlink_theme_wraps_its_text_in_osc_8() {
    let out = linked_row(Theme::default().with_colors(true).with_hyperlinks(true));
    assert!(
        out.contains(
            "\x1b]8;;https://example.test/docs/spec/module.md#fields\x1b\\\
             docs/spec/module.md#fields\x1b]8;;\x1b\\"
        ),
        "the row opens the URL behind the short path, got: {out:?}"
    );
}

/// Everywhere else the URL IS the value: a repo-relative path is something no
/// terminal auto-links and no reader can paste into a browser.
#[test]
fn a_linked_value_without_hyperlinks_prints_the_url_itself() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    printer.kv_rows([KvPair::linked(
        "Docs",
        "docs/spec/module.md#fields",
        "https://example.test/docs/spec/module.md#fields",
    )]);
    printer.flush();
    let out = captured_text(&buf);
    assert!(
        out.contains("Docs  https://example.test/docs/spec/module.md#fields"),
        "the value is the URL, got: {out:?}"
    );
    assert!(
        !out.contains('\x1b'),
        "a capture emits no escape, got: {out:?}"
    );
}

/// A physical-row break inside an OSC 8 escape would put its URL bytes on the
/// screen and leave the link half-open. The width accounting skips an OSC
/// string whole, exactly as it skips a CSI.
#[test]
fn wrapping_a_linked_value_never_splits_its_escape() {
    let url = "https://example.test/docs/spec/module.md#fields";
    let text = "docs/spec/module.md#a-very-long-anchor-name";
    let body = crate::output::osc8_hyperlink(url, text);
    let rows = crate::output::renderer::wrap::wrap_segment(&body, "Docs  ", "      ", Some(30));
    assert!(rows.len() > 1, "the row is narrow enough to wrap: {rows:?}");
    let opener = format!("\x1b]8;;{url}\x1b\\");
    assert!(
        rows[0].contains(&opener),
        "the opening escape stays whole on one row, got: {rows:?}"
    );
    for row in &rows {
        let visible = crate::output::strip_ansi(row);
        assert!(
            !visible.contains("8;;") && !visible.contains("example.test"),
            "no byte of the escape reaches the screen, got: {visible:?}"
        );
    }
}

/// An escape occupies no columns, so a linked value breaks at exactly the
/// column its own text would have broken at. Counted as visible width, an OSC
/// string's payload retreats every break by the length of its URL: the row
/// still rendered whole — the break landed past the escape either way — but it
/// carried six fewer characters than the terminal had room for, and a longer
/// URL moves the break further each time.
#[test]
fn wrapping_a_linked_value_measures_only_what_the_terminal_shows() {
    let text = "docs/spec/module.md#a-very-long-anchor-name";
    let cut = |body: &str| {
        crate::output::renderer::wrap::wrap_segment(body, "Docs  ", "      ", Some(30))
            .iter()
            .map(|row| crate::output::strip_ansi(row))
            .collect::<Vec<_>>()
    };
    let plain = cut(text);
    for url in [
        "https://example.test/docs/spec/module.md#fields",
        "https://example.test/docs/spec/module.md#a-much-much-longer-anchor-still",
    ] {
        assert_eq!(
            cut(&crate::output::osc8_hyperlink(url, text)),
            plain,
            "the link's own length must not move the break"
        );
    }
}
