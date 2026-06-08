//! CLI-boundary structured-error carrier.
//!
//! `render_cli_error` in `main.rs` is the sole error sink: it renders exactly
//! one human `✗` line in human mode and exactly one structured payload under
//! `-o json|yaml|...`. CLI handlers attach a [`CliErrorMeta`] to the error they
//! return so that sink can reproduce the rich structured shape (`error_kind`,
//! `name`, `extras`) that used to be emitted at each call site — without the
//! call site emitting anything itself.

/// Structured metadata attached to a CLI error so the central renderer
/// (`render_cli_error` in `main.rs`) can emit exactly one consistent payload
/// under `-o json|yaml|...` and exactly one human `✗` line — never both,
/// never neither.
///
/// `hints` are remediation lines rendered in human mode only (matching the
/// `.hint(...)` calls the old call sites attached to their error `Doc`);
/// they never appear in the structured payload.
#[derive(Debug, Clone)]
pub struct CliErrorMeta {
    pub error_kind: String,
    pub name: String,
    pub message: String,
    pub extras: serde_json::Value,
    pub hints: Vec<String>,
}

impl std::fmt::Display for CliErrorMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliErrorMeta {}

fn meta(
    name: impl Into<String>,
    error_kind: impl Into<String>,
    message: impl Into<String>,
    extras: serde_json::Value,
    hints: Vec<String>,
) -> CliErrorMeta {
    CliErrorMeta {
        error_kind: error_kind.into(),
        name: name.into(),
        message: message.into(),
        extras,
        hints,
    }
}

/// Build a CLI error with NO underlying typed error (the carrier IS the root).
/// Use where the old site returned a fresh `anyhow::bail!`/`anyhow!(...)` string
/// (exit code resolves to the generic Error code, same as before).
pub fn cli_error(
    name: impl Into<String>,
    error_kind: impl Into<String>,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> anyhow::Error {
    anyhow::Error::new(meta(name, error_kind, message, extras, Vec::new()))
}

/// Like [`cli_error`] but also carries human-mode remediation `hints` (the old
/// site attached them via `Doc::hint(...)`).
pub fn cli_error_with_hints(
    name: impl Into<String>,
    error_kind: impl Into<String>,
    message: impl Into<String>,
    extras: serde_json::Value,
    hints: Vec<String>,
) -> anyhow::Error {
    anyhow::Error::new(meta(name, error_kind, message, extras, hints))
}

/// Attach structured metadata ON TOP of an existing error via `.context(...)`,
/// preserving the underlying error in the chain so `main.rs`'s exit-code
/// downcast to `CfgdError` still resolves (e.g. parse failures → exit 4).
pub fn cli_error_ctx(
    source: anyhow::Error,
    name: impl Into<String>,
    error_kind: impl Into<String>,
    message: impl Into<String>,
    extras: serde_json::Value,
) -> anyhow::Error {
    source.context(meta(name, error_kind, message, extras, Vec::new()))
}

/// Like [`cli_error_ctx`] but also carries human-mode remediation `hints`.
pub fn cli_error_ctx_with_hints(
    source: anyhow::Error,
    name: impl Into<String>,
    error_kind: impl Into<String>,
    message: impl Into<String>,
    extras: serde_json::Value,
    hints: Vec<String>,
) -> anyhow::Error {
    source.context(meta(name, error_kind, message, extras, hints))
}

/// Emit the idempotent no-op success Doc for a `--ignore-not-found` removal of a
/// resource that does not exist. The caller has already established that the
/// named resource is absent AND the flag is set; this renders one success line
/// (exit 0) instead of the strict not-found error.
///
/// Output shape is identical across every destructive verb — only `kind`
/// (`"module"` / `"registry"` / `"source"` / `"profile"`) and `name` differ:
/// - Human: `✓ {kind} '{name}' not found; nothing to remove (--ignore-not-found)`
/// - Structured: `{"name": ..., "kind": ..., "removed": false, "reason": "not_found"}`
pub fn emit_not_found_ignored(
    printer: &cfgd_core::output::Printer,
    kind: &str,
    name: &str,
) -> anyhow::Result<()> {
    use cfgd_core::output::{Doc, Role};
    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("{kind} '{name}' not found; nothing to remove (--ignore-not-found)"),
            )
            .with_data(serde_json::json!({
                "name": name,
                "kind": kind,
                "removed": false,
                "reason": "not_found",
            })),
    );
    Ok(())
}

/// Map an [`anyhow::Error`] to an exit code by downcasting through the `CfgdError`
/// boundary. The downcast walks the whole error chain, so a `CfgdError` buried
/// under a [`CliErrorMeta`] context layer (attached by [`cli_error_ctx`]) still
/// resolves — that is how the typed exit code survives the metadata wrap. Returns
/// [`cfgd_core::exit::ExitCode::Error`] for errors that did not originate in cfgd's
/// typed domain (e.g. a bare `anyhow::anyhow!(...)` at a CLI callsite).
pub fn exit_code_for_anyhow(err: &anyhow::Error) -> cfgd_core::exit::ExitCode {
    err.downcast_ref::<cfgd_core::errors::CfgdError>()
        .map(cfgd_core::exit::exit_code_for_error)
        .unwrap_or(cfgd_core::exit::ExitCode::Error)
}

/// The SOLE CLI error sink. Renders exactly one failure representation and returns
/// the exit code. CLI handlers never emit their own error output — they return an
/// error (optionally carrying [`CliErrorMeta`] for a rich structured payload), and
/// this function renders it once: in human mode exactly one `✗` line (plus any
/// hints), in structured mode (`-o json|yaml|...`) exactly one payload, ALWAYS —
/// never silent, even for a plain `?`-propagated error (a fallback payload is
/// synthesized). Shared by the normal CLI dispatch (`main.rs`) and the kubectl
/// plugin entry (`plugin::plugin_main`) so there is one sink, not two.
pub fn render_cli_error(
    printer: &cfgd_core::output::Printer,
    err: &anyhow::Error,
) -> cfgd_core::exit::ExitCode {
    let doc = match err.downcast_ref::<CliErrorMeta>() {
        Some(m) => {
            // The handler attached a structured payload; reproduce its exact shape.
            // `with_data` (inside error_doc) replaces Doc-derived JSON in structured
            // mode, so the hints below render in human mode only — never in the payload.
            let mut doc = cfgd_core::output::error_doc(
                &m.name,
                &m.error_kind,
                cfgd_core::output::collapse_to_subject_line(&m.message),
                m.extras.clone(),
            );
            for hint in &m.hints {
                doc = doc.hint(hint.clone());
            }
            doc
        }
        None => {
            // A plain `?`-propagated error with no attached meta. Synthesize a payload
            // so structured consumers are never left silent on failure.
            let message = cfgd_core::output::collapse_to_subject_line(err);
            cfgd_core::output::error_doc(
                "",
                "error",
                message.clone(),
                serde_json::json!({ "message": message }),
            )
        }
    };
    printer.emit(doc);
    let code = exit_code_for_anyhow(err);
    // The hint goes to the same stream (stderr) as the error above.
    if code == cfgd_core::exit::ExitCode::NoConfig {
        printer.hint("run `cfgd init` to create a config, or pass --config <path>");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_error_round_trips_meta_via_downcast() {
        let err = cli_error(
            "mymod",
            "already_exists",
            "Module 'mymod' already exists",
            serde_json::json!({ "path": "/tmp/mymod" }),
        );
        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("CliErrorMeta resolves from the carrier");
        assert_eq!(meta.error_kind, "already_exists");
        assert_eq!(meta.name, "mymod");
        assert_eq!(meta.message, "Module 'mymod' already exists");
        assert_eq!(meta.extras, serde_json::json!({ "path": "/tmp/mymod" }));
        assert!(
            meta.hints.is_empty(),
            "no-hint constructor leaves hints empty"
        );
    }

    #[test]
    fn cli_error_with_hints_round_trips_hints_via_downcast() {
        let err = cli_error_with_hints(
            "nope",
            "not_found",
            "Module 'nope' not found",
            serde_json::json!({ "available": ["a", "b"] }),
            vec!["Available modules: a, b".to_string()],
        );
        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("CliErrorMeta resolves from the carrier");
        assert_eq!(meta.error_kind, "not_found");
        assert_eq!(meta.hints, vec!["Available modules: a, b".to_string()]);
    }

    #[test]
    fn cli_error_ctx_preserves_both_meta_and_underlying_cfgd_error() {
        // The exit-code-survival guarantee: wrapping a typed CfgdError with
        // CliErrorMeta must leave BOTH downcastable, because main.rs derives
        // the human/structured payload from CliErrorMeta but the exit code
        // from the inner CfgdError (here ConfigInvalid = 4).
        let inner = cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
            message: "bad config".to_string(),
        });
        let expected_code = cfgd_core::exit::exit_code_for_error(&inner);
        let err = cli_error_ctx(
            inner.into(),
            "myconfig",
            "parse_failed",
            "config parse failed",
            serde_json::json!({ "path": "/tmp/cfgd.yaml" }),
        );

        let meta = err
            .downcast_ref::<CliErrorMeta>()
            .expect("CliErrorMeta resolves from the context layer");
        assert_eq!(meta.error_kind, "parse_failed");
        assert_eq!(meta.name, "myconfig");

        let cfgd_err = err
            .downcast_ref::<cfgd_core::errors::CfgdError>()
            .expect("inner CfgdError still resolves through the chain");
        assert_eq!(
            cfgd_core::exit::exit_code_for_error(cfgd_err),
            expected_code,
            "exit code must survive the context wrap"
        );
        assert_eq!(expected_code, cfgd_core::exit::ExitCode::ConfigInvalid);
    }

    #[test]
    fn render_cli_error_hints_cfgd_init_on_missing_config() {
        // A NoConfig failure must point the user at `cfgd init`, not just print "not found".
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let err: anyhow::Error =
            cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::NotFound {
                path: "/home/u/.config/cfgd/cfgd.yaml".into(),
            })
            .into();
        let code = render_cli_error(&printer, &err);
        printer.flush();
        assert_eq!(code, cfgd_core::exit::ExitCode::NoConfig);
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("cfgd init"),
            "expected remediation naming `cfgd init`, got: {out:?}"
        );
    }

    #[test]
    fn exit_code_for_anyhow_falls_back_to_error_for_opaque_anyhow_errors() {
        // anyhow::anyhow! produces an error that doesn't downcast to
        // CfgdError; the helper must return ExitCode::Error.
        let err = anyhow::anyhow!("an opaque CLI-boundary error");
        let code = exit_code_for_anyhow(&err);
        assert_eq!(code, cfgd_core::exit::ExitCode::Error);
    }

    #[test]
    fn exit_code_for_anyhow_propagates_cfgd_error_exit_code_through_downcast() {
        // Errors that downcast to CfgdError should be routed through
        // exit_code_for_error so the typed-domain semantics are preserved.
        let cfgd_err =
            cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                message: "invalid config".to_string(),
            });
        let expected = cfgd_core::exit::exit_code_for_error(&cfgd_err);
        let anyhow_err: anyhow::Error = cfgd_err.into();
        let actual = exit_code_for_anyhow(&anyhow_err);
        assert_eq!(actual, expected);
    }

    #[test]
    fn render_cli_error_human_emits_exactly_one_fail_line() {
        // The double-print bug: a handler that emitted its own ✗ AND the central
        // sink rendering a second one. The sink is now the SOLE emitter — exactly
        // one ✗ line, never two.
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let err = cli_error(
            "mymod",
            "already_exists",
            "Module 'mymod' already exists",
            serde_json::json!({ "path": "/tmp/mymod" }),
        );
        render_cli_error(&printer, &err);
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert_eq!(
            out.matches('✗').count(),
            1,
            "expected exactly one ✗ line, got: {out:?}"
        );
        assert!(
            out.contains("already exists"),
            "fail line text, got: {out:?}"
        );
    }

    #[test]
    fn render_cli_error_human_renders_attached_hints() {
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        let err = cli_error_with_hints(
            "nope",
            "not_found",
            "Module 'nope' not found",
            serde_json::json!({ "available": ["a", "b"] }),
            vec!["Available modules: a, b".to_string()],
        );
        render_cli_error(&printer, &err);
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert_eq!(out.matches('✗').count(), 1, "one ✗ line, got: {out:?}");
        assert!(
            out.contains("Available modules: a, b"),
            "hint must render in human mode, got: {out:?}"
        );
    }

    #[test]
    fn render_cli_error_structured_emits_one_payload_no_human_line() {
        // Structured mode: exactly one JSON object, the rich payload, and NO stray
        // human ✗ line beside it. Parsing the whole buffer as a single JSON value
        // fails if a second object was concatenated — that is the "exactly one" check.
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
        let err = cli_error_with_hints(
            "mymod",
            "already_exists",
            "Module 'mymod' already exists",
            serde_json::json!({ "path": "/tmp/mymod" }),
            vec!["a hint".to_string()],
        );
        render_cli_error(&printer, &err);
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert!(
            !out.contains('✗'),
            "no human ✗ line in structured mode: {out:?}"
        );
        let v: serde_json::Value =
            serde_json::from_str(out.trim()).expect("buffer is exactly one JSON value");
        assert_eq!(v["error"], "already_exists");
        assert_eq!(v["name"], "mymod");
        assert_eq!(v["path"], "/tmp/mymod");
        assert!(v.get("hint").is_none(), "hints stay human-only: {v}");
        assert!(v.get("a hint").is_none(), "hints not in payload: {v}");
    }

    #[test]
    fn render_cli_error_structured_synthesizes_payload_for_propagated_error() {
        // A plain `?`-propagated error with no attached meta must NEVER be silent
        // in structured mode — the sink synthesizes a fallback payload carrying
        // the message.
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
        let err = anyhow::anyhow!("some opaque failure");
        render_cli_error(&printer, &err);
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert!(
            !out.trim().is_empty(),
            "structured failure must not be silent"
        );
        let v: serde_json::Value =
            serde_json::from_str(out.trim()).expect("buffer is exactly one JSON value");
        assert_eq!(v["error"], "error");
        assert_eq!(v["message"], "some opaque failure");
    }

    #[test]
    fn render_cli_error_preserves_exit_code_through_cli_error_ctx() {
        // cli_error_ctx wraps a typed CfgdError; the human/structured payload comes
        // from the CliErrorMeta but the exit code must still resolve from the inner
        // CfgdError walked out of the chain.
        let (printer, _buf) =
            cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
        let inner = cfgd_core::errors::CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
            message: "bad config".to_string(),
        });
        let expected = cfgd_core::exit::exit_code_for_error(&inner);
        let err = cli_error_ctx(
            inner.into(),
            "myconfig",
            "parse_failed",
            "config parse failed",
            serde_json::json!({}),
        );
        let code = render_cli_error(&printer, &err);
        assert_eq!(code, expected);
        assert_eq!(code, cfgd_core::exit::ExitCode::ConfigInvalid);
    }
}
