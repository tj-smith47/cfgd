use std::fmt::{self, Display};

use console::{Color, Style};

const ICON_OK: &str = "✓";
const ICON_WARN: &str = "⚠";
const ICON_FAIL: &str = "✗";
const ICON_PENDING: &str = "○";
const ICON_RUNNING: &str = "◐";
const ICON_SKIPPED: &str = "—";
const ICON_ARROW: &str = "→";

/// Single style slot held by `Theme`. Wraps `console::Style` (used for the
/// 256-color fallback path and for non-color attributes like bold/dim) and
/// optionally carries an `(r, g, b)` triple for high-fidelity rendering on
/// truecolor-capable terminals. The decision between truecolor and 256-color
/// is taken at render time inside `apply_to`, so existing call sites are
/// unaffected by the upgrade.
#[derive(Debug, Clone, Default)]
pub struct ThemedStyle {
    /// `console::Style` carrying attrs and (when no `rgb` is present) the
    /// 256-color foreground.
    inner: Style,
    /// Original truecolor triple, populated by `from_hex`. Read by `apply_to`
    /// when the terminal advertises truecolor support.
    rgb: Option<(u8, u8, u8)>,
    /// Attribute set, kept separately so the truecolor render path can emit
    /// SGR parameters without re-deriving them from `inner` (which only
    /// exposes its attrs via its `Debug` impl).
    attrs: AttrSet,
}

#[derive(Debug, Clone, Copy, Default)]
struct AttrSet {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl AttrSet {
    /// Whether any SGR attribute is set. Predicate guard for the
    /// `Display`-into-formatter path so callers can branch without
    /// pre-rendering an empty parameter string.
    fn has_attrs(&self) -> bool {
        self.bold || self.dim || self.italic || self.underline
    }
}

/// Writes SGR attribute parameters (without leading `\x1b[`, without
/// trailing `m`) joined by `;` directly into the formatter — no
/// intermediate `String` allocation on the styled-write hot path.
/// Always preceded by `\x1b[` and (optionally) followed by `;38;...` +
/// `m` by the caller.
impl Display for AttrSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let mut push = |f: &mut fmt::Formatter<'_>, s: &str| -> fmt::Result {
            if !first {
                f.write_str(";")?;
            }
            f.write_str(s)?;
            first = false;
            Ok(())
        };
        if self.bold {
            push(f, "1")?;
        }
        if self.dim {
            push(f, "2")?;
        }
        if self.italic {
            push(f, "3")?;
        }
        if self.underline {
            push(f, "4")?;
        }
        Ok(())
    }
}

impl ThemedStyle {
    /// Plain style — no color, no attrs. Matches `console::Style::new()`.
    pub fn plain() -> Self {
        Self::default()
    }

    /// Build a style from a `#rrggbb` hex string. On terminals that advertise
    /// truecolor support (`COLORTERM=truecolor|24bit`), `apply_to` emits the
    /// exact 24-bit color. Otherwise the color is quantized to the nearest
    /// ANSI 256-color slot for compatibility.
    pub fn from_hex(hex: &str) -> Self {
        match parse_hex_rgb(hex) {
            Some((r, g, b)) => Self {
                inner: Style::new().fg(Color::Color256(ansi256_from_rgb(r, g, b))),
                rgb: Some((r, g, b)),
                attrs: AttrSet::default(),
            },
            None => Self::default(),
        }
    }

    /// Build a style from a `console::Color`. Used for named-color presets
    /// (`Color::Cyan`, `Color::Red`, ...) where no RGB triple is available.
    fn from_console_color(color: Color) -> Self {
        Self {
            inner: Style::new().fg(color),
            rgb: None,
            attrs: AttrSet::default(),
        }
    }

    pub fn bold(mut self) -> Self {
        self.inner = self.inner.bold();
        self.attrs.bold = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.inner = self.inner.dim();
        self.attrs.dim = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.inner = self.inner.italic();
        self.attrs.italic = true;
        self
    }

    pub fn underlined(mut self) -> Self {
        self.inner = self.inner.underlined();
        self.attrs.underline = true;
        self
    }

    pub fn cyan(self) -> Self {
        Self::from_console_color(Color::Cyan).with_attrs(self.attrs)
    }

    pub fn red(self) -> Self {
        Self::from_console_color(Color::Red).with_attrs(self.attrs)
    }

    pub fn green(self) -> Self {
        Self::from_console_color(Color::Green).with_attrs(self.attrs)
    }

    pub fn yellow(self) -> Self {
        Self::from_console_color(Color::Yellow).with_attrs(self.attrs)
    }

    fn with_attrs(mut self, attrs: AttrSet) -> Self {
        if attrs.bold {
            self.inner = self.inner.bold();
        }
        if attrs.dim {
            self.inner = self.inner.dim();
        }
        if attrs.italic {
            self.inner = self.inner.italic();
        }
        if attrs.underline {
            self.inner = self.inner.underlined();
        }
        self.attrs = attrs;
        self
    }

    /// Wrap `text` for `Display` rendering. Resolved at format-time:
    ///
    /// - `console::colors_enabled()` is false (NO_COLOR / TERM=dumb / not a
    ///   tty) AND no attrs → emit `text` with no escapes.
    /// - `console::colors_enabled()` is false AND attrs are set → emit
    ///   `\x1b[<attrs>m{text}\x1b[0m`. NO_COLOR (per no-color.org) governs
    ///   color only — bold/dim/italic/underline are independent SGR signals
    ///   load-bearing for the `default` (italic accent) and `minimal`
    ///   (italic accent, underlined secondary) presets that intentionally
    ///   carry the accent/secondary distinction in non-color attrs.
    /// - `supports_truecolor()` is true AND an RGB triple is present → emit
    ///   `\x1b[<attrs>;38;2;R;G;Bm{text}\x1b[0m`.
    /// - Otherwise → delegate to `console::Style::apply_to`, which yields
    ///   the 256-color fallback path (existing behavior).
    pub fn apply_to<D: Display>(&self, text: D) -> StyledText<'_, D> {
        StyledText { style: self, text }
    }
}

/// `Display`-wrapper returned by `ThemedStyle::apply_to`. Stays generic over
/// the inner payload so callers can format `&str`, `String`, or anything else
/// `Display` without extra allocation up front.
pub struct StyledText<'a, D> {
    style: &'a ThemedStyle,
    text: D,
}

impl<D: Display> Display for StyledText<'_, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let attrs = &self.style.attrs;

        if !console::colors_enabled() {
            // NO_COLOR / TERM=dumb / not a tty: emit attrs-only SGR (bold,
            // dim, italic, underlined are independent of color per
            // no-color.org) so the `default` italic accent and the
            // `minimal` italic accent / underlined secondary keep their
            // non-color differentiator. No allocation when no attrs set.
            if !attrs.has_attrs() {
                return write!(f, "{}", self.text);
            }
            return write!(f, "\x1b[{attrs}m{}\x1b[0m", self.text);
        }

        if let Some((r, g, b)) = self.style.rgb
            && supports_truecolor()
        {
            if !attrs.has_attrs() {
                return write!(f, "\x1b[38;2;{r};{g};{b}m{}\x1b[0m", self.text);
            }
            return write!(f, "\x1b[{attrs};38;2;{r};{g};{b}m{}\x1b[0m", self.text);
        }

        write!(f, "{}", self.style.inner.apply_to(&self.text))
    }
}

pub struct Theme {
    // Style slots (12)
    pub header: ThemedStyle,
    pub success: ThemedStyle,
    pub warning: ThemedStyle,
    pub error: ThemedStyle,
    pub info: ThemedStyle,
    pub muted: ThemedStyle,
    pub running: ThemedStyle,
    pub diff_add: ThemedStyle,
    pub diff_remove: ThemedStyle,
    pub diff_context: ThemedStyle,
    /// "Attention without alarm" — orange-family in Dracula/Solarized, italic
    /// non-color signal in `default` and `minimal`. Drives `Role::Accent`.
    pub accent: ThemedStyle,
    /// "Structural pivot / label / identifier" — pink/magenta family in
    /// Dracula/Solarized, underlined non-color signal in `minimal`. Drives
    /// `Role::Secondary`.
    pub secondary: ThemedStyle,

    // Icon slots (7)
    pub icon_ok: String,
    pub icon_warn: String,
    pub icon_fail: String,
    pub icon_pending: String,
    pub icon_running: String,
    pub icon_skipped: String,
    pub icon_arrow: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            header: ThemedStyle::plain().bold().cyan(),
            success: ThemedStyle::plain().green(),
            warning: ThemedStyle::plain().yellow(),
            error: ThemedStyle::plain().red().bold(),
            info: ThemedStyle::plain().cyan(),
            muted: ThemedStyle::plain().dim(),
            running: ThemedStyle::plain().cyan(),
            diff_add: ThemedStyle::plain().green(),
            diff_remove: ThemedStyle::plain().red(),
            diff_context: ThemedStyle::plain().dim(),
            // Italic keeps an honest non-color signal under NO_COLOR; the hex
            // gives truecolor terminals an orange-leaning accent that does not
            // collide with the yellow `warning` slot.
            accent: hex("#d78700").italic(),
            secondary: hex("#af5fd7"),
            icon_ok: ICON_OK.into(),
            icon_warn: ICON_WARN.into(),
            icon_fail: ICON_FAIL.into(),
            icon_pending: ICON_PENDING.into(),
            icon_running: ICON_RUNNING.into(),
            icon_skipped: ICON_SKIPPED.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }
}

impl Theme {
    pub fn from_preset(name: &str) -> Self {
        match name {
            "dracula" => Self::dracula(),
            "solarized-dark" => Self::solarized_dark(),
            "solarized-light" => Self::solarized_light(),
            "minimal" => Self::minimal(),
            _ => Self::default(),
        }
    }

    fn dracula() -> Self {
        Self {
            header: hex("#bd93f9").bold(),
            success: hex("#50fa7b"),
            warning: hex("#f1fa8c"),
            error: hex("#ff5555").bold(),
            info: hex("#8be9fd"),
            muted: hex("#6272a4"),
            running: hex("#8be9fd"),
            diff_add: hex("#50fa7b"),
            diff_remove: hex("#ff5555"),
            diff_context: hex("#6272a4"),
            accent: hex("#ffb86c"),
            secondary: hex("#ff79c6"),
            ..Self::default()
        }
    }

    fn solarized_dark() -> Self {
        Self {
            header: hex("#268bd2").bold(),
            success: hex("#859900"),
            warning: hex("#b58900"),
            error: hex("#dc322f").bold(),
            info: hex("#268bd2"),
            muted: hex("#586e75"),
            running: hex("#2aa198"),
            diff_add: hex("#859900"),
            diff_remove: hex("#dc322f"),
            diff_context: hex("#586e75"),
            accent: hex("#cb4b16"),
            secondary: hex("#d33682"),
            ..Self::default()
        }
    }

    fn solarized_light() -> Self {
        Self {
            header: hex("#268bd2").bold(),
            success: hex("#859900"),
            warning: hex("#b58900"),
            error: hex("#dc322f").bold(),
            info: hex("#268bd2"),
            muted: hex("#93a1a1"),
            running: hex("#2aa198"),
            diff_add: hex("#859900"),
            diff_remove: hex("#dc322f"),
            diff_context: hex("#93a1a1"),
            accent: hex("#cb4b16"),
            secondary: hex("#d33682"),
            ..Self::default()
        }
    }

    pub fn from_config(config: Option<&crate::config::ThemeConfig>) -> Self {
        let Some(cfg) = config else {
            return Self::default();
        };
        let mut t = Self::from_preset(&cfg.name);
        let ov = &cfg.overrides;
        // Style overrides
        if let Some(c) = &ov.header {
            apply_color(&mut t.header, c);
        }
        if let Some(c) = &ov.success {
            apply_color(&mut t.success, c);
        }
        if let Some(c) = &ov.warning {
            apply_color(&mut t.warning, c);
        }
        if let Some(c) = &ov.error {
            apply_color(&mut t.error, c);
        }
        if let Some(c) = &ov.info {
            apply_color(&mut t.info, c);
        }
        if let Some(c) = &ov.muted {
            apply_color(&mut t.muted, c);
        }
        if let Some(c) = &ov.running {
            apply_color(&mut t.running, c);
        }
        if let Some(c) = &ov.diff_add {
            apply_color(&mut t.diff_add, c);
        }
        if let Some(c) = &ov.diff_remove {
            apply_color(&mut t.diff_remove, c);
        }
        if let Some(c) = &ov.diff_context {
            apply_color(&mut t.diff_context, c);
        }
        if let Some(c) = &ov.accent {
            apply_color(&mut t.accent, c);
        }
        if let Some(c) = &ov.secondary {
            apply_color(&mut t.secondary, c);
        }
        // Icon overrides
        if let Some(v) = &ov.icon_ok {
            t.icon_ok = v.clone();
        }
        if let Some(v) = &ov.icon_warn {
            t.icon_warn = v.clone();
        }
        if let Some(v) = &ov.icon_fail {
            t.icon_fail = v.clone();
        }
        if let Some(v) = &ov.icon_pending {
            t.icon_pending = v.clone();
        }
        if let Some(v) = &ov.icon_running {
            t.icon_running = v.clone();
        }
        if let Some(v) = &ov.icon_skipped {
            t.icon_skipped = v.clone();
        }
        if let Some(v) = &ov.icon_arrow {
            t.icon_arrow = v.clone();
        }
        t
    }

    fn minimal() -> Self {
        Self {
            header: ThemedStyle::plain().bold(),
            success: ThemedStyle::plain(),
            warning: ThemedStyle::plain(),
            error: ThemedStyle::plain().bold(),
            info: ThemedStyle::plain(),
            muted: ThemedStyle::plain().dim(),
            running: ThemedStyle::plain(),
            diff_add: ThemedStyle::plain(),
            diff_remove: ThemedStyle::plain(),
            diff_context: ThemedStyle::plain().dim(),
            // Italic vs underlined keeps the two accent axes distinguishable
            // without any color budget — orthogonal to bold/dim already used by
            // header/error/muted.
            accent: ThemedStyle::plain().italic(),
            secondary: ThemedStyle::plain().underlined(),
            icon_ok: "+".into(),
            icon_warn: "!".into(),
            icon_fail: "x".into(),
            icon_pending: " ".into(),
            icon_running: ".".into(),
            icon_skipped: "-".into(),
            icon_arrow: ">".into(),
        }
    }
}

/// Detect 24-bit color support via the standard `COLORTERM` signal, matching
/// the convention used by `bat`, `delta`, `git diff --color`, `lsd`, `eza`,
/// and friends. Honors `NO_COLOR` so the signal can't override an explicit
/// opt-out.
pub fn supports_truecolor() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    matches!(
        std::env::var("COLORTERM").as_deref(),
        Ok("truecolor") | Ok("24bit")
    )
}

/// Parse `#rrggbb` (or `rrggbb`) into an `(r, g, b)` triple. `None` for any
/// malformed input.
pub(super) fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Quantize an RGB triple to the closest ANSI 256-color slot. Used for the
/// 256-color fallback path when the terminal does not advertise truecolor
/// support. Algorithm matches xterm's 6×6×6 cube + 24-step grayscale ramp.
pub(super) fn ansi256_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((r as u16 - 8) * 24 / 247) as u8) + 232;
    }
    let ri = (r as u16 * 5 / 255) as u8;
    let gi = (g as u16 * 5 / 255) as u8;
    let bi = (b as u16 * 5 / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}

fn hex(s: &str) -> ThemedStyle {
    ThemedStyle::from_hex(s)
}

fn apply_color(style: &mut ThemedStyle, hex: &str) {
    if let Some((r, g, b)) = parse_hex_rgb(hex) {
        let attrs = style.attrs;
        *style = ThemedStyle {
            inner: Style::new().fg(Color::Color256(ansi256_from_rgb(r, g, b))),
            rgb: Some((r, g, b)),
            attrs: AttrSet::default(),
        }
        .with_attrs(attrs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::test_support::ColorsEnabledGuard;
    use crate::test_helpers::EnvVarGuard;
    use serial_test::serial;

    #[test]
    fn default_has_seven_icons() {
        let t = Theme::default();
        assert_eq!(t.icon_ok, "✓");
        assert_eq!(t.icon_warn, "⚠");
        assert_eq!(t.icon_fail, "✗");
        assert_eq!(t.icon_pending, "○");
        assert_eq!(t.icon_running, "◐");
        assert_eq!(t.icon_skipped, "—");
        assert_eq!(t.icon_arrow, "→");
    }

    #[test]
    fn presets_are_distinct() {
        let d = Theme::default();
        let dr = Theme::from_preset("dracula");
        let m = Theme::from_preset("minimal");
        // Default success is plain green; dracula uses hex (carries rgb).
        assert!(d.success.rgb.is_none());
        assert!(dr.success.rgb.is_some());
        assert_eq!(m.icon_ok, "+");
    }

    #[test]
    fn unknown_preset_falls_back_to_default() {
        let t = Theme::from_preset("not-a-real-preset");
        assert_eq!(t.icon_ok, "✓"); // matches default
    }

    #[test]
    fn hex_parses_six_chars() {
        assert!(parse_hex_rgb("#abcdef").is_some());
        assert!(parse_hex_rgb("abcdef").is_some());
        assert!(parse_hex_rgb("#abc").is_none());
        assert!(parse_hex_rgb("#zzzzzz").is_none());
    }

    #[test]
    #[serial]
    fn supports_truecolor_detects_colorterm_truecolor() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _g = EnvVarGuard::set("COLORTERM", "truecolor");
        assert!(supports_truecolor());
    }

    #[test]
    #[serial]
    fn supports_truecolor_detects_colorterm_24bit() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _g = EnvVarGuard::set("COLORTERM", "24bit");
        assert!(supports_truecolor());
    }

    #[test]
    #[serial]
    fn supports_truecolor_rejects_other_colorterm_values() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _g = EnvVarGuard::set("COLORTERM", "yes");
        assert!(!supports_truecolor());
    }

    #[test]
    #[serial]
    fn supports_truecolor_rejects_when_no_color_set() {
        let _g = EnvVarGuard::set("COLORTERM", "truecolor");
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        assert!(!supports_truecolor());
    }

    #[test]
    #[serial]
    fn supports_truecolor_returns_false_when_colorterm_unset() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _g = EnvVarGuard::unset("COLORTERM");
        assert!(!supports_truecolor());
    }

    #[test]
    #[serial]
    fn hex_style_emits_truecolor_escape_when_supported() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let _colors = ColorsEnabledGuard::set(true);
        let style = ThemedStyle::from_hex("#bd93f9");
        let out = style.apply_to("hi").to_string();
        assert_eq!(out, "\x1b[38;2;189;147;249mhi\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn hex_style_with_bold_emits_truecolor_with_attr() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let _colors = ColorsEnabledGuard::set(true);
        let style = ThemedStyle::from_hex("#bd93f9").bold();
        let out = style.apply_to("hi").to_string();
        assert_eq!(out, "\x1b[1;38;2;189;147;249mhi\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn hex_style_falls_back_to_256_when_no_truecolor() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _ct = EnvVarGuard::unset("COLORTERM");
        let _colors = ColorsEnabledGuard::set(true);
        let style = ThemedStyle::from_hex("#bd93f9");
        let out = style.apply_to("hi").to_string();
        // Output must contain the 256-color SGR for the quantized slot.
        let (r, g, b) = (0xbd, 0x93, 0xf9);
        let expected_slot = ansi256_from_rgb(r, g, b);
        let needle = format!("38;5;{expected_slot}");
        assert!(
            out.contains(&needle),
            "expected fallback to contain {needle:?}, got: {out:?}"
        );
        assert!(
            !out.contains("38;2;"),
            "must not emit truecolor SGR in fallback: {out:?}"
        );
    }

    #[test]
    #[serial]
    fn no_color_strips_color_keeps_attrs() {
        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        // Simulate the colors-disabled state for this test.
        let _colors = ColorsEnabledGuard::set(false);
        // Attrs are independent of color per no-color.org: bold survives.
        let style = ThemedStyle::from_hex("#bd93f9").bold();
        let out = style.apply_to("hi").to_string();
        assert_eq!(out, "\x1b[1mhi\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn no_color_keeps_italic_for_default_accent() {
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        let _colors = ColorsEnabledGuard::set(false);
        // Matches the `default` preset's accent slot: hex("#d78700").italic()
        let style = ThemedStyle::from_hex("#d78700").italic();
        let out = style.apply_to("x").to_string();
        assert_eq!(out, "\x1b[3mx\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn no_color_keeps_bold_on_plain_style() {
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        let _colors = ColorsEnabledGuard::set(false);
        let out = ThemedStyle::plain().bold().apply_to("x").to_string();
        assert_eq!(out, "\x1b[1mx\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn no_color_keeps_underline_for_minimal_secondary() {
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        let _colors = ColorsEnabledGuard::set(false);
        // Matches the `minimal` preset's secondary slot.
        let out = ThemedStyle::plain().underlined().apply_to("x").to_string();
        assert_eq!(out, "\x1b[4mx\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn no_color_emits_no_escapes_when_no_attrs() {
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        let _colors = ColorsEnabledGuard::set(false);
        let out = ThemedStyle::plain().apply_to("x").to_string();
        assert_eq!(out, "x", "got: {out:?}");
        // Hex without attrs also emits no escapes when colors are off.
        let out2 = ThemedStyle::from_hex("#bd93f9").apply_to("y").to_string();
        assert_eq!(out2, "y", "got: {out2:?}");
    }

    #[test]
    #[serial]
    fn no_color_joins_multiple_attrs() {
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        let _colors = ColorsEnabledGuard::set(false);
        // bold + italic share the attrs path.
        let out = ThemedStyle::plain()
            .bold()
            .italic()
            .apply_to("x")
            .to_string();
        assert_eq!(out, "\x1b[1;3mx\x1b[0m", "got: {out:?}");
    }

    #[test]
    fn from_hex_invalid_returns_plain_default() {
        let s = ThemedStyle::from_hex("not-a-color");
        assert!(s.rgb.is_none(), "invalid hex must not carry an rgb triple");
        assert!(!s.attrs.has_attrs(), "invalid hex must not carry any attrs");
    }

    #[test]
    fn from_hex_three_char_short_form_rejected() {
        // The parser requires six hex chars; the three-char short form is
        // not accepted and must round-trip to the default style.
        assert!(parse_hex_rgb("#abc").is_none());
        let s = ThemedStyle::from_hex("#abc");
        assert!(s.rgb.is_none());
    }

    #[test]
    fn with_attrs_preserves_italic_and_underline_through_color_swap() {
        // `cyan()` reconstructs the style from a console color and then calls
        // `with_attrs` to re-apply the prior attribute set. This exercises the
        // italic + underlined branches inside `with_attrs` that the existing
        // bold-only tests don't reach.
        let s = ThemedStyle::plain().italic().underlined().cyan();
        assert!(s.attrs.italic, "italic should survive color swap");
        assert!(s.attrs.underline, "underline should survive color swap");
        assert!(!s.attrs.bold);
        assert!(!s.attrs.dim);
    }

    #[test]
    fn with_attrs_preserves_dim_through_color_swap() {
        // `red()`/`green()`/etc. all funnel through `with_attrs`; verify the
        // `dim` branch (line 158) is reached and preserved.
        let s = ThemedStyle::plain().dim().red();
        assert!(s.attrs.dim, "dim attr should survive color swap");
        assert!(!s.attrs.bold);
    }

    #[test]
    fn with_attrs_preserves_all_attrs_through_yellow_swap() {
        let s = ThemedStyle::plain()
            .bold()
            .dim()
            .italic()
            .underlined()
            .yellow();
        assert!(s.attrs.bold);
        assert!(s.attrs.dim);
        assert!(s.attrs.italic);
        assert!(s.attrs.underline);
    }

    #[test]
    fn ansi256_grayscale_low_clamps_to_pure_black() {
        // r == g == b, with r < 8 → ANSI slot 16 (pure black).
        assert_eq!(ansi256_from_rgb(0, 0, 0), 16);
        assert_eq!(ansi256_from_rgb(7, 7, 7), 16);
    }

    #[test]
    fn ansi256_grayscale_high_clamps_to_pure_white() {
        // r == g == b, with r > 248 → ANSI slot 231 (pure white).
        assert_eq!(ansi256_from_rgb(255, 255, 255), 231);
        assert_eq!(ansi256_from_rgb(249, 249, 249), 231);
    }

    #[test]
    fn ansi256_grayscale_ramp_midrange_maps_into_232_to_255() {
        // r == g == b, with 8 <= r <= 248 → grayscale ramp 232..=255.
        let mid = ansi256_from_rgb(128, 128, 128);
        assert!(
            (232..=255).contains(&mid),
            "expected grayscale-ramp slot for #808080, got: {mid}"
        );
        // Edge: r == 8 lands at 232 (first ramp slot).
        assert_eq!(ansi256_from_rgb(8, 8, 8), 232);
        // Edge: r == 248 maps near the top of the ramp.
        let high = ansi256_from_rgb(248, 248, 248);
        assert!(
            (232..=255).contains(&high),
            "r==248 should still be in the ramp, got: {high}"
        );
    }

    #[test]
    fn ansi256_non_gray_lands_in_color_cube() {
        // Color cube spans 16..=231 (16 + 6*6*6 - 1 = 231); pure red lands at
        // the cube's max-red plane.
        let red = ansi256_from_rgb(255, 0, 0);
        assert_eq!(red, 16 + 36 * 5);
        let green = ansi256_from_rgb(0, 255, 0);
        assert_eq!(green, 16 + 6 * 5);
        let blue = ansi256_from_rgb(0, 0, 255);
        assert_eq!(blue, 16 + 5);
    }

    #[test]
    fn from_config_none_yields_default_theme() {
        let t = Theme::from_config(None);
        assert_eq!(t.icon_ok, "✓");
        assert!(
            t.success.rgb.is_none(),
            "default success uses console color"
        );
    }

    #[test]
    fn from_config_picks_named_preset_via_name() {
        let cfg = crate::config::ThemeConfig {
            name: "dracula".to_string(),
            overrides: crate::config::ThemeOverrides::default(),
        };
        let t = Theme::from_config(Some(&cfg));
        // Dracula's success is the green hex #50fa7b.
        assert_eq!(t.success.rgb, Some((0x50, 0xfa, 0x7b)));
    }

    #[test]
    fn from_config_unknown_preset_falls_back_to_default() {
        let cfg = crate::config::ThemeConfig {
            name: "no-such-preset".to_string(),
            overrides: crate::config::ThemeOverrides::default(),
        };
        let t = Theme::from_config(Some(&cfg));
        assert!(t.success.rgb.is_none(), "fallback to default → no rgb");
    }

    #[test]
    fn from_config_style_overrides_apply_all_twelve_slots() {
        // Each slot gets a distinct hex; verify the resolved Theme carries
        // back the exact rgb triple for each.
        let cfg = crate::config::ThemeConfig {
            name: "minimal".to_string(),
            overrides: crate::config::ThemeOverrides {
                header: Some("#010203".into()),
                success: Some("#040506".into()),
                warning: Some("#070809".into()),
                error: Some("#0a0b0c".into()),
                info: Some("#0d0e0f".into()),
                muted: Some("#101112".into()),
                running: Some("#131415".into()),
                diff_add: Some("#161718".into()),
                diff_remove: Some("#191a1b".into()),
                diff_context: Some("#1c1d1e".into()),
                accent: Some("#1f2021".into()),
                secondary: Some("#222324".into()),
                ..Default::default()
            },
        };
        let t = Theme::from_config(Some(&cfg));
        assert_eq!(t.header.rgb, Some((0x01, 0x02, 0x03)));
        assert_eq!(t.success.rgb, Some((0x04, 0x05, 0x06)));
        assert_eq!(t.warning.rgb, Some((0x07, 0x08, 0x09)));
        assert_eq!(t.error.rgb, Some((0x0a, 0x0b, 0x0c)));
        assert_eq!(t.info.rgb, Some((0x0d, 0x0e, 0x0f)));
        assert_eq!(t.muted.rgb, Some((0x10, 0x11, 0x12)));
        assert_eq!(t.running.rgb, Some((0x13, 0x14, 0x15)));
        assert_eq!(t.diff_add.rgb, Some((0x16, 0x17, 0x18)));
        assert_eq!(t.diff_remove.rgb, Some((0x19, 0x1a, 0x1b)));
        assert_eq!(t.diff_context.rgb, Some((0x1c, 0x1d, 0x1e)));
        assert_eq!(t.accent.rgb, Some((0x1f, 0x20, 0x21)));
        assert_eq!(t.secondary.rgb, Some((0x22, 0x23, 0x24)));
    }

    #[test]
    fn from_config_style_override_preserves_preset_attrs() {
        // Minimal's `error` slot is plain().bold(); overriding the color via
        // apply_color must not strip the bold attr.
        let cfg = crate::config::ThemeConfig {
            name: "minimal".to_string(),
            overrides: crate::config::ThemeOverrides {
                error: Some("#abcdef".into()),
                ..Default::default()
            },
        };
        let t = Theme::from_config(Some(&cfg));
        assert_eq!(t.error.rgb, Some((0xab, 0xcd, 0xef)));
        assert!(
            t.error.attrs.bold,
            "minimal preset's error slot is bold; override must preserve it"
        );
    }

    #[test]
    fn from_config_icon_overrides_apply_all_seven_slots() {
        let cfg = crate::config::ThemeConfig {
            name: "default".to_string(),
            overrides: crate::config::ThemeOverrides {
                icon_ok: Some("[ok]".into()),
                icon_warn: Some("[!]".into()),
                icon_fail: Some("[X]".into()),
                icon_pending: Some("[.]".into()),
                icon_running: Some("[*]".into()),
                icon_skipped: Some("[-]".into()),
                icon_arrow: Some("=>".into()),
                ..Default::default()
            },
        };
        let t = Theme::from_config(Some(&cfg));
        assert_eq!(t.icon_ok, "[ok]");
        assert_eq!(t.icon_warn, "[!]");
        assert_eq!(t.icon_fail, "[X]");
        assert_eq!(t.icon_pending, "[.]");
        assert_eq!(t.icon_running, "[*]");
        assert_eq!(t.icon_skipped, "[-]");
        assert_eq!(t.icon_arrow, "=>");
    }

    #[test]
    fn from_config_invalid_hex_override_leaves_slot_unchanged() {
        // apply_color's parse_hex_rgb returns None for malformed input; the
        // slot stays as the preset's value, including its rgb triple.
        let preset = Theme::from_preset("dracula");
        let original_rgb = preset.header.rgb;
        let cfg = crate::config::ThemeConfig {
            name: "dracula".to_string(),
            overrides: crate::config::ThemeOverrides {
                header: Some("not-a-hex-string".into()),
                ..Default::default()
            },
        };
        let t = Theme::from_config(Some(&cfg));
        assert_eq!(
            t.header.rgb, original_rgb,
            "invalid override must not mutate the slot"
        );
    }

    #[test]
    fn from_config_partial_override_only_touches_specified_slots() {
        // Override `header` only; the rest of the dracula preset slots stay.
        let cfg = crate::config::ThemeConfig {
            name: "dracula".to_string(),
            overrides: crate::config::ThemeOverrides {
                header: Some("#112233".into()),
                ..Default::default()
            },
        };
        let t = Theme::from_config(Some(&cfg));
        assert_eq!(t.header.rgb, Some((0x11, 0x22, 0x33)));
        // Dracula's success stays at #50fa7b.
        assert_eq!(t.success.rgb, Some((0x50, 0xfa, 0x7b)));
        // And the icons stay at the default.
        assert_eq!(t.icon_ok, "✓");
    }

    #[test]
    fn solarized_dark_preset_has_expected_palette() {
        let t = Theme::from_preset("solarized-dark");
        assert_eq!(t.success.rgb, Some((0x85, 0x99, 0x00)));
        assert_eq!(t.muted.rgb, Some((0x58, 0x6e, 0x75)));
    }

    #[test]
    fn solarized_light_preset_distinct_muted_from_dark() {
        let dark = Theme::from_preset("solarized-dark");
        let light = Theme::from_preset("solarized-light");
        // Only the muted/diff_context slot differs between solarized-dark and
        // solarized-light; everything else matches.
        assert_ne!(dark.muted.rgb, light.muted.rgb);
        assert_eq!(light.muted.rgb, Some((0x93, 0xa1, 0xa1)));
        assert_eq!(dark.success.rgb, light.success.rgb);
    }
}
