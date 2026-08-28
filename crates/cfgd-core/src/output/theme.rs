use std::fmt::{self, Display};

use console::{Color, Style};

const ICON_OK: &str = "✓";
const ICON_WARN: &str = "⚠";
const ICON_FAIL: &str = "✗";
const ICON_PENDING: &str = "○";
const ICON_RUNNING: &str = "◐";
// Never an em dash: `—` is already the renderer's GLUE, the mark that says a
// detail follows on this line, so a skip marked with it says two things at
// once. U+2205 over the circled `⊘`/`⊗` shapes because it is present in
// FiraCode Nerd Font Mono, which the recorded demos render in.
const ICON_SKIPPED: &str = "∅";
const ICON_ARROW: &str = "→";
// U+25C9, verified present in FiraCode Nerd Font Mono (the recorded demos'
// font), JetBrains Mono and DejaVu Sans Mono via `fc-list ":charset=25C9"`.
// It stays in the ○/◐ family the rest of the set draws from without being the
// filled `●` that would read as the END of the pending → running progression.
// The enclosed-alphanumeric `ⓘ` reads better in prose but is absent from every
// common terminal font, so it renders as a tofu box.
const ICON_INFO: &str = "◉";

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
    /// Whether this style may emit colour. Stamped once by
    /// [`Theme::with_colors`], from the decision the `Printer` took at its own
    /// construction, instead of re-read from `console`'s process-global flag at
    /// every render. `false` on a freshly built style, so a construction path
    /// that forgets to stamp renders UNSTYLED — that fails a positive assertion
    /// loudly, where the opposite default makes a negative one pass vacuously.
    colors: bool,
    /// Whether this style has been given an actual foreground colour (a
    /// truecolor hex or a named `console::Color`), independent of `colors`
    /// (which says only whether emitting it is currently allowed). Backs the
    /// `bold()` debug assertion: bold never pairs with colour in a
    /// theme's rendered style. A style built colourless and bolded first,
    /// then coloured via `recolor`, is the legitimate "attrs survive a
    /// colour swap" shape; a style that already carries a colour gaining
    /// bold is the silent pairing this field lets `bold()` refuse.
    has_color: bool,
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
                colors: false,
                has_color: true,
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
            colors: false,
            has_color: true,
        }
    }

    /// Bold never pairs with colour in a theme's rendered style, in either
    /// composition order. Panics in debug builds when the style already
    /// carries a colour; `with_attrs` carries the mirror check for the
    /// other order (bold carried into a style that is only now being
    /// coloured, via `recolor`/`apply_color`), since neither of those routes
    /// through this method.
    pub fn bold(mut self) -> Self {
        debug_assert!(
            !self.has_color,
            "bold must not be layered onto an already-coloured ThemedStyle — this style already carries a colour"
        );
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
        self.recolor(Color::Cyan)
    }

    pub fn red(self) -> Self {
        self.recolor(Color::Red)
    }

    pub fn green(self) -> Self {
        self.recolor(Color::Green)
    }

    pub fn yellow(self) -> Self {
        self.recolor(Color::Yellow)
    }

    /// Swap the foreground for a named colour, carrying the attribute set and
    /// the colour decision across. Rebuilding `inner` from scratch is what makes
    /// carrying explicit: a swap that dropped `colors` would leave a stamped
    /// theme with one silently unstyled slot. `bold` is the one attribute NOT
    /// carried across: bold never pairs with colour, so a style bolded while
    /// colourless drops bold the moment it gains a colour here, rather than
    /// silently landing in the paired state `with_attrs`'s own assert exists
    /// to refuse.
    fn recolor(self, color: Color) -> Self {
        let attrs = AttrSet {
            bold: false,
            ..self.attrs
        };
        Self::from_console_color(color)
            .with_attrs(attrs)
            .with_colors(self.colors)
    }

    /// Stamp whether this style may emit colour. `force_styling` is set in step
    /// with the flag because the 256-colour fallback arm of `StyledText::fmt`
    /// delegates to `console::Style::apply_to`, which otherwise re-consults the
    /// process-global colour flag and strips whatever this style decided.
    pub fn with_colors(mut self, enabled: bool) -> Self {
        self.colors = enabled;
        self.inner = self.inner.force_styling(enabled);
        self
    }

    fn with_attrs(mut self, attrs: AttrSet) -> Self {
        // The mirror of `bold()`'s own check, for the order `bold()` cannot
        // see: `recolor`/`apply_color` build a freshly-coloured style
        // (`has_color` already true here) and carry the PREVIOUS attribute
        // set across via this method, bypassing `bold()` entirely. Without
        // this, a colourless style bolded first and then recoloured (or a
        // bold-only preset slot later given a colour override) would gain
        // bold+colour with no assertion catching it — the same pairing
        // `bold()` refuses, reached from the other direction.
        debug_assert!(
            !(attrs.bold && self.has_color),
            "bold must not be paired with a coloured ThemedStyle in either composition order"
        );
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

    /// Wrap `text` for `Display` rendering. Resolved at format-time against the
    /// colour decision this style carries:
    ///
    /// - `colors` is false (NO_COLOR / TERM=dumb / structured output / not a
    ///   tty) AND no attrs → emit `text` with no escapes.
    /// - `colors` is false AND attrs are set → emit
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

        if !self.style.colors {
            // Colour off for the printer this style belongs to: emit attrs-only SGR (bold,
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

/// `Clone` so a printer derived from another (`Printer::at_verbosity`) inherits
/// the theme it was actually rendering with — presets, config overrides and the
/// colour stamp together — instead of rebuilding a preset from a name and
/// silently dropping `spec.theme.overrides`.
#[derive(Clone)]
pub struct Theme {
    /// Whether this theme's styles may emit colour, stamped by
    /// [`Theme::with_colors`]. Private, so a theme cannot be assembled with the
    /// slots and the decision disagreeing — every preset is built here and
    /// stamped by the `Printer` that will render through it.
    colors: bool,
    /// Whether a linked value may emit an OSC 8 hyperlink, stamped by
    /// [`Theme::with_hyperlinks`]. Held beside `colors` because the two are
    /// one decision: a hyperlink is an escape sequence, so a printer that may
    /// not emit colour may not emit one either. Private for the same reason
    /// `colors` is — a preset cannot be assembled with the slots and the
    /// decision disagreeing.
    hyperlinks: bool,

    // Style slots (14)
    /// Style for an action subject at the deepest level of the run tree.
    /// `None` means the subject keeps the role's own style — the correct
    /// answer for a preset with no palette foreground of its own.
    pub primary: Option<ThemedStyle>,
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
    /// "This is a schema TYPE" — the `<[]ModuleFileEntry>` span inside a
    /// `cfgd explain` field row and inside its drill-down heading. Its own
    /// slot rather than `accent`'s, which already means the Caveats heading
    /// and a title heading's value: each preset maps it to whatever its own
    /// syntax spec calls a type (Dracula's cyan-italic Types, Solarized's
    /// yellow `Type`), which is a different question from "attention without
    /// alarm". Drives no `Role` — a type is not an outcome, so the renderer
    /// names the slot directly.
    pub type_hint: ThemedStyle,

    // Icon slots (8)
    pub icon_ok: String,
    pub icon_warn: String,
    pub icon_fail: String,
    pub icon_pending: String,
    pub icon_running: String,
    pub icon_skipped: String,
    pub icon_arrow: String,
    pub icon_info: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: false,
            hyperlinks: false,
            // No palette foreground exists to spend here, and the terminal's
            // own default is the fall-through this slot exists to avoid — so
            // the subject keeps its role style.
            primary: None,
            header: ThemedStyle::plain().cyan(),
            success: ThemedStyle::plain().green(),
            warning: ThemedStyle::plain().yellow(),
            error: ThemedStyle::plain().red(),
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
            // The value `accent` carried when types shared its slot, so the
            // approved default look does not change with the split.
            type_hint: hex("#d78700").italic(),
            icon_ok: ICON_OK.into(),
            icon_warn: ICON_WARN.into(),
            icon_fail: ICON_FAIL.into(),
            icon_pending: ICON_PENDING.into(),
            icon_running: ICON_RUNNING.into(),
            icon_skipped: ICON_SKIPPED.into(),
            icon_arrow: ICON_ARROW.into(),
            icon_info: ICON_INFO.into(),
        }
    }
}

impl Theme {
    /// Stamp every style slot with the colour decision the renderer's owner
    /// took, and return the theme. This is the ONE place a theme learns whether
    /// it may emit colour: `StyledText` reads the stamp instead of
    /// `console::colors_enabled()`, so a printer's output cannot change because
    /// an unrelated thread flipped a process-global flag mid-render.
    pub fn with_colors(mut self, enabled: bool) -> Self {
        self.colors = enabled;
        // Colour withdrawn withdraws the hyperlink with it, whatever order the
        // two stamps arrive in: an OSC 8 sequence is an escape like any other.
        self.hyperlinks &= enabled;
        self.primary = self.primary.map(|s| s.with_colors(enabled));
        self.header = self.header.with_colors(enabled);
        self.success = self.success.with_colors(enabled);
        self.warning = self.warning.with_colors(enabled);
        self.error = self.error.with_colors(enabled);
        self.info = self.info.with_colors(enabled);
        self.muted = self.muted.with_colors(enabled);
        self.running = self.running.with_colors(enabled);
        self.diff_add = self.diff_add.with_colors(enabled);
        self.diff_remove = self.diff_remove.with_colors(enabled);
        self.diff_context = self.diff_context.with_colors(enabled);
        self.accent = self.accent.with_colors(enabled);
        self.secondary = self.secondary.with_colors(enabled);
        self.type_hint = self.type_hint.with_colors(enabled);
        self
    }

    /// Whether styles from this theme may emit colour.
    pub fn colors(&self) -> bool {
        self.colors
    }

    /// Stamp whether a linked value may emit an OSC 8 hyperlink. Only the
    /// production printer stamps `true`, and only when colour is on and the
    /// terminal is a known OSC 8 emitter ([`super::terminal_supports_hyperlinks`]);
    /// every capture leaves it off, so goldens stay ANSI-free.
    pub fn with_hyperlinks(mut self, enabled: bool) -> Self {
        self.hyperlinks = enabled && self.colors;
        self
    }

    /// Whether a linked value may emit an OSC 8 hyperlink.
    pub fn hyperlinks(&self) -> bool {
        self.hyperlinks
    }

    /// The ONE arrow glyph for a rendered `old -> new` relationship
    /// (`icon_arrow`, `"→"` by default, themeable per preset/config). Every
    /// caller composing such a string reaches it here instead of hardcoding
    /// ASCII `->`, so a preset override applies uniformly.
    pub fn arrow(&self) -> &str {
        &self.icon_arrow
    }

    /// Every preset name [`Theme::preset`] answers, in the order `--help`
    /// lists them. The ONE list: `--theme` / `cfgd init --theme` take their
    /// clap value parser from it, so an unknown name is refused at the flag
    /// rather than silently rendering the default palette.
    pub const PRESET_NAMES: &'static [&'static str] = &[
        "default",
        "dracula",
        "solarized-dark",
        "solarized-light",
        "nord",
        "monokai",
        "adventure-time",
        "catppuccin-mocha",
        "gruvbox-dark",
        "tokyo-night",
        "one-dark",
        "minimal",
    ];

    /// The preset called `name`, or `None` for a name not in
    /// [`Theme::PRESET_NAMES`].
    pub fn preset(name: &str) -> Option<Self> {
        Some(match name {
            "default" => Self::default(),
            "dracula" => Self::dracula(),
            "solarized-dark" => Self::solarized_dark(),
            "solarized-light" => Self::solarized_light(),
            "nord" => Self::nord(),
            "monokai" => Self::monokai(),
            "adventure-time" => Self::adventure_time(),
            "catppuccin-mocha" => Self::catppuccin_mocha(),
            "gruvbox-dark" => Self::gruvbox_dark(),
            "tokyo-night" => Self::tokyo_night(),
            "one-dark" => Self::one_dark(),
            "minimal" => Self::minimal(),
            _ => return None,
        })
    }

    /// [`Theme::preset`] for a name read out of a config file, where an
    /// unknown name falls back to the default palette: a printer has to
    /// exist before there is anything to report the bad name ON.
    pub fn from_preset(name: &str) -> Self {
        Self::preset(name).unwrap_or_default()
    }

    fn dracula() -> Self {
        Self {
            primary: Some(hex("#f8f8f2")),
            header: hex("#bd93f9"),
            success: hex("#50fa7b"),
            warning: hex("#f1fa8c"),
            error: hex("#ff5555"),
            info: hex("#8be9fd"),
            muted: hex("#6272a4"),
            running: hex("#8be9fd"),
            diff_add: hex("#50fa7b"),
            diff_remove: hex("#ff5555"),
            diff_context: hex("#6272a4"),
            accent: hex("#ffb86c"),
            secondary: hex("#ff79c6"),
            // spec.draculatheme.com maps Types to Cyan italic. Sharing the hex
            // with `info`/`running` is the preset's own pattern (green does
            // success and diff_add) and neither slot appears beside a type.
            type_hint: hex("#8be9fd").italic(),
            ..Self::default()
        }
    }

    fn solarized_dark() -> Self {
        Self {
            // base2 — solarized's own ANSI white, the canonical bright text
            // for the dark background.
            primary: Some(hex("#eee8d5")),
            // The vim canon's Title is BOLD ORANGE; colour never pairs with
            // bold here, and stripped of it the heading collides with accent
            // (#cb4b16). Blue is the canon-supported fallback: Directory,
            // MoreMsg and Identifier all map to it.
            header: hex("#268bd2"),
            success: hex("#859900"),
            // The vim canon's WarningMsg is BOLD RED, which stripped of bold
            // collides with error — ANSI yellow is the fallback.
            warning: hex("#b58900"),
            error: hex("#dc322f"),
            info: hex("#268bd2"),
            // Comment's COLOUR only: the vim canon italicizes comments, but
            // this slot also paints skipped/pending/diff-context rows, which
            // are not comments.
            muted: hex("#586e75"),
            running: hex("#2aa198"),
            diff_add: hex("#859900"),
            diff_remove: hex("#dc322f"),
            diff_context: hex("#586e75"),
            accent: hex("#cb4b16"),
            // The vim canon's Identifier is blue, which collides with
            // header/info — ANSI magenta is the fallback pivot.
            secondary: hex("#d33682"),
            // Solarized's own syntax mapping puts `Type` on yellow, unstyled
            // of attributes. It does not collide with the magenta field-name
            // column or the unstyled description beside it. Sharing `warning`'s
            // byte-identical yellow is deliberate and safe: the eight-colour
            // palette already doubles several slots by design, and the two
            // never meet — the surfaces that render a type slot emit no
            // warning lines.
            type_hint: hex("#b58900"),
            ..Self::default()
        }
    }

    fn solarized_light() -> Self {
        Self {
            // base02, not a light tone: on a light background the deliberate
            // contrast colour is the dark end of the palette — solarized's
            // own ANSI black.
            primary: Some(hex("#073642")),
            // Same stated fallback as the dark variant: canon Title is bold
            // orange, bold is banned, and blue is what Directory/MoreMsg/
            // Identifier carry.
            header: hex("#268bd2"),
            success: hex("#859900"),
            // Canon WarningMsg is bold red — collides error once bold is
            // stripped, so ANSI yellow.
            warning: hex("#b58900"),
            error: hex("#dc322f"),
            info: hex("#268bd2"),
            // Comment's colour only, without the vim canon's italic — this
            // slot also paints skipped/pending/diff-context rows.
            muted: hex("#93a1a1"),
            running: hex("#2aa198"),
            diff_add: hex("#859900"),
            diff_remove: hex("#dc322f"),
            diff_context: hex("#93a1a1"),
            accent: hex("#cb4b16"),
            // Canon Identifier is blue (collides header/info) — ANSI magenta
            // is the fallback pivot, as in the dark variant.
            secondary: hex("#d33682"),
            // Same official `Type` yellow as the dark variant: the palette is
            // one, and both backgrounds are designed against it. It shares
            // `warning`'s exact value for the same reason the dark variant
            // does — an eight-colour palette doubles slots by design, and no
            // surface renders a type beside a warning.
            type_hint: hex("#b58900"),
            ..Self::default()
        }
    }

    fn nord() -> Self {
        Self {
            // nord6: the spec's "plain text and syntax structures" for dark
            // ambiance designs.
            primary: Some(hex("#eceff4")),
            // nord8: "the bright and shiny primary accent color", and the
            // vim port's own markdownH1/asciidoc title colour.
            header: hex("#88c0d0"),
            success: hex("#a3be8c"), // nord14, success states + strings
            warning: hex("#ebcb8b"), // nord13, warning states
            error: hex("#bf616a"),   // nord11, error states
            info: hex("#81a1c1"),    // nord9
            muted: hex("#4c566a"),   // nord3, comments
            running: hex("#88c0d0"),
            diff_add: hex("#a3be8c"),    // DiffAdd/diffAdded, nord14
            diff_remove: hex("#bf616a"), // DiffDelete/diffRemoved, nord11
            diff_context: hex("#4c566a"),
            // nord12, "annotations and decorators" — the palette's one orange.
            accent: hex("#d08770"),
            // nord15 — nord has no pink; its purple is the label pivot.
            secondary: hex("#b48ead"),
            // nord7, "used for classes, types and primitives" (spec verbatim).
            // The spec's own types-vs-functions distinction is nord7 vs nord8,
            // so sitting one step from `header` is the palette's design, not a
            // collision. No attribute: the spec gives colour roles only.
            type_hint: hex("#8fbcbb"),
            ..Self::default()
        }
    }

    fn monokai() -> Self {
        Self {
            primary: Some(hex("#f8f8f2")),
            // markup.heading is green BOLD, and colour never pairs with bold
            // here — stripped of it the heading collides with success on the
            // same screen, so the slot falls back to the constants purple:
            // the one palette colour no other slot spends.
            header: hex("#ae81ff"),
            success: hex("#a6e22e"),
            warning: hex("#e6db74"),
            // The shipped theme's own `invalid` foreground — classic Monokai
            // has no red apart from the keyword pink, which `secondary` owns.
            error: hex("#f44747"),
            info: hex("#66d9ef"),
            muted: hex("#88846f"), // comment
            running: hex("#66d9ef"),
            diff_add: hex("#a6e22e"), // markup.inserted
            // markup.deleted — the keyword pink doubles as the deletion
            // colour in the shipped theme, and a diff body renders no
            // secondary labels beside it.
            diff_remove: hex("#f92672"),
            diff_context: hex("#88846f"),
            accent: hex("#fd971f"),    // the parameter orange
            secondary: hex("#f92672"), // the keyword pink — Monokai's signature
            // storage.type is cyan italic in the shipped theme; sharing
            // info/running's hex is the same pattern dracula documents.
            type_hint: hex("#66d9ef").italic(),
            ..Self::default()
        }
    }

    fn adventure_time() -> Self {
        Self {
            primary: Some(hex("#f8dcc0")), // terminal foreground
            header: hex("#1997c6"),        // bright blue
            success: hex("#9eff6e"),       // bright green
            warning: hex("#efc11a"),       // bright yellow
            error: hex("#fc5f5a"),         // bright red
            info: hex("#70a598"),          // cyan
            muted: hex("#4e7cbf"),         // bright black
            running: hex("#70a598"),
            diff_add: hex("#9eff6e"),
            diff_remove: hex("#fc5f5a"),
            diff_context: hex("#4e7cbf"),
            // The palette's ansi-yellow slot is literally orange.
            accent: hex("#e7741e"),
            secondary: hex("#665993"), // purple
            // A terminal palette carries no syntax spec, so this preset takes
            // the stated fallback: the accent value, zero visual change from
            // the pre-split behaviour.
            type_hint: hex("#e7741e"),
            ..Self::default()
        }
    }

    fn catppuccin_mocha() -> Self {
        Self {
            primary: Some(hex("#cdd6f4")), // text
            header: hex("#cba6f7"),        // mauve
            success: hex("#a6e3a1"),       // green
            warning: hex("#f9e2af"),       // yellow
            error: hex("#f38ba8"),         // red
            info: hex("#94e2d5"),          // teal, the guide's "Information"
            muted: hex("#9399b2"),         // overlay2, the guide's comment colour
            running: hex("#89dceb"),       // sky — no guide role names a spinner
            diff_add: hex("#a6e3a1"),
            diff_remove: hex("#f38ba8"),
            diff_context: hex("#9399b2"),
            accent: hex("#fab387"),    // peach
            secondary: hex("#f5c2e7"), // pink
            // Style guide: "Classes, Interfaces, ... Types" → Yellow. Sharing
            // warning's exact value is the same deliberate doubling both
            // Solarized variants document; the guide assigns colours only, so
            // no attribute rides along.
            type_hint: hex("#f9e2af"),
            ..Self::default()
        }
    }

    fn gruvbox_dark() -> Self {
        Self {
            primary: Some(hex("#ebdbb2")), // fg1
            // Title/Directory are green BOLD, and colour never pairs with
            // bold here — stripped of it the heading collides with success,
            // so the slot falls back to the one bright colour no outcome
            // row spends.
            header: hex("#83a598"),  // bright_blue
            success: hex("#b8bb26"), // bright_green
            warning: hex("#fabd2f"), // bright_yellow
            error: hex("#fb4934"),   // bright_red
            info: hex("#83a598"),
            muted: hex("#928374"),       // gray, the scheme's Comment colour
            running: hex("#8ec07c"),     // bright_aqua
            diff_add: hex("#b8bb26"),    // diffAdded → GruvboxGreen
            diff_remove: hex("#fb4934"), // diffRemoved → GruvboxRed
            diff_context: hex("#928374"),
            accent: hex("#fe8019"),    // bright_orange, the scheme's signature
            secondary: hex("#d3869b"), // bright_purple
            // gruvbox.vim: `hi! link Type GruvboxYellow`, no attribute. Shares
            // warning's value the way both Solarized variants do.
            type_hint: hex("#fabd2f"),
            ..Self::default()
        }
    }

    fn tokyo_night() -> Self {
        Self {
            primary: Some(hex("#a9b1d6")), // editor foreground
            // The reference maps Markdown headings to the terminal cyan;
            // doubling `info` is the same pairing the default preset ships.
            header: hex("#7dcfff"),
            success: hex("#73daca"), // terminal green
            warning: hex("#e0af68"), // terminal yellow
            error: hex("#f7768e"),   // terminal red
            info: hex("#7dcfff"),    // terminal cyan
            muted: hex("#565f89"),   // comments
            running: hex("#7dcfff"),
            diff_add: hex("#73daca"),
            diff_remove: hex("#f7768e"),
            diff_context: hex("#565f89"),
            accent: hex("#ff9e64"),    // number/boolean constants orange
            secondary: hex("#bb9af7"), // terminal magenta, control keywords
            // The reference maps "Storage Types" to the same magenta
            // `secondary` owns — byte-identical to the field-name column a
            // type span sits beside, so that mapping collides. Class names
            // map to the variables white, which reads unstyled. With no
            // non-colliding official mapping the slot keeps its accent value.
            type_hint: hex("#ff9e64"),
            ..Self::default()
        }
    }

    fn one_dark() -> Self {
        Self {
            primary: Some(hex("#abb2bf")), // mono-1, default text
            // markup.heading is hue-5, byte-identical to `error` — colliding
            // on any failing report — so the slot takes hue-2, the colour
            // the scheme's other heading/identity scopes carry.
            header: hex("#61afef"),
            success: hex("#98c379"), // hue-4 green
            warning: hex("#e5c07b"), // hue-6-2
            error: hex("#e06c75"),   // hue-5 red
            info: hex("#56b6c2"),    // hue-1 cyan
            muted: hex("#5c6370"),   // mono-3, comments
            running: hex("#56b6c2"),
            diff_add: hex("#98c379"),    // markup.inserted → hue-4
            diff_remove: hex("#e06c75"), // markup.deleted → hue-5
            diff_context: hex("#5c6370"),
            accent: hex("#d19a66"),    // hue-6 orange, numeric constants
            secondary: hex("#c678dd"), // hue-3 purple, keywords
            // one-dark-syntax maps entity.name.type.class to hue-6-2; the
            // only italic in its base mapping is comments, so none here.
            // Shares warning's value the way both Solarized variants do.
            type_hint: hex("#e5c07b"),
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
        if let Some(c) = &ov.primary
            && parse_hex_rgb(c).is_some()
        {
            // The slot is optional, so an override on a preset that answers
            // `None` fills it rather than adjusting an existing colour.
            apply_color(t.primary.get_or_insert_with(ThemedStyle::plain), c);
        }
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
        if let Some(c) = &ov.type_hint {
            apply_color(&mut t.type_hint, c);
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
        if let Some(v) = &ov.icon_info {
            t.icon_info = v.clone();
        }
        t
    }

    fn minimal() -> Self {
        Self {
            colors: false,
            hyperlinks: false,
            // minimal spends no colour at all.
            primary: None,
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
            // No colour to spend on a type either, and italic beside the
            // underlined field-name column is the distinction that survives.
            type_hint: ThemedStyle::plain().italic(),
            icon_ok: "+".into(),
            icon_warn: "!".into(),
            icon_fail: "x".into(),
            icon_pending: " ".into(),
            icon_running: ".".into(),
            icon_skipped: "-".into(),
            icon_arrow: ">".into(),
            icon_info: "i".into(),
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
/// The six values xterm's 6×6×6 colour cube actually uses per channel. They
/// are not evenly spaced — the gap from 0 to 95 is nearly three times the gap
/// between any later pair — so a channel cannot be mapped onto them by
/// division.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Cube index whose level is closest to `v`.
fn nearest_cube_index(v: u8) -> usize {
    CUBE_LEVELS
        .iter()
        .enumerate()
        .min_by_key(|(_, level)| v.abs_diff(**level))
        .map_or(0, |(i, _)| i)
}

/// Squared euclidean distance between two RGB triples.
fn rgb_dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| {
        let d = u32::from(x.abs_diff(y));
        d * d
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// 256-color fallback path when the terminal does not advertise truecolor
/// support. Quantizes to the nearer of xterm's 6×6×6 cube and its 24-step
/// grayscale ramp.
pub(super) fn ansi256_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    let (ri, gi, bi) = (
        nearest_cube_index(r),
        nearest_cube_index(g),
        nearest_cube_index(b),
    );
    let cube_rgb = (CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);
    let cube_idx = (16 + 36 * ri + 6 * gi + bi) as u8;

    // Both candidates are measured rather than branching on `r == g == b`: the
    // ramp's 10-unit steps beat the cube's coarse levels for anything merely
    // near-grey, not just exactly grey.
    let avg = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let ramp_i = u8::try_from(avg.saturating_sub(3) / 10)
        .unwrap_or(23)
        .min(23);
    let ramp_level = 8 + 10 * ramp_i;
    let ramp_rgb = (ramp_level, ramp_level, ramp_level);

    if rgb_dist2(ramp_rgb, (r, g, b)) < rgb_dist2(cube_rgb, (r, g, b)) {
        232 + ramp_i
    } else {
        cube_idx
    }
}

fn hex(s: &str) -> ThemedStyle {
    ThemedStyle::from_hex(s)
}

fn apply_color(style: &mut ThemedStyle, hex: &str) {
    if let Some((r, g, b)) = parse_hex_rgb(hex) {
        // `bold` is dropped, not carried: a user overriding a colourless
        // bold-only slot's colour (`minimal`'s header/error) is moving that
        // slot INTO the coloured population, where bold never pairs with
        // colour — the same rule any other preset's slot already carries.
        let attrs = AttrSet {
            bold: false,
            ..style.attrs
        };
        // The colour decision belongs to the printer, not to the palette an
        // override names, so it survives the slot being rebuilt.
        let colors = style.colors;
        *style = ThemedStyle {
            inner: Style::new().fg(Color::Color256(ansi256_from_rgb(r, g, b))),
            rgb: Some((r, g, b)),
            attrs: AttrSet::default(),
            colors: false,
            has_color: true,
        }
        .with_attrs(attrs)
        .with_colors(colors);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputFormat;
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
        assert_eq!(t.icon_skipped, "∅");
        assert_eq!(t.icon_arrow, "→");
        assert_eq!(t.icon_info, "◉");
    }

    /// Every default glyph must be text-presentation AND drawn from a block the
    /// fonts terminals actually ship. An emoji-presentation character renders
    /// double-width and color-substituted; a character outside these blocks
    /// renders as a tofu box, which is how `ⓘ` (U+24D8, Enclosed
    /// Alphanumerics — absent from JetBrains Mono, DejaVu and Noto Sans Mono
    /// alike) reached a recorded demo. Either way the status-line glyph column
    /// that the whole icon set exists to align is broken.
    #[test]
    fn default_icons_are_all_text_presentation_single_glyphs() {
        // Latin-1 Supplement, General Punctuation, Arrows, Mathematical
        // Operators, Geometric Shapes, Miscellaneous Symbols, Dingbats — the
        // ranges with near-universal coverage in monospaced terminal fonts.
        const COVERED_BLOCKS: &[std::ops::RangeInclusive<u32>] = &[
            0x00A0..=0x00FF,
            0x2000..=0x206F,
            0x2190..=0x21FF,
            0x2200..=0x22FF,
            0x25A0..=0x25FF,
            0x2600..=0x26FF,
            0x2700..=0x27BF,
        ];

        let t = Theme::default();
        for icon in [
            &t.icon_ok,
            &t.icon_warn,
            &t.icon_fail,
            &t.icon_pending,
            &t.icon_running,
            &t.icon_skipped,
            &t.icon_arrow,
            &t.icon_info,
        ] {
            assert_eq!(icon.chars().count(), 1, "{icon:?} is not a single glyph");
            let c = icon.chars().next().unwrap_or('\0') as u32;
            // U+FE0F would force emoji presentation; the emoji-source blocks
            // (Misc Symbols & Pictographs onward) are excluded outright.
            assert!(
                !icon.contains('\u{FE0F}'),
                "{icon:?} forces emoji presentation"
            );
            assert!(c < 0x1F300, "{icon:?} is from an emoji block");
            assert!(
                COVERED_BLOCKS.iter().any(|r| r.contains(&c)),
                "{icon:?} (U+{c:04X}) is outside the blocks terminal fonts cover \
                 and will render as a tofu box"
            );
        }
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
        assert!(Theme::preset("not-a-real-preset").is_none());
        let t = Theme::from_preset("not-a-real-preset");
        assert_eq!(t.icon_ok, "✓"); // matches default
    }

    #[test]
    fn every_listed_preset_name_resolves() {
        // PRESET_NAMES feeds the `--theme` value parser, so a name listed
        // here that `preset` does not answer is a flag value clap accepts
        // and the renderer silently ignores.
        for name in Theme::PRESET_NAMES {
            assert!(Theme::preset(name).is_some(), "{name} has no preset arm");
        }
    }

    /// One glyph, one meaning — across every preset, not just the default.
    ///
    /// Two roles sharing a glyph makes the icon column unreadable, and the
    /// renderer's own `—` glue (the mark that says a detail follows) is
    /// reserved: `Role::Skipped` wore it, so every skipped line read as a
    /// subject with an empty detail.
    #[test]
    fn no_preset_spells_two_meanings_with_one_glyph() {
        const GLUE_DASH: &str = "—";
        for name in Theme::PRESET_NAMES {
            let Some(t) = Theme::preset(name) else {
                continue;
            };
            let icons = [
                ("ok", &t.icon_ok),
                ("warn", &t.icon_warn),
                ("fail", &t.icon_fail),
                ("pending", &t.icon_pending),
                ("running", &t.icon_running),
                ("skipped", &t.icon_skipped),
                ("arrow", &t.icon_arrow),
                ("info", &t.icon_info),
            ];
            for (i, (role, glyph)) in icons.iter().enumerate() {
                assert_ne!(
                    glyph.as_str(),
                    GLUE_DASH,
                    "{name}: {role} wears the renderer's detail glue"
                );
                for (other, other_glyph) in icons.iter().skip(i + 1) {
                    assert_ne!(
                        glyph, other_glyph,
                        "{name}: {role} and {other} are spelled the same"
                    );
                }
            }
        }
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
        let style = ThemedStyle::from_hex("#bd93f9").with_colors(true);
        let out = style.apply_to("hi").to_string();
        assert_eq!(out, "\x1b[38;2;189;147;249mhi\x1b[0m", "got: {out:?}");
    }

    /// Builds the same style `from_hex(hex).bold()` produced when a preset
    /// still paired bold with colour — bypassing the guarded `bold()` method
    /// directly, via the private
    /// fields this same-file test module can still reach — so the SGR
    /// composition of an attr with a truecolor foreground stays proven even
    /// though no theme preset is allowed to construct that pairing anymore.
    fn colored_then_bolded(hex: &str) -> ThemedStyle {
        let mut style = ThemedStyle::from_hex(hex);
        style.inner = style.inner.bold();
        style.attrs.bold = true;
        style
    }

    #[test]
    #[serial]
    fn hex_style_with_bold_emits_truecolor_with_attr() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let style = colored_then_bolded("#bd93f9").with_colors(true);
        let out = style.apply_to("hi").to_string();
        assert_eq!(out, "\x1b[1;38;2;189;147;249mhi\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn hex_style_falls_back_to_256_when_no_truecolor() {
        let _no_color = EnvVarGuard::unset("NO_COLOR");
        let _ct = EnvVarGuard::unset("COLORTERM");
        let style = ThemedStyle::from_hex("#bd93f9").with_colors(true);
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
        // The two halves of the NO_COLOR contract, joined: the printer's own
        // decision function answers "no colour", and the style stamped with
        // that answer still emits its attrs — bold is independent of colour
        // per no-color.org.
        let colors = !crate::output::printer::colors_must_be_disabled(&OutputFormat::Table);
        assert!(!colors, "NO_COLOR must rule colour out");
        let style = colored_then_bolded("#bd93f9").with_colors(colors);
        let out = style.apply_to("hi").to_string();
        assert_eq!(out, "\x1b[1mhi\x1b[0m", "got: {out:?}");
    }

    /// The colour-off decision, taken the way a printer takes it, so the env
    /// var below is load-bearing: an unstamped `ThemedStyle` spends no colour
    /// whatever `NO_COLOR` says, and a test that skipped this step asserted
    /// nothing about the contract it was named for.
    #[track_caller]
    fn no_color_decision() -> bool {
        let colors = !crate::output::printer::colors_must_be_disabled(&OutputFormat::Table);
        assert!(!colors, "NO_COLOR must rule colour out");
        colors
    }

    #[test]
    #[serial]
    fn no_color_keeps_italic_for_default_accent() {
        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        // Matches the `default` preset's accent slot: hex("#d78700").italic()
        let style = ThemedStyle::from_hex("#d78700")
            .italic()
            .with_colors(no_color_decision());
        let out = style.apply_to("x").to_string();
        assert_eq!(out, "\x1b[3mx\x1b[0m", "got: {out:?}");
    }

    #[test]
    #[serial]
    fn no_color_emits_no_escapes_when_no_attrs() {
        let _ct = EnvVarGuard::set("COLORTERM", "truecolor");
        let _no_color = EnvVarGuard::set("NO_COLOR", "1");
        let colors = no_color_decision();
        let out = ThemedStyle::plain()
            .with_colors(colors)
            .apply_to("x")
            .to_string();
        assert_eq!(out, "x", "got: {out:?}");
        // Hex without attrs also emits no escapes when colors are off.
        let out2 = ThemedStyle::from_hex("#bd93f9")
            .with_colors(colors)
            .apply_to("y")
            .to_string();
        assert_eq!(out2, "y", "got: {out2:?}");
    }

    /// Every SGR parameter `styled` sets, in the order they appear, however
    /// they are grouped: the colour-off path joins its attrs into one sequence
    /// (`\x1b[1;3m`) while the colour-on path spends one sequence per attr
    /// (`\x1b[1m\x1b[3m`). Both are the same terminal state, and an assertion
    /// about which attrs are set must not read as an assertion about grouping.
    fn sgr_params(styled: &str) -> Vec<u16> {
        styled
            .split("\u{1b}[")
            .skip(1)
            .filter_map(|seq| seq.split_once('m').map(|(params, _)| params))
            .flat_map(|params| params.split(';').filter_map(|p| p.parse::<u16>().ok()))
            .filter(|n| *n != 0)
            .collect()
    }

    /// Attributes are independent of the colour decision — `NO_COLOR` governs
    /// colour only, per no-color.org. Asserted against BOTH stamps rather than
    /// against an env var, because a plain style carries no colour to strip and
    /// so cannot tell the two decisions apart on its own.
    #[test]
    fn attrs_survive_either_colour_decision() {
        for colors in [false, true] {
            let bold = ThemedStyle::plain()
                .bold()
                .with_colors(colors)
                .apply_to("x")
                .to_string();
            assert_eq!(sgr_params(&bold), [1], "colors={colors}, got: {bold:?}");
            assert_eq!(crate::output::strip_ansi(&bold), "x");

            // Matches the `minimal` preset's secondary slot.
            let underlined = ThemedStyle::plain()
                .underlined()
                .with_colors(colors)
                .apply_to("x")
                .to_string();
            assert_eq!(
                sgr_params(&underlined),
                [4],
                "colors={colors}, got: {underlined:?}"
            );

            let joined = ThemedStyle::plain()
                .bold()
                .italic()
                .with_colors(colors)
                .apply_to("x")
                .to_string();
            assert_eq!(
                sgr_params(&joined),
                [1, 3],
                "colors={colors}, got: {joined:?}"
            );
        }

        // The colour-off path is the one this module owns, so its exact wire
        // form is pinned: multiple attrs join into a single sequence.
        let joined_off = ThemedStyle::plain()
            .bold()
            .italic()
            .with_colors(false)
            .apply_to("x")
            .to_string();
        assert_eq!(joined_off, "\x1b[1;3mx\x1b[0m", "got: {joined_off:?}");
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
    fn with_attrs_drops_bold_but_preserves_other_attrs_through_yellow_swap() {
        // Bold never pairs with colour: unlike dim/italic/underline, it does
        // not survive a colourless style gaining a colour — that composition
        // is exactly the one `with_attrs`'s own assert refuses.
        let s = ThemedStyle::plain()
            .bold()
            .dim()
            .italic()
            .underlined()
            .yellow();
        assert!(!s.attrs.bold);
        assert!(s.attrs.dim);
        assert!(s.attrs.italic);
        assert!(s.attrs.underline);
    }

    /// Reproduces the exact composition order the historical defect shipped
    /// as (`ThemedStyle::plain().bold().cyan()`, once a real preset's
    /// `header` slot) and proves it can no longer land bold+colour: the
    /// style still gains its colour, only bold is the casualty.
    #[test]
    fn bold_then_colour_composition_yields_coloured_not_bold() {
        let s = ThemedStyle::plain().bold().cyan();
        assert!(s.has_color, "the colour swap must still take effect");
        assert!(
            !s.attrs.bold,
            "bold-then-colour must not silently produce a bold coloured style"
        );
    }

    /// The RGB a terminal actually paints for one of the 240 addressable
    /// slots: 16..=231 is the 6×6×6 cube, 232..=255 the grayscale ramp.
    fn slot_rgb(slot: u8) -> (u8, u8, u8) {
        if slot >= 232 {
            let level = 8 + 10 * (slot - 232);
            return (level, level, level);
        }
        let i = usize::from(slot - 16);
        (
            CUBE_LEVELS[i / 36],
            CUBE_LEVELS[(i / 6) % 6],
            CUBE_LEVELS[i % 6],
        )
    }

    /// The property that matters, asserted directly rather than through the
    /// algorithm that satisfies it: no addressable slot is closer to the
    /// requested colour than the one chosen. An earlier implementation divided
    /// each channel by 51 to index the cube, which systematically rounded down
    /// — #50fa7b's green landed on 215 with 255 available — and no
    /// membership-style assertion could see it.
    fn assert_nearest_slot(r: u8, g: u8, b: u8) {
        let chosen = ansi256_from_rgb(r, g, b);
        let chosen_dist = rgb_dist2(slot_rgb(chosen), (r, g, b));
        for slot in 16..=255u8 {
            let d = rgb_dist2(slot_rgb(slot), (r, g, b));
            assert!(
                d >= chosen_dist,
                "slot {slot} ({:?}) is nearer to ({r},{g},{b}) than chosen {chosen} ({:?})",
                slot_rgb(slot),
                slot_rgb(chosen),
            );
        }
    }

    #[test]
    fn ansi256_always_picks_the_nearest_addressable_slot() {
        for (r, g, b) in [
            (0, 0, 0),
            (7, 7, 7),
            (8, 8, 8),
            (128, 128, 128),
            (248, 248, 248),
            (249, 249, 249),
            (255, 255, 255),
            // Every dracula slot — the preset this is most visible on.
            (0xbd, 0x93, 0xf9),
            (0x50, 0xfa, 0x7b),
            (0xf1, 0xfa, 0x8c),
            (0xff, 0x55, 0x55),
            (0x8b, 0xe9, 0xfd),
            (0x62, 0x72, 0xa4),
            (0xff, 0xb8, 0x6c),
            (0xff, 0x79, 0xc6),
        ] {
            assert_nearest_slot(r, g, b);
        }
    }

    #[test]
    fn ansi256_pure_black_and_white_are_exact() {
        assert_eq!(ansi256_from_rgb(0, 0, 0), 16);
        assert_eq!(ansi256_from_rgb(255, 255, 255), 231);
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
    fn from_config_style_overrides_apply_all_thirteen_slots() {
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
                type_hint: Some("#252627".into()),
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
        assert_eq!(t.type_hint.rgb, Some((0x25, 0x26, 0x27)));
    }

    #[test]
    fn from_config_style_override_on_a_bold_only_slot_drops_bold() {
        // Minimal's `error` slot is plain().bold() — bold stands in for the
        // colour minimal spends none of. Overriding the colour moves that
        // slot into the coloured population, where bold never pairs with
        // colour: the override must apply the new colour AND drop bold,
        // never combine both.
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
            !t.error.attrs.bold,
            "a colour override must drop bold from a slot it colours, never pair the two"
        );
    }

    #[test]
    fn from_config_icon_overrides_apply_all_eight_slots() {
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
                icon_info: Some("[i]".into()),
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
        assert_eq!(t.icon_info, "[i]");
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
        // Only the muted/diff_context and primary slots differ between
        // solarized-dark and solarized-light; everything else matches.
        assert_ne!(dark.muted.rgb, light.muted.rgb);
        assert_eq!(light.muted.rgb, Some((0x93, 0xa1, 0xa1)));
        // The foreground the two palettes are deliberate about is the one slot
        // that has to invert with the background.
        assert_ne!(
            dark.primary.as_ref().and_then(|s| s.rgb),
            light.primary.as_ref().and_then(|s| s.rgb)
        );
        assert_eq!(
            light.primary.as_ref().and_then(|s| s.rgb),
            Some((0x07, 0x36, 0x42))
        );
        assert_eq!(dark.success.rgb, light.success.rgb);
    }

    #[test]
    fn primary_slot_differs_per_preset() {
        let repr = |t: &Theme| format!("{:?}", t.primary);
        let dracula = Theme::from_preset("dracula");
        let sol_dark = Theme::from_preset("solarized-dark");
        let sol_light = Theme::from_preset("solarized-light");
        assert_ne!(repr(&dracula), repr(&sol_dark));
        assert_ne!(repr(&sol_dark), repr(&sol_light));
        assert_ne!(repr(&dracula), repr(&sol_light));
    }

    /// The fence for the claim that the majority path is unchanged: with no
    /// primary style, an action subject keeps the role's own style.
    #[test]
    fn primary_is_none_for_default_and_minimal() {
        assert!(Theme::default().primary.is_none());
        assert!(Theme::from_preset("minimal").primary.is_none());
    }

    #[test]
    fn primary_override_fills_the_slot_on_a_preset_that_has_none() {
        let cfg = crate::config::ThemeConfig {
            name: "default".into(),
            overrides: crate::config::ThemeOverrides {
                primary: Some("#ff0000".into()),
                ..Default::default()
            },
        };
        let t = Theme::from_config(Some(&cfg));
        assert_eq!(
            t.primary.as_ref().and_then(|s| s.rgb),
            Some((0xff, 0x00, 0x00))
        );
    }

    /// Bold never pairs with colour. Walks every style slot of the four
    /// named colour presets rather than pinning one hand-picked slot (header
    /// and error are the ones that regressed; a slot-by-slot walk catches a
    /// future preset author reintroducing the pairing on any slot, not just
    /// those two).
    #[test]
    fn named_colour_presets_never_pair_bold_with_colour() {
        for name in [
            "default",
            "dracula",
            "solarized-dark",
            "solarized-light",
            "nord",
            "monokai",
            "adventure-time",
            "catppuccin-mocha",
            "gruvbox-dark",
            "tokyo-night",
            "one-dark",
        ] {
            let t = Theme::from_preset(name);
            let mut slots: Vec<(&str, &ThemedStyle)> = vec![
                ("header", &t.header),
                ("success", &t.success),
                ("warning", &t.warning),
                ("error", &t.error),
                ("info", &t.info),
                ("muted", &t.muted),
                ("running", &t.running),
                ("diff_add", &t.diff_add),
                ("diff_remove", &t.diff_remove),
                ("diff_context", &t.diff_context),
                ("accent", &t.accent),
                ("secondary", &t.secondary),
                ("type_hint", &t.type_hint),
            ];
            if let Some(p) = &t.primary {
                slots.push(("primary", p));
            }
            for (label, style) in slots {
                assert!(
                    !(style.has_color && style.attrs.bold),
                    "{name}'s {label} slot pairs bold with colour"
                );
            }
        }
    }

    /// `minimal` spends no colour at all, so bold on header/error is its
    /// colour distinction rather than a forbidden pairing — the one preset
    /// that is a legitimate exception to the bold-never-pairs-with-colour rule.
    #[test]
    fn minimal_preset_keeps_bold_since_it_spends_no_colour() {
        let t = Theme::from_preset("minimal");
        assert!(t.header.attrs.bold, "minimal's header must stay bold");
        assert!(t.error.attrs.bold, "minimal's error must stay bold");
        assert!(
            !t.header.has_color,
            "minimal must not spend colour anywhere"
        );
        assert!(!t.error.has_color, "minimal must not spend colour anywhere");
    }

    /// Each preset's type slot is a deliberate pick, pinned so a palette edit
    /// has to move it on purpose: `default` keeps the exact value `accent`
    /// carried while types shared its slot, `dracula` takes the cyan-italic
    /// its own spec maps Types to, and both Solarized variants take the
    /// official `Type` yellow.
    #[test]
    fn every_preset_type_hint_is_pinned() {
        let default = Theme::from_preset("default");
        assert_eq!(default.type_hint.rgb, Some((0xd7, 0x87, 0x00)));
        assert!(default.type_hint.attrs.italic);
        assert_eq!(
            default.type_hint.rgb, default.accent.rgb,
            "the default preset's look must not change with the split"
        );

        let dracula = Theme::from_preset("dracula");
        assert_eq!(dracula.type_hint.rgb, Some((0x8b, 0xe9, 0xfd)));
        assert!(dracula.type_hint.attrs.italic);

        for name in ["solarized-dark", "solarized-light"] {
            let t = Theme::from_preset(name);
            assert_eq!(t.type_hint.rgb, Some((0xb5, 0x89, 0x00)), "{name}");
            assert!(!t.type_hint.attrs.italic, "{name}");
        }

        // nord's spec maps types to nord7 teal, colour only.
        let nord = Theme::from_preset("nord");
        assert_eq!(nord.type_hint.rgb, Some((0x8f, 0xbc, 0xbb)));
        assert!(!nord.type_hint.attrs.italic);

        // monokai's shipped theme maps storage.type to cyan ITALIC — the one
        // new preset whose canon carries the attribute.
        let monokai = Theme::from_preset("monokai");
        assert_eq!(monokai.type_hint.rgb, Some((0x66, 0xd9, 0xef)));
        assert!(monokai.type_hint.attrs.italic);

        // catppuccin, gruvbox and one-dark each map types to their own
        // yellow, colour only.
        for (name, rgb) in [
            ("catppuccin-mocha", (0xf9, 0xe2, 0xaf)),
            ("gruvbox-dark", (0xfa, 0xbd, 0x2f)),
            ("one-dark", (0xe5, 0xc0, 0x7b)),
        ] {
            let t = Theme::from_preset(name);
            assert_eq!(t.type_hint.rgb, Some(rgb), "{name}");
            assert!(!t.type_hint.attrs.italic, "{name}");
        }

        // adventure-time has no syntax spec and tokyo-night's official type
        // mappings all collide — both take the stated fallback: the accent
        // value, unattributed.
        for name in ["adventure-time", "tokyo-night"] {
            let t = Theme::from_preset(name);
            assert_eq!(t.type_hint.rgb, t.accent.rgb, "{name}");
            assert!(!t.type_hint.attrs.italic, "{name}");
        }

        let minimal = Theme::from_preset("minimal");
        assert!(!minimal.type_hint.has_color);
        assert!(minimal.type_hint.attrs.italic);
    }

    /// Each new preset's palette is pinned on the slots nothing else pins
    /// (accent/secondary live in themes_raw.rs, type_hint above), so a hex
    /// edit has to move a test on purpose.
    #[test]
    fn new_preset_palettes_pin_their_distinctive_slots() {
        for (name, primary, header, muted) in [
            (
                "nord",
                (0xec, 0xef, 0xf4),
                (0x88, 0xc0, 0xd0),
                (0x4c, 0x56, 0x6a),
            ),
            (
                "monokai",
                (0xf8, 0xf8, 0xf2),
                (0xae, 0x81, 0xff),
                (0x88, 0x84, 0x6f),
            ),
            (
                "adventure-time",
                (0xf8, 0xdc, 0xc0),
                (0x19, 0x97, 0xc6),
                (0x4e, 0x7c, 0xbf),
            ),
            (
                "catppuccin-mocha",
                (0xcd, 0xd6, 0xf4),
                (0xcb, 0xa6, 0xf7),
                (0x93, 0x99, 0xb2),
            ),
            (
                "gruvbox-dark",
                (0xeb, 0xdb, 0xb2),
                (0x83, 0xa5, 0x98),
                (0x92, 0x83, 0x74),
            ),
            (
                "tokyo-night",
                (0xa9, 0xb1, 0xd6),
                (0x7d, 0xcf, 0xff),
                (0x56, 0x5f, 0x89),
            ),
            (
                "one-dark",
                (0xab, 0xb2, 0xbf),
                (0x61, 0xaf, 0xef),
                (0x5c, 0x63, 0x70),
            ),
        ] {
            let t = Theme::from_preset(name);
            assert_eq!(
                t.primary.as_ref().and_then(|s| s.rgb),
                Some(primary),
                "{name} primary"
            );
            assert_eq!(t.header.rgb, Some(header), "{name} header");
            assert_eq!(t.muted.rgb, Some(muted), "{name} muted");
        }
    }
}
