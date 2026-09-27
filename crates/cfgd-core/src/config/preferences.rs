//! `spec.preferences.<domain>`: a ranked candidate list per domain, resolved
//! against the running session the way `spec.packages[].prefer` resolves
//! against this machine. Filter to what is admitted and installed, walk in the
//! author's order, first match wins.

use serde::{Deserialize, Serialize};

use super::EnvVar;
use crate::errors::{ConfigError, Result};
use crate::platform::{DisplayServer, Session};

/// `spec.preferences`: one ranked candidate list per domain.
///
/// A domain joins as a sibling field; nothing about the block's shape changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferencesSpec {
    /// Clipboard mechanisms in preference order, most-wanted first. cfgd picks
    /// the first one the running session can reach and whose tool is
    /// installed, and exports it as `CFGD_CLIPBOARD`. Candidates:
    /// `wl-clipboard`, `xclip`, `xsel`, `pbcopy`, `clip.exe`, `osc52`.
    #[serde(default)]
    pub clipboard: Vec<String>,
}

impl PreferencesSpec {
    /// Fold `overlay` over this one, returning whether it claimed any domain.
    ///
    /// The LAST layer to declare a domain wins that domain's whole ranking: a
    /// ranking is one ordered statement, and a union of two rankings is an
    /// order no layer wrote. `envScope` resolves the same way.
    pub fn absorb(&mut self, overlay: &PreferencesSpec) -> bool {
        let PreferencesSpec { clipboard } = overlay;
        let mut claimed = false;
        if !clipboard.is_empty() {
            self.clipboard = clipboard.clone();
            claimed = true;
        }
        claimed
    }
}

/// One candidate: the spelling an author writes, the binary whose presence on
/// `PATH` means it is installed, and the sessions it can actually reach.
struct Candidate {
    name: &'static str,
    binary: Option<&'static str>,
    admits: fn(&Session) -> bool,
}

const CLIPBOARD: &[Candidate] = &[
    Candidate {
        name: "wl-clipboard",
        binary: Some("wl-copy"),
        admits: |s| s.display == Some(DisplayServer::Wayland),
    },
    // XWayland serves X clients on a Wayland session, so any display admits.
    Candidate {
        name: "xclip",
        binary: Some("xclip"),
        admits: |s| s.display.is_some(),
    },
    Candidate {
        name: "xsel",
        binary: Some("xsel"),
        admits: |s| s.display.is_some(),
    },
    // `pbcopy` over SSH writes the server's clipboard, which the operator at
    // the other end of the connection never sees.
    Candidate {
        name: "pbcopy",
        binary: Some("pbcopy"),
        admits: |s| !s.ssh,
    },
    Candidate {
        name: "clip.exe",
        binary: Some("clip.exe"),
        admits: |s| s.wsl,
    },
    // OSC 52 is the terminal's own escape: no binary, and reachable from a
    // session with no display at all.
    Candidate {
        name: "osc52",
        binary: None,
        admits: |_| true,
    },
];

/// One domain of `prefs`: its field name, the author's ranking, the candidate
/// table the ranking is read against, and the var its winner exports under.
type Domain<'a> = (
    &'static str,
    &'a [String],
    &'static [Candidate],
    &'static str,
);

/// Every domain `prefs` declares, the one table both resolution and validation
/// read.
fn domains(prefs: &PreferencesSpec) -> [Domain<'_>; 1] {
    // No `..`: a domain added to `PreferencesSpec` fails to compile here until
    // someone says what it resolves to.
    let PreferencesSpec { clipboard } = prefs;
    [("clipboard", clipboard, CLIPBOARD, "CFGD_CLIPBOARD")]
}

/// The env vars this host's session resolves `prefs` to.
///
/// One `CFGD_<DOMAIN>` per domain whose ranking yields a winner. A domain that
/// yields none contributes nothing, so a machine that can reach no candidate
/// exports no var for it.
pub fn resolved_env(prefs: &PreferencesSpec, session: &Session) -> Vec<EnvVar> {
    domains(prefs)
        .into_iter()
        .filter_map(|(_, ranked, table, var)| {
            first_admitted(ranked, table, session).map(|pick| EnvVar {
                name: var.to_string(),
                value: pick.to_string(),
                platforms: Vec::new(),
            })
        })
        .collect()
}

/// The first candidate in the author's order that the running session admits
/// and whose tool is on `PATH`. An empty ranking probes nothing.
fn first_admitted<'a>(
    ranked: &'a [String],
    table: &[Candidate],
    session: &Session,
) -> Option<&'a str> {
    ranked.iter().map(String::as_str).find(|name| {
        table.iter().any(|c| {
            c.name == *name
                && (c.admits)(session)
                && c.binary.is_none_or(|b| crate::command_path(b).is_some())
        })
    })
}

/// Refuse a candidate no domain knows, naming the set that would have worked.
///
/// A typo would otherwise resolve to nothing at all and read as "cfgd ignored
/// my preference" on a machine where every listed tool is installed.
pub fn validate_preferences(prefs: &PreferencesSpec) -> Result<()> {
    for (domain, ranked, table, _) in domains(prefs) {
        for name in ranked {
            if !table.iter().any(|c| c.name == name.as_str()) {
                let known: Vec<&str> = table.iter().map(|c| c.name).collect();
                return Err(ConfigError::Invalid {
                    message: format!(
                        "spec.preferences.{domain}: unknown candidate '{name}' (known: {})",
                        known.join(", ")
                    ),
                }
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::DisplayServer;
    #[cfg(unix)]
    use crate::test_helpers::ProbePath;

    fn session(display: Option<DisplayServer>, ssh: bool, wsl: bool) -> Session {
        Session { display, ssh, wsl }
    }

    fn ranked(names: &[&str]) -> PreferencesSpec {
        PreferencesSpec {
            clipboard: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn a_display_less_ssh_session_falls_past_every_installed_x_tool_to_osc52() {
        let _guard = crate::test_helpers::path_env_mutation_guard();
        let _path = ProbePath::containing(&["xclip", "wl-copy"]);
        let picked = resolved_env(
            &ranked(&["wl-clipboard", "xclip", "osc52"]),
            &session(None, true, false),
        );
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "CFGD_CLIPBOARD");
        assert_eq!(picked[0].value, "osc52");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn wl_clipboard_wins_on_wayland_when_its_binary_is_installed() {
        let _guard = crate::test_helpers::path_env_mutation_guard();
        let _path = ProbePath::containing(&["wl-copy", "xclip"]);
        let picked = resolved_env(
            &ranked(&["wl-clipboard", "xclip", "osc52"]),
            &session(Some(DisplayServer::Wayland), false, false),
        );
        assert_eq!(picked[0].value, "wl-clipboard");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn an_admitted_candidate_whose_binary_is_absent_is_passed_over() {
        let _guard = crate::test_helpers::path_env_mutation_guard();
        let _dirs = crate::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let _path = ProbePath::containing(&["xclip"]);
        let picked = resolved_env(
            &ranked(&["wl-clipboard", "xclip"]),
            &session(Some(DisplayServer::Wayland), false, false),
        );
        // `xclip` is admitted on Wayland through XWayland and IS installed.
        assert_eq!(picked[0].value, "xclip");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn pbcopy_is_refused_over_ssh_because_it_writes_the_servers_clipboard() {
        let _guard = crate::test_helpers::path_env_mutation_guard();
        let _path = ProbePath::containing(&["pbcopy"]);
        assert!(resolved_env(&ranked(&["pbcopy"]), &session(None, true, false)).is_empty());
        assert_eq!(
            resolved_env(&ranked(&["pbcopy"]), &session(None, false, false))[0].value,
            "pbcopy"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn clip_exe_is_the_wsl_candidate() {
        let _guard = crate::test_helpers::path_env_mutation_guard();
        let _path = ProbePath::containing(&["clip.exe"]);
        assert_eq!(
            resolved_env(&ranked(&["clip.exe", "osc52"]), &session(None, false, true))[0].value,
            "clip.exe"
        );
        assert_eq!(
            resolved_env(
                &ranked(&["clip.exe", "osc52"]),
                &session(None, false, false)
            )[0]
            .value,
            "osc52"
        );
    }

    #[test]
    fn an_undeclared_domain_resolves_to_no_env_var() {
        assert!(resolved_env(&PreferencesSpec::default(), &session(None, false, false)).is_empty());
    }

    #[test]
    fn an_unknown_candidate_names_itself_and_the_known_set() {
        let err = validate_preferences(&ranked(&["xclipp"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("xclipp"), "names the typo: {err}");
        assert!(
            err.contains("wl-clipboard") && err.contains("osc52"),
            "names the known set: {err}"
        );
    }

    #[test]
    fn a_later_layer_replaces_a_domains_whole_ranking() {
        let mut base = ranked(&["xclip", "osc52"]);
        assert!(base.absorb(&ranked(&["osc52"])));
        assert_eq!(base.clipboard, vec!["osc52".to_string()]);
        assert!(!base.absorb(&PreferencesSpec::default()));
        assert_eq!(base.clipboard, vec!["osc52".to_string()]);
    }
}
