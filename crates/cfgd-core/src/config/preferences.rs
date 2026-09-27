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

/// One domain of `prefs`: its field name, the author's ranking (empty when the
/// domain is undeclared) and the candidate table the ranking is read against.
type Domain<'a> = (&'static str, &'a [String], &'static [Candidate]);

/// How many domains `PreferencesSpec` offers; `domains` returns one entry each.
const DOMAIN_COUNT: usize = 1;

/// Every domain `PreferencesSpec` offers, paired with `prefs`'s ranking for it,
/// the one table resolution, validation and the chain fold all read.
fn domains(prefs: &PreferencesSpec) -> [Domain<'_>; DOMAIN_COUNT] {
    // No `..`: a domain added to `PreferencesSpec` fails to compile here until
    // someone says what it resolves to.
    let PreferencesSpec { clipboard } = prefs;
    [("clipboard", clipboard, CLIPBOARD)]
}

/// The var a domain's winner exports under: `CFGD_` and the upper-cased
/// domain name, so a domain's field name and its var cannot disagree.
fn env_var_name(domain: &str) -> String {
    let mut name = String::with_capacity("CFGD_".len() + domain.len());
    name.push_str("CFGD_");
    name.extend(domain.chars().map(|c| c.to_ascii_uppercase()));
    name
}

/// The env vars this host's session resolves `prefs` to.
///
/// One `CFGD_<DOMAIN>` per domain whose ranking yields a winner. A domain that
/// yields none contributes nothing, so a machine that can reach no candidate
/// exports no var for it.
pub fn resolved_env(prefs: &PreferencesSpec, session: &Session) -> Vec<EnvVar> {
    let mut chain = ChainPreferences::new();
    chain.absorb(prefs, ());
    chain
        .resolve(session)
        .into_iter()
        .map(|(var, ())| var)
        .collect()
}

/// A layer chain's preferences: per domain, the ranking of the LAST layer
/// that declared it and that layer's owner `O`.
///
/// A ranking is one ordered statement, and a union of two rankings is an
/// order no layer wrote, so the last declaring layer wins the whole list,
/// the way `envScope` resolves. Ownership is per domain: a layer ranking only
/// one domain leaves every other domain with the layer that ranked it.
/// Rankings stay borrowed from their layers until `resolve`, which copies only
/// the winner.
pub(crate) struct ChainPreferences<'a, O> {
    slots: [Option<(&'a [String], O)>; DOMAIN_COUNT],
}

impl<'a, O: Copy> ChainPreferences<'a, O> {
    /// A chain no layer has declared a domain in.
    pub(crate) fn new() -> Self {
        Self {
            slots: [None; DOMAIN_COUNT],
        }
    }

    /// Fold one layer's declared rankings, in chain order, under `owner`.
    pub(crate) fn absorb(&mut self, layer: &'a PreferencesSpec, owner: O) {
        claim_declared(&mut self.slots, domains(layer).map(|(_, r, _)| r), owner);
    }

    /// Each domain's winner for `session` as its env var, beside the owner of
    /// the ranking that produced it. A domain with no winner is absent.
    pub(crate) fn resolve(&self, session: &Session) -> Vec<(EnvVar, O)> {
        let undeclared = PreferencesSpec::default();
        domains(&undeclared)
            .into_iter()
            .zip(&self.slots)
            .filter_map(|((domain, _, table), slot)| {
                let (ranked, owner) = (*slot)?;
                let pick = first_admitted(ranked, table, session)?;
                let var = EnvVar {
                    name: env_var_name(domain),
                    value: pick.to_string(),
                    platforms: Vec::new(),
                };
                Some((var, owner))
            })
            .collect()
    }
}

/// Point each slot a layer's non-empty ranking declares at that ranking and
/// `owner`; a slot the layer leaves empty keeps what an earlier layer set.
fn claim_declared<'a, O: Copy, const N: usize>(
    slots: &mut [Option<(&'a [String], O)>; N],
    rankings: [&'a [String]; N],
    owner: O,
) {
    for (slot, ranked) in slots.iter_mut().zip(rankings) {
        if !ranked.is_empty() {
            *slot = Some((ranked, owner));
        }
    }
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
    for (domain, ranked, table) in domains(prefs) {
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
        let (base, child, silent) = (
            ranked(&["xclip", "osc52"]),
            ranked(&["osc52"]),
            PreferencesSpec::default(),
        );
        let mut chain = ChainPreferences::new();
        chain.absorb(&base, "base");
        chain.absorb(&child, "child");
        chain.absorb(&silent, "silent");
        let picked = chain.resolve(&session(None, false, false));
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].0.value, "osc52");
        assert_eq!(
            picked[0].1, "child",
            "a layer declaring nothing leaves the owner alone"
        );
    }

    /// Two domains run through the same fold the chain uses: a layer ranking
    /// only the second domain takes that domain alone, and the first stays
    /// with the layer that ranked it.
    #[test]
    fn a_layer_ranking_one_domain_takes_ownership_of_that_domain_alone() {
        let (a1, a2, b2) = (
            vec!["xclip".to_string()],
            vec!["firefox".to_string()],
            vec!["chromium".to_string()],
        );
        let mut slots: [Option<(&[String], &str)>; 2] = [None, None];
        claim_declared(&mut slots, [a1.as_slice(), a2.as_slice()], "parent");
        claim_declared(&mut slots, [&[], b2.as_slice()], "child");
        assert_eq!(
            slots[0],
            Some((a1.as_slice(), "parent")),
            "the domain the child left unranked stays with the parent"
        );
        assert_eq!(
            slots[1],
            Some((b2.as_slice(), "child")),
            "the child owns what it ranked"
        );
    }

    /// Every domain the schema offers must carry a candidate table and a
    /// documented section. A domain added to `PreferencesSpec` with neither
    /// accepts the author's ranking and then resolves nothing.
    #[test]
    fn every_preferences_domain_the_schema_offers_has_a_resolver_and_a_doc_section() {
        let value =
            serde_json::to_value(PreferencesSpec::default()).expect("PreferencesSpec serializes");
        let keys: Vec<String> = value
            .as_object()
            .expect("a mapping")
            .keys()
            .cloned()
            .collect();
        assert!(
            !keys.is_empty(),
            "PreferencesSpec must offer at least one domain"
        );

        let doc =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/spec/profile.md");
        let body = crate::test_helpers::walked_file_body(&doc);
        // A whole-line match: `### spec.preferences` is also a prefix of every
        // domain's own heading, which a substring search would accept for it.
        let has_heading = |h: &str| body.lines().any(|l| l.trim_end() == h);
        assert!(
            has_heading("### spec.preferences"),
            "docs/spec/profile.md must document spec.preferences"
        );
        let undeclared = PreferencesSpec::default();
        let offered = domains(&undeclared);
        for key in &keys {
            assert!(
                offered.iter().any(|(d, _, _)| *d == key.as_str()),
                "spec.preferences.{key} has no candidate table in `domains`"
            );
            assert!(
                has_heading(&format!("### spec.preferences.{key}")),
                "spec.preferences.{key} has no `### spec.preferences.{key}` section in docs/spec/profile.md"
            );
        }
    }
}
