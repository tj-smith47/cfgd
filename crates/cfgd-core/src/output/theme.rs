use console::{Color, Style};

use crate::config::ThemeConfig;

// Default icons shared across all non-minimal presets
const ICON_SUCCESS: &str = "✓";
const ICON_WARNING: &str = "⚠";
const ICON_ERROR: &str = "✗";
const ICON_INFO: &str = "⚙";
const ICON_PENDING: &str = "○";
const ICON_ARROW: &str = "→";

pub struct Theme {
    pub success: Style,
    pub warning: Style,
    pub error: Style,
    pub info: Style,
    pub muted: Style,

    pub header: Style,
    pub subheader: Style,
    pub key: Style,
    pub value: Style,

    pub diff_add: Style,
    pub diff_remove: Style,
    pub diff_context: Style,

    pub icon_success: String,
    pub icon_warning: String,
    pub icon_error: String,
    pub icon_info: String,
    pub icon_pending: String,
    pub icon_arrow: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            success: Style::new().green(),
            warning: Style::new().yellow(),
            error: Style::new().red().bold(),
            info: Style::new().cyan(),
            muted: Style::new().dim(),

            header: Style::new().bold().cyan(),
            subheader: Style::new().bold(),
            key: Style::new().bold(),
            value: Style::new(),

            diff_add: Style::new().green(),
            diff_remove: Style::new().red(),
            diff_context: Style::new().dim(),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }
}

impl Theme {
    pub fn from_config(config: Option<&ThemeConfig>) -> Self {
        let config = match config {
            Some(c) => c,
            None => return Self::default(),
        };

        let mut theme = Self::from_preset(&config.name);

        // Apply hex color overrides
        let ov = &config.overrides;
        if let Some(ref c) = ov.success {
            apply_color(&mut theme.success, c);
        }
        if let Some(ref c) = ov.warning {
            apply_color(&mut theme.warning, c);
        }
        if let Some(ref c) = ov.error {
            apply_color(&mut theme.error, c);
        }
        if let Some(ref c) = ov.info {
            apply_color(&mut theme.info, c);
        }
        if let Some(ref c) = ov.muted {
            apply_color(&mut theme.muted, c);
        }
        if let Some(ref c) = ov.header {
            apply_color(&mut theme.header, c);
        }
        if let Some(ref c) = ov.subheader {
            apply_color(&mut theme.subheader, c);
        }
        if let Some(ref c) = ov.key {
            apply_color(&mut theme.key, c);
        }
        if let Some(ref c) = ov.value {
            apply_color(&mut theme.value, c);
        }
        if let Some(ref c) = ov.diff_add {
            apply_color(&mut theme.diff_add, c);
        }
        if let Some(ref c) = ov.diff_remove {
            apply_color(&mut theme.diff_remove, c);
        }
        if let Some(ref c) = ov.diff_context {
            apply_color(&mut theme.diff_context, c);
        }

        // Apply icon overrides
        if let Some(ref v) = ov.icon_success {
            theme.icon_success = v.clone();
        }
        if let Some(ref v) = ov.icon_warning {
            theme.icon_warning = v.clone();
        }
        if let Some(ref v) = ov.icon_error {
            theme.icon_error = v.clone();
        }
        if let Some(ref v) = ov.icon_info {
            theme.icon_info = v.clone();
        }
        if let Some(ref v) = ov.icon_pending {
            theme.icon_pending = v.clone();
        }
        if let Some(ref v) = ov.icon_arrow {
            theme.icon_arrow = v.clone();
        }

        theme
    }

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
            success: style_from_hex("#50fa7b"),
            warning: style_from_hex("#f1fa8c"),
            error: style_from_hex("#ff5555").bold(),
            info: style_from_hex("#8be9fd"),
            muted: style_from_hex("#6272a4"),

            header: style_from_hex("#bd93f9").bold(),
            subheader: Style::new().bold(),
            key: style_from_hex("#ff79c6").bold(),
            value: style_from_hex("#f8f8f2"),

            diff_add: style_from_hex("#50fa7b"),
            diff_remove: style_from_hex("#ff5555"),
            diff_context: style_from_hex("#6272a4"),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }

    fn solarized_dark() -> Self {
        Self {
            success: style_from_hex("#859900"),
            warning: style_from_hex("#b58900"),
            error: style_from_hex("#dc322f").bold(),
            info: style_from_hex("#268bd2"),
            muted: style_from_hex("#586e75"),

            header: style_from_hex("#268bd2").bold(),
            subheader: Style::new().bold(),
            key: style_from_hex("#2aa198").bold(),
            value: style_from_hex("#839496"),

            diff_add: style_from_hex("#859900"),
            diff_remove: style_from_hex("#dc322f"),
            diff_context: style_from_hex("#586e75"),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }

    fn solarized_light() -> Self {
        Self {
            success: style_from_hex("#859900"),
            warning: style_from_hex("#b58900"),
            error: style_from_hex("#dc322f").bold(),
            info: style_from_hex("#268bd2"),
            muted: style_from_hex("#93a1a1"),

            header: style_from_hex("#268bd2").bold(),
            subheader: Style::new().bold(),
            key: style_from_hex("#2aa198").bold(),
            value: style_from_hex("#657b83"),

            diff_add: style_from_hex("#859900"),
            diff_remove: style_from_hex("#dc322f"),
            diff_context: style_from_hex("#93a1a1"),

            icon_success: ICON_SUCCESS.into(),
            icon_warning: ICON_WARNING.into(),
            icon_error: ICON_ERROR.into(),
            icon_info: ICON_INFO.into(),
            icon_pending: ICON_PENDING.into(),
            icon_arrow: ICON_ARROW.into(),
        }
    }

    fn minimal() -> Self {
        Self {
            success: Style::new(),
            warning: Style::new(),
            error: Style::new().bold(),
            info: Style::new(),
            muted: Style::new().dim(),

            header: Style::new().bold(),
            subheader: Style::new().bold(),
            key: Style::new().bold(),
            value: Style::new(),

            diff_add: Style::new(),
            diff_remove: Style::new(),
            diff_context: Style::new().dim(),

            icon_success: "+".into(),
            icon_warning: "!".into(),
            icon_error: "x".into(),
            icon_info: "-".into(),
            icon_pending: " ".into(),
            icon_arrow: ">".into(),
        }
    }
}

/// Parse a hex color string (#rrggbb or rrggbb) into a console::Color.
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

/// Map RGB to the nearest ANSI 256-color index.
pub(super) fn ansi256_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    // Check grayscale ramp (232-255) first
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((r as u16 - 8) * 24 / 247) as u8) + 232;
    }
    // Map to 6x6x6 color cube (indices 16-231)
    let ri = (r as u16 * 5 / 255) as u8;
    let gi = (g as u16 * 5 / 255) as u8;
    let bi = (b as u16 * 5 / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}

fn style_from_hex(hex: &str) -> Style {
    match parse_hex_color(hex) {
        Some(color) => Style::new().fg(color),
        None => Style::new(),
    }
}

fn apply_color(style: &mut Style, hex: &str) {
    if let Some(color) = parse_hex_color(hex) {
        *style = Style::new().fg(color);
    }
}
