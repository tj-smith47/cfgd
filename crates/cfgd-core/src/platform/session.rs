//! Session detection — which display server, remoteness and container the
//! running process can reach. `Platform` answers WHICH MACHINE; this answers
//! WHICH SESSION, and a preference candidate needs both.

/// The display server a session can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    X11,
    Wayland,
}

/// What kind of session this process runs under, as the environment reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    /// The display server, or `None` for a session with no graphical display
    /// at all — a bare tty, or an SSH login with no forwarded `DISPLAY`.
    pub display: Option<DisplayServer>,
    /// Whether this process was reached over SSH.
    pub ssh: bool,
    /// Whether this process runs inside WSL.
    pub wsl: bool,
}

impl Session {
    /// Read the session out of the environment.
    ///
    /// Read fresh on every call. [`super::Platform::current`] memoizes because a
    /// platform's inputs are fixed at boot; `WAYLAND_DISPLAY` and `SSH_TTY` are
    /// ordinary process environment. Six environment reads cost nothing to
    /// repeat, and a memo would freeze a test's first answer for the whole binary.
    pub fn detect() -> Self {
        // XWayland sets `DISPLAY` on a Wayland session, so Wayland is asked
        // FIRST: a session offering both is Wayland with an X11 shim, and the
        // compositor's own clipboard is the one a user means.
        let display = if env_set("WAYLAND_DISPLAY") {
            Some(DisplayServer::Wayland)
        } else if env_set("DISPLAY") {
            Some(DisplayServer::X11)
        } else {
            match std::env::var("XDG_SESSION_TYPE").as_deref() {
                Ok("wayland") => Some(DisplayServer::Wayland),
                Ok("x11") => Some(DisplayServer::X11),
                _ => None,
            }
        };
        Session {
            display,
            ssh: env_set("SSH_TTY") || env_set("SSH_CONNECTION"),
            wsl: env_set("WSL_DISTRO_NAME"),
        }
    }
}

/// A signal counts as set only when it carries a value: a login shell that
/// exports `DISPLAY=` hands on an empty string, which reaches no X server.
fn env_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::EnvVarGuard;

    /// Clears every signal `detect` reads, so a developer's own desktop
    /// session cannot decide what a case asserts.
    fn cleared() -> Vec<EnvVarGuard> {
        [
            "WAYLAND_DISPLAY",
            "DISPLAY",
            "XDG_SESSION_TYPE",
            "SSH_TTY",
            "SSH_CONNECTION",
            "WSL_DISTRO_NAME",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .collect()
    }

    #[test]
    #[serial_test::serial]
    fn a_session_offering_both_display_protocols_is_wayland() {
        let _c = cleared();
        let _w = EnvVarGuard::set("WAYLAND_DISPLAY", "wayland-0");
        let _x = EnvVarGuard::set("DISPLAY", ":0");
        assert_eq!(Session::detect().display, Some(DisplayServer::Wayland));
    }

    #[test]
    #[serial_test::serial]
    fn an_ssh_session_with_no_forwarded_display_reaches_no_display_server() {
        let _c = cleared();
        let _s = EnvVarGuard::set("SSH_TTY", "/dev/pts/3");
        let s = Session::detect();
        assert_eq!(s.display, None);
        assert!(s.ssh);
        assert!(!s.wsl);
    }

    #[test]
    #[serial_test::serial]
    fn xdg_session_type_answers_when_neither_display_var_is_set() {
        let _c = cleared();
        let _t = EnvVarGuard::set("XDG_SESSION_TYPE", "x11");
        assert_eq!(Session::detect().display, Some(DisplayServer::X11));
    }

    #[test]
    #[serial_test::serial]
    fn an_empty_display_is_not_a_display() {
        let _c = cleared();
        let _x = EnvVarGuard::set("DISPLAY", "");
        assert_eq!(Session::detect().display, None);
    }

    #[test]
    #[serial_test::serial]
    fn wsl_is_read_off_its_distro_name() {
        let _c = cleared();
        let _w = EnvVarGuard::set("WSL_DISTRO_NAME", "Ubuntu");
        assert!(Session::detect().wsl);
    }
}
