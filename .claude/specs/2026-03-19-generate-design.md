# `cfgd generate` — AI-Guided Configuration Generation

## Problem

A user switching machines (e.g., changing jobs) wants to capture their existing setup into well-organized cfgd profiles and modules without manually authoring YAML. The cognitive load of inventorying installed packages, dotfiles, shell config, system settings, and organizing them into layered profiles with proper module decomposition is exactly what an AI assistant excels at.

## Solution

An AI-guided interactive command (`cfgd generate`) that introspects the user's system, proposes an organizational structure (profiles and modules), then walks through each component with deep per-tool investigation — presenting complete YAML for confirmation before writing.

Two interfaces share a single tool layer:
1. **`cfgd generate`** — standalone CLI experience with an embedded AI client (Anthropic Messages API)
2. **MCP server** — exposes the same tools for use by any MCP-compatible AI client (Claude Code, Cursor, etc.)

## Command Interface

```
cfgd generate                      # full flow: scan → propose structure → generate all
cfgd generate module <name>        # investigate one tool, generate its module
cfgd generate profile <name>       # generate one profile from system scan

Flags:
  --model <model-id>               # override AI model (default: claude-sonnet-4-6)
  --provider <name>                # override AI provider (default: claude)
```

MCP server (separate command, registered in client configs):
```
cfgd mcp-server                    # start MCP server on stdin/stdout
```

## Configuration

```yaml
# in cfgd.yaml
spec:
  ai:
    provider: claude              # only supported value initially
    model: claude-sonnet-4-6      # default model
    apiKeyEnv: ANTHROPIC_API_KEY # env var name (never the key itself)
```

API keys are never stored in config files. The `apiKeyEnv` field names the environment variable to read. Defaults to `ANTHROPIC_API_KEY`. If the key is missing at runtime, the user gets a clear message explaining how to set it.

The `ai` field on `ConfigSpec` is `Option<AiConfig>` with `#[serde(default)]`, matching existing optional fields. Existing `cfgd.yaml` files without an `ai:` section continue to work — defaults are applied at runtime (provider: claude, model: claude-sonnet-4-6, apiKeyEnv: ANTHROPIC_API_KEY).

CLI flags `--model` and `--provider` override config values.

## Full Flow (`cfgd generate`)

### Phase 1: System Scan & Structure Proposal

1. Run all introspection tools: `detect_platform`, `scan_installed_packages` (all available managers), `scan_dotfiles`, `scan_shell_config`, `scan_system_settings`
2. AI analyzes results and proposes a layering strategy:
   - Which profiles to create, with inheritance relationships (e.g., `base` → `dev`, `base` → `personal`)
   - Which tools warrant their own modules (vs. being inline in a profile)
   - Dependency graph between modules
3. Present the proposed structure to the user for confirmation/adjustment before proceeding

### Phase 2: Module Generation (dependency order)

For each module, the AI systematically evaluates every schema field with deep investigation:

**`packages`** — For each package in the module:
- `inspect_tool` to get installed version
- Web search (AI's own capability in MCP mode; search API in CLI mode) for companion/recommended packages (e.g., neovim → pynvim, tree-sitter CLI, neovim npm/ruby packages)
- `query_package_manager` across all available managers to determine:
  - `prefer` ordering (which managers have the required version)
  - `min-version` constraints (based on installed version and feature requirements)
  - Manager exclusions (e.g., apt's neovim is too old for version 0.11)
  - Name aliases across managers (`fd` vs `fd-find`, `ripgrep` vs `rg`)

**`files`** — For each tool's configuration:
- `inspect_tool` + `list_directory` to find all config locations (XDG, legacy, platform-specific)
- `read_file` on key config files to assess complexity
- Identify files containing machine-specific values (paths, hostnames) that need templating (`.tera`)
- Determine source strategy: plain copy vs. symlink vs. template

**`env`** — Cross-reference `scan_shell_config` results for exports related to this tool. Web search for recommended environment variables.

**`aliases`** — Pull from shell config scan, web search for common community aliases.

**`scripts.post-apply`** — Web search for recommended post-install steps (plugin sync, cache rebuild, etc.). Verify commands work with `inspect_tool`.

**`depends`** — Evaluate whether any package's configuration is complex enough to warrant its own module. Heuristic: if it has its own config directory AND multiple related packages AND post-apply steps, suggest a separate module. Always ask the user.

**File adoption** — When a module's YAML references source files (e.g., `source: config/init.lua`), the AI calls `adopt_files` to copy the actual config files from their current locations into the module's directory (`modules/<name>/config/`). This happens after the user accepts the module YAML. The AI confirms which files will be adopted and their destinations before calling the tool.

### Phase 3: Profile Assembly

For each profile:
- Assign confirmed modules to `modules` list
- Aggregate packages that don't belong to a specific module (general CLI tools, fonts, etc.)
- Set `inherits` based on the agreed layering
- Pull env/aliases from shell config that aren't module-specific
- Platform-specific `system` settings from the scan
- File permissions from the scan

### Phase 4: Finalization

- Present summary of all generated files
- Offer to commit all with a descriptive message
- Suggest `cfgd apply --dry-run` as next step

## Scoped Flows

**`cfgd generate module <name>`** — Skips Phase 1 (structure proposal) and Phase 3 (profile assembly). Investigates the named tool and generates a single module YAML. May suggest dependency modules during investigation but doesn't require them.

**`cfgd generate profile <name>`** — Skips Phase 1. Runs system scan, generates one profile. May suggest extracting modules but doesn't require it.

## Per-Component Interaction Protocol

After investigating a component, the AI presents the full YAML with syntax highlighting and offers four options:

- **[a]ccept** — Write this file and move on
- **[r]eject** — Skip this component entirely
- **[f]eedback** — User provides verbal feedback; AI revises and re-presents
- **[s]tep-through** — Switch to section-by-section mode for this component (packages, then files, then env, then aliases, then scripts — each with the same accept/feedback options; assembled into final YAML from confirmed sections)

On feedback: AI revises the YAML, re-presents. Loops until accepted or rejected.

Generated files are written to disk immediately on accept. At the end, a summary is presented and the user can commit all or discard via git.

## Tool Layer

Tools are split across two crates to respect the `std::process::Command` boundary (CLAUDE.md Hard Rule #6):

- **cfgd-core**: Pure data types, schema export, YAML validation, session state, file writing. No shelling out.
- **cfgd binary crate** (`cfgd/src/generate/`): Tool implementations that shell out — system introspection, tool inspection, package manager queries. These use the existing `PackageManager` trait and `ProviderRegistry`, not parallel implementations.

Both the embedded CLI client and MCP server call the same tool implementations in the binary crate. The split is an internal boundary, not visible to the AI.

### System Introspection (cfgd binary)

| Tool | Description |
|------|-------------|
| `scan_installed_packages(manager?)` | Delegates to `PackageManager::installed_packages_with_versions()` (new trait method, see below) via `ProviderRegistry`. Returns name, version, manager. Shells out per provider. |
| `scan_dotfiles(home)` | Find dotfiles/config dirs in `~` and `~/.config/`. Pure filesystem scan, no shelling out. Returns path, size, type, associated tool guess. |
| `scan_shell_config(shell)` | Parse shell rc files for aliases, exports, PATH additions, sourced files, plugin managers. Pure file parsing, no shelling out. |
| `scan_system_settings()` | Platform-specific: macOS defaults domains, systemd user units, LaunchAgents, gsettings schemas, Windows registry values, Windows services. Shells out via existing `SystemConfigurator` trait where possible. |
| `detect_platform()` | OS, distro, arch, available package managers. Already exists in cfgd-core `platform/`. |

Note: `scan_dotfiles` and `scan_shell_config` are pure filesystem/parsing operations that don't shell out. They live in `generate/` for organizational cohesion with the other introspection tools, not because of `Command` usage.

### Investigation (cfgd binary — shells out)

| Tool | Description |
|------|-------------|
| `inspect_tool(name)` | Run `<tool> --version`, find config paths (XDG + legacy + platform-specific), detect plugin systems. |
| `query_package_manager(manager, package)` | Delegates to `PackageManager::available_version()` and new `PackageManager::package_aliases()` method via `ProviderRegistry`. Returns availability, version, aliases. |

### PackageManager Trait Extensions

Two new methods added to the existing `PackageManager` trait to support generate tools without creating parallel implementations:

```rust
/// Return installed packages with their versions.
/// Default implementation calls installed_packages() with version "unknown".
fn installed_packages_with_versions(&self) -> Result<Vec<PackageInfo>> {
    Ok(self.installed_packages()?
        .into_iter()
        .map(|name| PackageInfo { name, version: "unknown".into() })
        .collect())
}

/// Return known name aliases for a package across this manager.
/// e.g., "fd" on brew is "fd-find" on apt. Default returns empty vec.
fn package_aliases(&self, _canonical_name: &str) -> Result<Vec<String>> {
    Ok(vec![])
}
```

Default implementations ensure existing `PackageManager` impls (and `MockPackageManager` in tests) don't break. Each provider overrides with real implementations as part of Layer 2.

Each `PackageManager` implementation (brew, apt, cargo, npm, pipx, dnf) implements these using their existing shelling-out patterns. No new `Command` boundaries needed — these methods live in `cfgd/src/packages/` where `Command` is already allowed.

### File Access (cfgd binary — no shelling out)

| Tool | Description |
|------|-------------|
| `read_file(path)` | Read file contents with security constraints (see Security Model below). |
| `list_directory(path)` | List directory contents with security constraints. |
| `adopt_files(source_paths, module_name)` | Copy config files into `modules/<name>/config/` or `files/` for profile use. Uses `atomic_write`. |

### Schema & Validation (cfgd-core — no shelling out)

| Tool | Description |
|------|-------------|
| `get_schema(kind)` | Return annotated YAML schema for "Module", "Profile", or "Config". Uses hand-maintained annotated example YAML embedded as const strings (not runtime reflection). Updated when config structs change — enforced by a snapshot test that fails if the schema string diverges from the struct's fields. |
| `validate_yaml(content, kind)` | Deserialize YAML into the corresponding config struct (`ModuleSpec`, `ProfileSpec`, `ConfigSpec`). Returns structured errors on failure so the AI can self-correct. |

### Generation (cfgd-core — no shelling out)

| Tool | Description |
|------|-------------|
| `write_module_yaml(name, content)` | Validate via `validate_yaml`, then write to `modules/<name>/module.yaml` via `atomic_write`. Returns validation errors on failure (not silent rejection) so the AI can fix and retry. Repo root path is held by `GenerateSession` (initialized at command start from config resolution). |
| `write_profile_yaml(name, content)` | Same pattern for `profiles/<name>.yaml`. Same repo root resolution via `GenerateSession`. |
| `present_yaml(content, kind, description)` | Dedicated tool the AI calls to present YAML for user review. cfgd-core defines the request/response types only. In CLI mode, the conversation loop in `cli/generate.rs` handles this tool specially: renders with `Printer::syntax_highlight` and triggers the accept/reject/feedback/step-through prompt via `inquire` (keeping `inquire` at the CLI boundary). In MCP mode, returns the YAML as a formatted tool result for the client to display and collect feedback. This is how the conversation loop detects "YAML for confirmation" — it's an explicit tool call, not text parsing. |

### Session State (cfgd-core — no shelling out)

| Tool | Description |
|------|-------------|
| `list_generated()` | In-memory list of what's been written this session. If the process crashes, `get_existing_modules`/`get_existing_profiles` discovers what's on disk — partial generation is resumable by re-running `cfgd generate`. |
| `get_existing_modules()` | Modules already in the repo (avoid duplicates, enable dependency references). |
| `get_existing_profiles()` | Profiles already in the repo. |

### Security Model for File Access

`read_file` and `list_directory` are security-sensitive — file contents are sent to the AI provider's API as tool results. Constraints:

- **Size limit**: 64 KB per `read_file` call. Larger files return a truncated preview with a size warning.
- **Path restrictions**: Paths must be within the user's home directory (`~/`) or the cfgd repo directory. Requests outside these boundaries are rejected with an error. Specifically blocked: `~/.ssh/id_*` (private keys), `~/.gnupg/private-keys*`, any path matching `*.pem`, `*.key`, `*credentials*`, `*secret*`, `*token*` (configurable blocklist).
- **Symlink resolution**: Paths are canonicalized before boundary checks to prevent symlink escapes.
- **Disclosure**: On first invocation of `cfgd generate`, the user is informed: "This command sends file contents and system information to [provider]'s API to generate your configuration. Only files in your home directory are accessible, and private keys/credentials are excluded. Continue?" Requires explicit consent via `inquire::Confirm` (or `--yes` flag).
- **MCP mode**: The MCP client provides its own consent model. The same path restrictions and blocklist still apply server-side.

## Orchestration Skill

A markdown document stored in cfgd's source tree and embedded as a const string in the binary. Serves as the system prompt for the embedded CLI client and as an MCP resource (`cfgd://skill/generate`) for MCP clients.

The skill contains:

1. **Role and objective** — "You are cfgd's configuration generator. Investigate the user's system and produce well-organized, production-quality cfgd profiles and modules."

2. **Mode handling** — How to adapt behavior for full/module/profile scoped invocations.

3. **Schema field coverage** — For every field in the Module and Profile schemas, the investigation depth expected. Package-agnostic guidance (not "neovim needs pynvim" but "for every package, search for companion packages recommended by the tool's community"). Covers: packages (version constraints, manager exclusions, aliases, companions), files (config discovery, templating decisions), env, aliases, scripts, depends (decomposition heuristics), system settings, secrets, inheritance.

4. **Interaction protocol** — The accept/reject/feedback/step-through flow. How to present YAML. When to ask for confirmation.

5. **Quality standards** — Every `prefer` list must be justified by version availability. Every `min-version` must be based on actual installed version or feature requirement. Every file entry must specify source strategy. No stub modules.

The skill does NOT contain package-specific knowledge. The AI brings world knowledge; the skill provides structure and rigor.

## Embedded CLI Client (`cfgd generate`)

### Module Structure

The command handler lives in `cli/` alongside other commands. The AI client machinery lives in a new `ai/` module:

- `cli/generate.rs` — Clap subcommand definition and command handler. Orchestrates: load config → resolve API key → build system prompt → enter conversation loop → handle user interaction → finalize.
- `ai/client.rs` — Anthropic Messages API client using `ureq`. Handles streaming responses, tool-use request/response cycles, API key resolution from env var.
- `ai/tools.rs` — Maps tool-use calls from the API to tool functions (both cfgd-core and `generate/` module). Serializes results as tool responses. Unrecognized tool names return an error result so the AI can self-correct.
- `ai/conversation.rs` — Multi-turn conversation management: system prompt assembly, message history, token usage tracking (input/output token counts per turn, cumulative total displayed at end).

The `AiConfig` struct lives in `cfgd-core/src/config/mod.rs` as a field on `ConfigSpec`, per Hard Rule #5. The `ai/` module in the binary crate reads the parsed config, it does not parse config itself.

### Conversation Loop

```
1. Display consent disclosure (what data is sent where). Skip if --yes.
2. Build system prompt (skill text + mode context)
3. If full mode: send initial message with scan results summary
   If scoped mode: send initial message with target component name
4. Loop:
   a. Receive assistant response (streamed to terminal via Printer)
   b. If tool_use blocks in response:
      - For recognized tools: execute and send tool results back
      - For unrecognized tools: send error result ("unknown tool: X")
      - For `present_yaml` tool: render YAML with syntax highlighting,
        present accept/reject/feedback/step-through via inquire,
        send user choice as tool result
      - Continue loop (back to 4a)
   c. If assistant signals completion (no tool calls, final text):
      - Present summary of generated files via Printer
      - Display token usage (input/output/total)
      - Offer commit via inquire::Confirm
      - Exit
```

The `present_yaml` tool is the mechanism for structured user interaction — the AI explicitly invokes it when it wants confirmation, rather than the client parsing text looking for YAML blocks. This makes the confirmation flow reliable and unambiguous.

### User Interaction Rendering

- AI text responses stream to terminal. For v1, streamed text is rendered as-is (plain text with ANSI). Markdown rendering (via `termimad` or similar) is a follow-up enhancement — the orchestration skill instructs the AI to keep prose brief and let the YAML speak for itself, so raw text is acceptable initially.
- YAML blocks in `present_yaml` tool calls get syntax highlighting via `Printer::syntax_highlight`
- Confirmation prompts use `inquire::Select` (accept/reject/feedback/step-through) and `inquire::Text` (freeform feedback)
- `inquire` is already a dependency of cfgd-core; add it to the binary crate's Cargo.toml as well

### Web Search in CLI Mode

In MCP mode, the AI client provides its own web search capability. In embedded CLI mode, web search is delegated to the Anthropic API's built-in web search tool (if the model supports it). cfgd does not implement its own search provider.

If the model doesn't support web search, the AI proceeds with its training knowledge only — degraded but functional. The orchestration skill notes this: "If web search is unavailable, state your confidence level and note what you'd want to verify."

No search-provider configuration in `spec.ai`. This avoids the complexity of managing third-party search API keys.

## MCP Server

### Command

```
cfgd mcp-server    # starts on stdin/stdout
```

### Exposed Tools

All tools from the Tool Layer, prefixed with `cfgd_`:
- `cfgd_scan_installed_packages`, `cfgd_scan_dotfiles`, `cfgd_scan_shell_config`, `cfgd_scan_system_settings`, `cfgd_detect_platform`
- `cfgd_inspect_tool`, `cfgd_query_package_manager`, `cfgd_read_file`, `cfgd_list_directory`
- `cfgd_write_module_yaml`, `cfgd_write_profile_yaml`, `cfgd_validate_yaml`
- `cfgd_adopt_files`
- `cfgd_present_yaml` — returns formatted YAML as a tool result; the MCP client is responsible for rendering and collecting user feedback, then sending the user's choice as the next message
- `cfgd_list_generated`, `cfgd_get_existing_modules`, `cfgd_get_existing_profiles`
- `cfgd_get_schema`

No `cfgd_web_search` — the client provides its own search capability.

### Exposed Resources

- `cfgd://skill/generate` — The orchestration skill text
- `cfgd://schema/module` — Module YAML schema reference
- `cfgd://schema/profile` — Profile YAML schema reference
- `cfgd://schema/config` — Config YAML schema reference

### Exposed Prompts

- `cfgd_generate(mode?, name?)` — Combines skill + mode instructions as an initial prompt
- `cfgd_generate_module(name)` — Convenience: `cfgd_generate(mode="module", name=<arg>)`
- `cfgd_generate_profile(name)` — Convenience: `cfgd_generate(mode="profile", name=<arg>)`

### Implementation

New module in `cfgd/src/mcp/`:
- `server.rs` — JSON-RPC stdin/stdout handler, request dispatch
- `tools.rs` — MCP tool definitions (JSON schemas), dispatch to tool functions in `generate/` and cfgd-core
- `resources.rs` — Skill and schema resource serving
- `prompts.rs` — MCP prompt definitions

Implemented directly against the MCP spec using `serde_json`. No heavy framework dependency.

The MCP server is stateful within a single stdin/stdout session — `list_generated` tracks writes made during that session's lifetime. Session state is in-memory (no persistence). If the server restarts, `get_existing_modules`/`get_existing_profiles` re-discovers what's on disk.

## Build Order

Each layer is independently shippable. Commit between each layer. Update relevant docs with each layer.

### Layer 1: Core Types & Validation (cfgd-core)

- `AiConfig` struct in `config/mod.rs` (provider, model, apiKeyEnv fields)
- `get_schema(kind)` — hand-maintained annotated YAML examples, embedded as const strings. Snapshot test that fails if schema strings diverge from struct fields.
- `validate_yaml(content, kind)` — deserialize into config structs, return structured errors
- `present_yaml` tool types (request/response structs for the confirmation flow)
- Session state types and tracking (`GenerateSession` struct)
- `write_module_yaml` / `write_profile_yaml` — validate-then-atomic-write, return errors on failure
- `PackageManager` trait extensions: `installed_packages_with_versions()`, `package_aliases()`
- Unit tests for all of the above
- Update `docs/configuration.md` with `spec.ai` section

### Layer 2: Tool Implementations (cfgd binary)

- New `generate/` module in binary crate with tool implementations:
  - `scan_installed_packages` — delegates to `ProviderRegistry` + new trait methods
  - `scan_dotfiles`, `scan_shell_config` — pure filesystem/parsing
  - `scan_system_settings` — delegates to `SystemConfigurator` impls where possible
  - `inspect_tool` — shells out for `--version`, filesystem scan for config paths
  - `query_package_manager` — delegates to `ProviderRegistry`
  - `read_file`, `list_directory` — with security model (path restrictions, size limits, blocklist)
  - `adopt_files` — copy config files into module/profile directories
- `PackageManager` trait method implementations in each provider (brew, apt, cargo, npm, pipx, dnf)
- Unit tests for every tool function (tempfile for fs, mock providers for package queries)
- Update CLAUDE.md to add `generate/` to the `std::process::Command` allowlist

### Layer 3: Orchestration Skill + Embedded CLI Client

- Author the skill markdown document, embed as const string
- `ai/` module: API client (`client.rs`), tool dispatch (`tools.rs`), conversation management (`conversation.rs`)
- `cli/generate.rs`: Clap subcommand, `--model`/`--provider` flags, consent disclosure, conversation loop
- Unit tests for tool dispatch mapping, conversation state, config parsing
- Integration tests: mock HTTP responses simulating Messages API tool-use flow, verify correct tool dispatch and YAML validation
- Update `docs/cli-reference.md`, `docs/bootstrap.md`

### Layer 4: MCP Server

- `mcp/` module: JSON-RPC transport, tool/resource/prompt definitions, stateful session
- `cli/mcp_server.rs`: subcommand
- Unit tests for JSON-RPC parsing, tool dispatch, resource serving, session state
- Integration tests: raw JSON-RPC request/response cycles
- New `docs/ai-generate.md` covering both CLI and MCP usage, client setup instructions

## Testing Strategy

- **Core types & validation**: Unit tests for `validate_yaml` (valid and invalid inputs for each kind), snapshot tests for `get_schema` output (fail if schema drifts from struct fields), unit tests for `AiConfig` deserialization
- **Tool functions**: `tempfile` for filesystem operations (`scan_dotfiles`, `scan_shell_config`, `read_file`, `list_directory`, `adopt_files`), mock `PackageManager` trait impls for package queries (`scan_installed_packages`, `query_package_manager`), security model tests (path rejection, size limits, blocklist matching)
- **Skill**: Validated by `validate_yaml` tests — generate sample YAML following skill guidance, confirm it passes validation
- **CLI client**: Unit tests for conversation state management, tool dispatch mapping (including unknown tool error handling), `AiConfig` resolution (env var, config file, CLI flags). Integration tests with mock HTTP responses simulating the Messages API tool-use flow — verify correct tool dispatch, YAML validation gating, and `present_yaml` interaction.
- **MCP server**: Unit tests for JSON-RPC message parsing, tool dispatch, resource serving, session state tracking. Integration tests sending raw JSON-RPC messages and verifying responses match MCP spec.

## Documentation Updates

- `docs/cli-reference.md` — Add `generate` and `mcp-server` commands
- `docs/configuration.md` — Add `spec.ai` config section
- `docs/bootstrap.md` — Add AI-guided generation as a bootstrap path
- `docs/ai-generate.md` — New doc covering: full flow walkthrough, scoped generation, MCP server setup for various clients, model/provider configuration, troubleshooting
