# AI-Guided Generation

`cfgd generate` scans your system and uses an AI model to propose a structured cfgd configuration — modules for each tool, profiles tying them together, and any dotfiles to bring under management. You review each generated file before it is written.

## What `cfgd generate` Does

Instead of writing YAML by hand, `cfgd generate` does the following:

1. Scans your installed packages, dotfiles, shell config (aliases, exports, PATH), and system settings.
2. Sends the scan results to an AI model (Claude by default).
3. The AI proposes a module and profile structure — which tools warrant their own modules, how profiles should inherit, what dependencies exist.
4. For each generated YAML file, you see the full content and choose: accept, reject, give feedback, or step through it section by section.
5. Accepted files are written to `modules/<name>/module.yaml` and `profiles/<name>.yaml` in your config repo.
6. When all files have been reviewed, you're offered the option to commit them.

## Full Flow Walkthrough

```sh
cfgd init                  # scaffold an empty config repo
cfgd generate              # scan, propose structure, generate
cfgd profile switch base   # activate the generated base profile
cfgd apply --dry-run       # preview what would be applied
cfgd apply                 # apply to the machine
```

### System Scan

cfgd scans:

- **Installed packages** — queries all available package managers (brew, apt, cargo, npm, pipx, dnf, etc.) for their installed package lists.
- **Dotfiles** — walks `~` and `~/.config/` for config files, identifies which tool each belongs to.
- **Shell config** — parses your RC files (`.zshrc`, `.bashrc`, `config.fish`) to extract aliases, exports, PATH additions, sourced files, and plugin managers.
- **System settings** — macOS defaults domains, systemd user units, and LaunchAgents (platform-dependent).

### AI Proposes Structure

The AI receives the scan results and proposes which tools should become modules (nvim, tmux, zsh, git, etc.) and how profiles should be organized. Dependencies are inferred — if your nvim config requires Node.js for LSP, the AI will set `depends: [node]`.

### Per-Component Generation

The AI generates modules first (leaf dependencies first, dependents last), then profiles. For each generated document, it calls the `present_yaml` tool, which shows you the YAML with syntax highlighting:

```
Generated Module — neovim: editor configuration with Lazy.nvim and LSP
─────────────────────────────────────────────────────────────
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  depends: [node]
  packages:
    - name: neovim
      min-version: "0.9"
      prefer: [brew, snap, apt]
  files:
    - source: config/
      target: ~/.config/nvim/
  scripts:
    post-apply:
      - nvim --headless "+Lazy! sync" +qa
─────────────────────────────────────────────────────────────
What would you like to do?
> Accept
  Reject
  Give feedback
  Step through
```

- **Accept** — write the file to the repo.
- **Reject** — skip this file; the AI continues to the next.
- **Give feedback** — type a message; the AI revises and presents again.
- **Step through** — the AI breaks the document into smaller pieces and presents each separately.

### File Writing and Optional Commit

After all components have been reviewed, cfgd shows a summary of written files and offers to commit them:

```
Generated files
  module/nvim: modules/nvim/module.yaml
  module/tmux: modules/tmux/module.yaml
  profile/base: profiles/base.yaml

Commit all generated files? [Y/n]
```

With `--yes`, the commit is automatic. The commit message is:
`feat: add AI-generated configuration profiles and modules`

## Scoped Generation

Generate a single module or profile without a full system scan:

```sh
cfgd generate module nvim   # investigate neovim, generate its module
cfgd generate profile work  # generate a work profile interactively
```

### `cfgd generate module <name>`

The AI inspects the named tool: detects its version, finds its config files, reads them, identifies plugin systems and dependencies, then generates a module. Useful when you want to bring one tool under management without touching everything else.

```sh
cfgd generate module tmux
cfgd generate module zsh
cfgd generate module git
```

### `cfgd generate profile <name>`

The AI generates a profile by asking about the machine's purpose, the modules it should include, and any platform-specific settings. The resulting profile can inherit from an existing base profile.

```sh
cfgd generate profile work-mac
cfgd generate profile dev-linux
```

## Configuration

AI settings live under `spec.ai` in `cfgd.yaml`:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  ai:
    provider: claude
    model: claude-sonnet-4-6
    api-key-env: ANTHROPIC_API_KEY
```

All three fields have defaults — you only need this section if you want to override something.

| Field | Default | Description |
|---|---|---|
| `provider` | `claude` | AI provider name |
| `model` | `claude-sonnet-4-6` | Model ID |
| `api-key-env` | `ANTHROPIC_API_KEY` | Environment variable holding the API key |

### Provider and Model Overrides

Override via CLI flags without touching `cfgd.yaml`:

```sh
cfgd generate --model claude-opus-4-20250514
cfgd generate --provider claude --model claude-haiku-4-5
```

### API Key Setup

Set the API key as an environment variable. Never put it in `cfgd.yaml` — the config file is typically committed to git.

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cfgd generate
```

To use a different variable name:

```yaml
spec:
  ai:
    api-key-env: MY_ANTHROPIC_KEY
```

```sh
export MY_ANTHROPIC_KEY=sk-ant-...
cfgd generate
```

For persistent configuration, add the export to your shell RC file (outside of cfgd management — you don't want cfgd managing the file that sets up cfgd's own key).

## CLI Flags

| Flag | Description |
|---|---|
| `--model <model-id>` | Override AI model |
| `--provider <name>` | Override AI provider |
| `--yes`, `-y` | Skip confirmation prompts; auto-accept all generated YAML |
| `--scan-only` | Scan and print findings without starting the AI conversation |
| `--shell <name>` | Override shell for config scanning (default: auto-detect from `$SHELL`) |
| `--home <path>` | Override home directory for scanning |

## MCP Server Setup

`cfgd mcp-server` exposes the same generation tools over the [Model Context Protocol](https://modelcontextprotocol.io/) (MCP). This lets any MCP-compatible AI client (Claude Code, Cursor, etc.) call cfgd's scan and write tools directly — without the embedded CLI client.

### What MCP Is

MCP is a protocol for connecting AI models to external tools via a JSON-RPC stdin/stdout transport. When a client connects to `cfgd mcp-server`, it can call tools, read resources, and use prompts — all defined by cfgd and executed locally on your machine.

### Running the MCP Server

```sh
cfgd mcp-server
```

The server reads JSON-RPC messages from stdin and writes responses to stdout. It runs until stdin is closed. Most users don't run this directly — they configure their AI client to launch it automatically.

### Claude Code Setup

Add to your Claude Code settings (`~/.config/claude/settings.json` or the project-level `.claude/settings.json`):

```json
{
  "mcpServers": {
    "cfgd": {
      "command": "cfgd",
      "args": ["mcp-server"]
    }
  }
}
```

After restarting Claude Code, the cfgd tools are available in any conversation. You can ask Claude to scan your system and generate a cfgd config, and it will call the tools directly.

### Cursor Setup

Add to `.cursor/mcp.json` in your home directory or project root:

```json
{
  "mcpServers": {
    "cfgd": {
      "command": "cfgd",
      "args": ["mcp-server"]
    }
  }
}
```

Restart Cursor after saving. The cfgd tools appear in Cursor's tool list.

### Generic MCP Client

Any MCP-compatible client can connect by launching `cfgd mcp-server` as a subprocess and communicating over stdin/stdout. The server speaks JSON-RPC 2.0 and implements the MCP 2024-11-05 specification.

### Available Tools

| Tool | Description |
|---|---|
| `cfgd_scan_installed_packages` | List installed packages across all available package managers |
| `cfgd_scan_dotfiles` | Scan the home directory for dotfiles and XDG config entries |
| `cfgd_scan_shell_config` | Parse shell RC files: aliases, exports, PATH, plugin managers |
| `cfgd_scan_system_settings` | macOS defaults, systemd units, LaunchAgents |
| `cfgd_detect_platform` | OS, distro, version, and CPU architecture |
| `cfgd_inspect_tool` | Version, config file locations, plugin system for a named tool |
| `cfgd_query_package_manager` | Check package availability and version in a specific manager |
| `cfgd_read_file` | Read a file's contents (64 KB limit, blocked patterns enforced) |
| `cfgd_list_directory` | List directory entries (boundary enforced) |
| `cfgd_adopt_files` | Copy config files into the config repo |
| `cfgd_get_schema` | YAML schema for Module, Profile, or Config |
| `cfgd_validate_yaml` | Validate YAML against cfgd schema |
| `cfgd_write_module_yaml` | Write a Module YAML file to the config repo |
| `cfgd_write_profile_yaml` | Write a Profile YAML file to the config repo |
| `cfgd_present_yaml` | Present generated YAML to the user for review |
| `cfgd_list_generated` | List modules and profiles generated in this session |
| `cfgd_get_existing_modules` | List module names already in the config repo |
| `cfgd_get_existing_profiles` | List profile names already in the config repo |

### Available Resources

Resources are read-only documents the AI can fetch from the server:

| URI | Description |
|---|---|
| `cfgd://skill/generate` | Orchestration skill: the system prompt for AI-guided generation |
| `cfgd://schema/module` | Annotated YAML schema for Module resources |
| `cfgd://schema/profile` | Annotated YAML schema for Profile resources |
| `cfgd://schema/config` | Annotated YAML schema for Config resources |

### Available Prompts

Prompts are ready-made conversation starters the client can inject:

| Prompt | Arguments | Description |
|---|---|---|
| `cfgd_generate` | `mode` (full/module/profile), `name` | AI-guided full generation or scoped to one component |
| `cfgd_generate_module` | `name` (required) | Generate a module for a specific tool |
| `cfgd_generate_profile` | `name` (required) | Generate a profile |

## Security Model

### What Is Sent to the AI Provider

`cfgd generate` sends the following to the AI provider's API:

- Names and versions of installed packages
- Dotfile paths (not contents, until the AI calls `read_file` for a specific file)
- Shell aliases, exports, and PATH additions parsed from RC files
- System settings metadata (macOS defaults values, systemd unit names)
- Config file contents when the AI calls `read_file` — subject to the constraints below

Nothing else leaves your machine. The API call is made directly from cfgd to the provider's HTTPS endpoint — no cfgd servers are involved.

### File Access Constraints

The `read_file` and `list_directory` tools enforce a strict boundary:

- **Home and repo boundary** — files must be within `$HOME` or the cfgd config repo. Paths resolving outside these roots (including symlinks pointing elsewhere) are rejected.
- **64 KB limit** — files larger than 64 KB are truncated. The AI sees the first 64 KB and a truncation notice.
- **Credential blocklist** — paths matching any of the following patterns are blocked unconditionally, regardless of location:
  - `.ssh/id_` — SSH private keys
  - `.gnupg/private-keys` — GnuPG private keys
  - `.pem`, `.key` — PEM and raw key files
  - `credentials`, `secret`, `token` — common credential file names

### Consent Disclosure

On first run (without `--yes`), cfgd prints a disclosure before making any API call:

```
Warning: This command sends file contents and system information to claude's API to generate your configuration.
Info: Only files in your home directory are accessible, and private keys/credentials are excluded.
Continue? [Y/n]
```

Use `--yes` to skip the prompt in automated or trusted environments.

## Troubleshooting

### API Key Not Found

```
Error: API key not found in environment variable ANTHROPIC_API_KEY
```

Set the key in your environment before running:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
cfgd generate
```

If you're using a different variable name, set `spec.ai.api-key-env` in `cfgd.yaml`.

### Model Not Available

```
Error: model not found or you don't have access
```

Verify that your API key has access to the requested model. Try a different model:

```sh
cfgd generate --model claude-haiku-4-5
```

### Rate Limiting

If you hit rate limits during a long generation session, the API will return a `429` error. cfgd does not retry automatically. Wait a moment and re-run. For large configs, consider generating one module at a time:

```sh
cfgd generate module nvim
cfgd generate module tmux
cfgd generate module zsh
```

### Scan-Only Mode

Use `--scan-only` to inspect what cfgd finds on your system without starting an AI conversation — useful for debugging scan output or verifying what would be sent to the API:

```sh
cfgd generate --scan-only
```

Output example:

```
Scanning dotfiles
  Found 23 dotfile entries
  Detected tools: git, nvim, starship, tmux, zsh

Scanning zsh config
  Found 14 aliases
  Found 8 exports
  Found 3 PATH additions
  Plugin manager: oh-my-zsh

Scan complete — use without --scan-only to generate config
```
