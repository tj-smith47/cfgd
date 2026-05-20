use console::{Color, Style};

const ICON_OK: &str = "✓";
const ICON_WARN: &str = "⚠";
const ICON_FAIL: &str = "✗";
const ICON_PENDING: &str = "○";
const ICON_RUNNING: &str = "◐";
const ICON_SKIPPED: &str = "—";
const ICON_ARROW: &str = "→";

pub struct Theme {
    // Style slots (8)
    pub header: Style,
    pub success: Style,
    pub warning: Style,
    pub error: Style,
    pub info: Style,
    pub muted: Style,
    pub running: Style,
    pub diff_add: Style,
    pub diff_remove: Style,
    pub diff_context: Style,

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
            header: Style::new().bold().cyan(),
            success: Style::new().green(),
            warning: Style::new().yellow(),
            error: Style::new().red().bold(),
            info: Style::new().cyan(),
            muted: Style::new().dim(),
            running: Style::new().cyan(),
            diff_add: Style::new().green(),
            diff_remove: Style::new().red(),
            diff_context: Style::new().dim(),
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
            header: Style::new().bold(),
            success: Style::new(),
            warning: Style::new(),
            error: Style::new().bold(),
            info: Style::new(),
            muted: Style::new().dim(),
            running: Style::new(),
            diff_add: Style::new(),
            diff_remove: Style::new(),
            diff_context: Style::new().dim(),
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

pub(super) fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Color256(ansi256_from_rgb(r, g, b)))
}

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

fn hex(s: &str) -> Style {
    match parse_hex_color(s) {
        Some(c) => Style::new().fg(c),
        None => Style::new(),
    }
}

fn apply_color(style: &mut Style, hex: &str) {
    if let Some(c) = parse_hex_color(hex) {
        *style = Style::new().fg(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Default success is plain green; dracula uses hex; minimal is plain.
        assert_ne!(format!("{:?}", d.success), format!("{:?}", dr.success));
        assert_eq!(m.icon_ok, "+");
    }

    #[test]
    fn unknown_preset_falls_back_to_default() {
        let t = Theme::from_preset("not-a-real-preset");
        assert_eq!(t.icon_ok, "✓"); // matches default
    }

    #[test]
    fn hex_parses_six_chars() {
        assert!(parse_hex_color("#abcdef").is_some());
        assert!(parse_hex_color("abcdef").is_some());
        assert!(parse_hex_color("#abc").is_none());
        assert!(parse_hex_color("#zzzzzz").is_none());
    }
}
