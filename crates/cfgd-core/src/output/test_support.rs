//! Test-only helpers for the output module. Gated to `#[cfg(test)]` because
//! every consumer mutates process-global state (`console::set_colors_enabled`
//! and friends) and must pair with `serial_test::serial`.

/// Panic-safe RAII guard for the process-global
/// `console::set_colors_enabled` and `console::set_colors_enabled_stderr`
/// flags. Captures both prior states on construction and restores them on
/// drop so a panicking assertion inside a `#[serial]` test does not leak a
/// `colors_enabled=false` state into the next test in the serial chain.
///
/// Both flags are tracked because callers that disable colors (e.g.
/// `Printer::with_format` under structured output, NO_COLOR, or TERM=dumb)
/// toggle both — a stdout-only guard would silently leak the stderr flag.
pub(crate) struct ColorsEnabledGuard {
    prior_stdout: bool,
    prior_stderr: bool,
}

impl ColorsEnabledGuard {
    pub(crate) fn set(enabled: bool) -> Self {
        let prior_stdout = console::colors_enabled();
        let prior_stderr = console::colors_enabled_stderr();
        console::set_colors_enabled(enabled);
        console::set_colors_enabled_stderr(enabled);
        Self {
            prior_stdout,
            prior_stderr,
        }
    }
}

impl Drop for ColorsEnabledGuard {
    fn drop(&mut self) {
        console::set_colors_enabled(self.prior_stdout);
        console::set_colors_enabled_stderr(self.prior_stderr);
    }
}
