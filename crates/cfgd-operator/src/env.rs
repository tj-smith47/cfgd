//! Env-var parsing helpers for cfgd-operator startup.
//!
//! Extracted from main.rs so the env→config translation is unit-testable
//! without spinning up the operator's tokio runtime or a kube client.
//!
//! Every helper here logs a structured warning on bad input and falls back
//! to the documented default — the operator MUST keep starting on
//! garbled env values so a typo doesn't crash the control plane.

/// Parse a port number from an env var, falling back to `default` on
/// missing or unparseable values. Emits a `tracing::warn!` with the bad
/// value and the parse error when the var is set but invalid.
pub fn parse_port_env(var: &str, default: u16) -> u16 {
    match std::env::var(var) {
        Ok(val) => val.parse().unwrap_or_else(|e| {
            tracing::warn!(
                env_var = %var,
                value = %val,
                error = %e,
                default = default,
                "invalid port value, using default"
            );
            default
        }),
        Err(_) => default,
    }
}

/// Parse a boolean-shaped env var. Accepts `true` or `1` as true; anything
/// else (or unset) is false. Matches the convention used across cfgd
/// (`LEADER_ELECTION_ENABLED=1`, `DEVICE_GATEWAY_ENABLED=true`).
pub fn parse_bool_env(var: &str) -> bool {
    std::env::var(var)
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// Read an env var, returning the value or `default` when unset.
pub fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

/// Parse an env var as a `u32`, falling back to `default` on missing or
/// unparseable values. Used for retention windows / size knobs that don't
/// need the port-specific warning shape.
pub fn parse_u32_env(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::test_helpers::with_test_env_var;
    use serial_test::serial;

    // --- parse_port_env ---

    #[test]
    #[serial]
    fn parse_port_env_returns_default_when_unset() {
        with_test_env_var("CFGD_TEST_PORT_UNSET", None, || {
            assert_eq!(parse_port_env("CFGD_TEST_PORT_UNSET", 8081), 8081);
        });
    }

    #[test]
    #[serial]
    fn parse_port_env_parses_valid_port() {
        with_test_env_var("CFGD_TEST_PORT_VALID", Some("9090"), || {
            assert_eq!(parse_port_env("CFGD_TEST_PORT_VALID", 8081), 9090);
        });
    }

    #[test]
    #[serial]
    fn parse_port_env_falls_back_on_garbage() {
        // Operator startup contract: a typo in the deployment YAML must
        // NOT crash the process; we log + fall through to the default.
        with_test_env_var("CFGD_TEST_PORT_GARBAGE", Some("not-a-number"), || {
            assert_eq!(parse_port_env("CFGD_TEST_PORT_GARBAGE", 8081), 8081);
        });
    }

    #[test]
    #[serial]
    fn parse_port_env_falls_back_on_negative() {
        // u16 can't hold negative — confirm the parse-error branch handles it.
        with_test_env_var("CFGD_TEST_PORT_NEG", Some("-1"), || {
            assert_eq!(parse_port_env("CFGD_TEST_PORT_NEG", 8081), 8081);
        });
    }

    #[test]
    #[serial]
    fn parse_port_env_falls_back_on_overflow() {
        with_test_env_var("CFGD_TEST_PORT_OVER", Some("99999"), || {
            assert_eq!(parse_port_env("CFGD_TEST_PORT_OVER", 8081), 8081);
        });
    }

    #[test]
    #[serial]
    fn parse_port_env_accepts_zero() {
        // 0 is a valid port (means "kernel assigns") — accept it rather
        // than silently fall back; the caller decides whether 0 is sane
        // for their service.
        with_test_env_var("CFGD_TEST_PORT_ZERO", Some("0"), || {
            assert_eq!(parse_port_env("CFGD_TEST_PORT_ZERO", 8081), 0);
        });
    }

    // --- parse_bool_env ---

    #[test]
    #[serial]
    fn parse_bool_env_returns_false_when_unset() {
        with_test_env_var("CFGD_TEST_BOOL_UNSET", None, || {
            assert!(!parse_bool_env("CFGD_TEST_BOOL_UNSET"));
        });
    }

    #[test]
    #[serial]
    fn parse_bool_env_accepts_true_literal() {
        with_test_env_var("CFGD_TEST_BOOL_T", Some("true"), || {
            assert!(parse_bool_env("CFGD_TEST_BOOL_T"));
        });
    }

    #[test]
    #[serial]
    fn parse_bool_env_accepts_numeric_one() {
        // `1` is what bash conditionals and Helm chart values often emit.
        with_test_env_var("CFGD_TEST_BOOL_1", Some("1"), || {
            assert!(parse_bool_env("CFGD_TEST_BOOL_1"));
        });
    }

    #[test]
    #[serial]
    fn parse_bool_env_rejects_yes_on_off_etc() {
        // We do NOT accept "yes", "on", "y", "T", "True" — only the two
        // exact tokens. Pinning this rejects accidental loosening that
        // would silently change deployment behavior.
        for val in ["yes", "on", "y", "T", "True", "TRUE", "enable"] {
            with_test_env_var("CFGD_TEST_BOOL_X", Some(val), || {
                assert!(
                    !parse_bool_env("CFGD_TEST_BOOL_X"),
                    "{val} must NOT parse as true"
                );
            });
        }
    }

    #[test]
    #[serial]
    fn parse_bool_env_rejects_empty_string() {
        with_test_env_var("CFGD_TEST_BOOL_EMPTY", Some(""), || {
            assert!(!parse_bool_env("CFGD_TEST_BOOL_EMPTY"));
        });
    }

    // --- env_or ---

    #[test]
    #[serial]
    fn env_or_returns_default_when_unset() {
        with_test_env_var("CFGD_TEST_S_UNSET", None, || {
            assert_eq!(env_or("CFGD_TEST_S_UNSET", "fallback"), "fallback");
        });
    }

    #[test]
    #[serial]
    fn env_or_returns_value_when_set() {
        with_test_env_var("CFGD_TEST_S_SET", Some("explicit"), || {
            assert_eq!(env_or("CFGD_TEST_S_SET", "fallback"), "explicit");
        });
    }

    #[test]
    #[serial]
    fn env_or_returns_empty_string_when_set_to_empty() {
        // Setting a var to "" is a deliberate caller action, not unset.
        // Returning "" lets callers distinguish "user explicitly cleared"
        // from "not configured."
        with_test_env_var("CFGD_TEST_S_EMPTY", Some(""), || {
            assert_eq!(env_or("CFGD_TEST_S_EMPTY", "fallback"), "");
        });
    }

    // --- parse_u32_env ---

    #[test]
    #[serial]
    fn parse_u32_env_returns_default_when_unset() {
        with_test_env_var("CFGD_TEST_U32_UNSET", None, || {
            assert_eq!(parse_u32_env("CFGD_TEST_U32_UNSET", 90), 90);
        });
    }

    #[test]
    #[serial]
    fn parse_u32_env_parses_valid_value() {
        with_test_env_var("CFGD_TEST_U32_VALID", Some("365"), || {
            assert_eq!(parse_u32_env("CFGD_TEST_U32_VALID", 90), 365);
        });
    }

    #[test]
    #[serial]
    fn parse_u32_env_falls_back_on_garbage() {
        with_test_env_var("CFGD_TEST_U32_GARBAGE", Some("forever"), || {
            assert_eq!(parse_u32_env("CFGD_TEST_U32_GARBAGE", 90), 90);
        });
    }
}
