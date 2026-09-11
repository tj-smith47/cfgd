# Bundled TextMate themes

Each file here is an unmodified upstream `.tmTheme`, fetched at the commit named
below and embedded with `include_str!` by `output/theme.rs`, which parses them
once per process and hands one to `Printer::syntax_highlight`. The preset that
reads each file is in the table on `Theme`'s rustdoc; `default`,
`adventure-time`, `solarized-dark` and `solarized-light` read syntect's own
built-ins instead, and `minimal` renders a code block plain.

| File | Preset | Upstream | Commit | Licence |
|---|---|---|---|---|
| `dracula.tmTheme` | `dracula` | https://github.com/dracula/sublime (`Dracula.tmTheme`) | `d490b57c08f3d110ff61a07ec6edcc1ed9e24a63` | MIT |
| `nord.tmTheme` | `nord` | https://github.com/crabique/Nord-plist (`Nord.tmTheme`), the plist distribution of https://github.com/arcticicestudio/nord-sublime-text | `bf92a9e4457dc2f97efebc59bbeac95933ec6515` | MIT |
| `monokai-extended.tmTheme` | `monokai` | https://github.com/jonschlinkert/sublime-monokai-extended (`Monokai Extended.tmTheme`) | `0ca4e75291515c4d47e2d455e598e03e0dc53745` | MIT |
| `gruvbox-dark.tmTheme` | `gruvbox-dark` | https://github.com/subnut/gruvbox-tmTheme (`gruvbox (Dark) (Medium).tmTheme`) | `429749e29c84724b72b71a80dfdd67be2e0bc506` | MIT |
| `tokyo-night.tmTheme` | `tokyo-night` | https://github.com/folke/tokyonight.nvim (`extras/sublime/tokyonight_night.tmTheme`) | `cdc07ac78467a233fd62c493de29a17e0cf2b2b6` | Apache-2.0 |
| `one-dark.tmTheme` | `one-dark` | https://github.com/andresmichel/one-dark-theme (`One Dark.tmTheme`) | `9ecab1531c983680897b9a77190262561ce4fe0e` | MIT |
| `catppuccin-mocha.tmTheme` | `catppuccin-mocha` | https://github.com/catppuccin/bat (`themes/Catppuccin Mocha.tmTheme`) | `6810349b28055dce54076712fc05fc68da4b8ec0` | MIT |

Two presets read a port, because the project itself publishes no TextMate
theme: `catppuccin/sublime-text` ships only
`.sublime-color-scheme` files, so Catppuccin's own `bat` theme (which syntect
itself consumes) is bundled instead, and `enkia/tokyo-night-vscode-theme` ships
only VS Code JSON, so the Sublime port in `folke/tokyonight.nvim` is bundled
instead.
