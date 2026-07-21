# Authoring Skills (`cfgd skill`)

`cfgd skill` installs a provider-native **agent skill** — one per author-facing
resource kind — into your coding agent. The skill teaches *your* agent (Claude
Code, Gemini CLI, GitHub Copilot, Codex, Cursor) to author high-quality cfgd
resources: it enumerates every field via `cfgd explain`, researches
best-practices for the subject, justifies each choice, and validates the result
against the live schema before handing it back.

Unlike `cfgd generate` — which runs an AI loop *inside cfgd* against your API key
— a skill costs no marginal API tokens, sends nothing home, and is reusable
inside every interactive coding session. See
[when to use `generate` vs the skill](#when-to-use-generate-vs-the-skill).

## Quick start

```console
$ cfgd skill install module
Installing skill module (project scope)
  claude-code: .claude/skills/cfgd-module/SKILL.md
  codex: AGENTS.md
```

Then, inside Claude Code:

```
/cfgd-module          # the skill is now invocable; the agent authors a Module
```

## Command surface

```bash
cfgd skill install <kind> [-g|--global] [--provider <id>] [--force] [--yes]
cfgd skill list           [-g|--global]                                   # alias: ls
cfgd skill remove  <kind> [-g|--global] [--provider <id>]                 # alias: rm
cfgd skill update  (<kind> | --all) [-g|--global] [--provider <id>]
```

`<kind>` is one of the six author-facing kinds:

| Kind | What the skill authors |
|---|---|
| `module` | a local Module (`modules/<name>/module.yaml`) |
| `profile` | a local Profile (`profiles/<name>/profile.yaml`; legacy flat `profiles/<name>.yaml` still supported) |
| `source` | a ConfigSource (`cfgd-source.yaml`) |
| `machineconfig` | a MachineConfig CRD |
| `configpolicy` | a ConfigPolicy CRD |
| `clusterconfigpolicy` | a ClusterConfigPolicy CRD |

| Flag | Meaning |
|---|---|
| `-g` / `--global` | install under the user's home dirs instead of the project (see [scope](#scope-project-vs-user)) |
| `--provider <id>` | restrict to named providers (repeatable: `--provider claude-code --provider gemini`); default is **every detected** provider |
| `--force` | write even for an undetected agent, and overwrite an existing skill |
| `--yes` / `-y` | skip the overwrite confirmation (also `CFGD_YES=1`) |
| `--all` | (on `update`) re-render every skill currently installed at the scope |

`--provider` ids are `claude-code`, `gemini`, `copilot`, `codex`, `cursor`. An
unknown id is a hard error that lists the valid ones.

### `install`

With no `--provider`, install targets every agent it auto-detects at the scope
(presence of `.claude/`, `.gemini/`, `.github/`, `.cursor/`, `AGENTS.md`, or the
agent's CLI on `PATH`). Undetected agents are reported and skipped, never
silently dropped:

```console
$ cfgd skill install profile
Installing skill profile (project scope)
  claude-code: .claude/skills/cfgd-profile/SKILL.md
  gemini: .gemini/commands/cfgd-profile.toml
  copilot: not detected
  codex: not detected
  cursor: not detected
```

Name a provider explicitly to install it even when undetected (the directory is
created):

```console
$ cfgd skill install module --provider claude-code --provider cursor
Installing skill module (project scope)
  claude-code: .claude/skills/cfgd-module/SKILL.md
  cursor: .cursor/rules/cfgd-module.mdc
```

Multi-provider install is **continue-on-error**: every provider is attempted,
each outcome (`installed` / `skipped` / `failed` + reason) is reported, and the
command exits non-zero if any *targeted* provider failed. Successful providers
are left in place — each file is written atomically and is independently valid.

### `list`

```console
$ cfgd skill list
Installed skills (project scope)
  claude-code/module: .claude/skills/cfgd-module/SKILL.md (0.6.1)
  codex/module: AGENTS.md (0.6.1)
```

A skill rendered by an older cfgd is flagged stale (it carries a version stamp):

```console
$ cfgd skill list -g
Installed skills (user scope)
  claude-code/module: ~/.claude/skills/cfgd-module/SKILL.md (0.3.5)  stale — run `cfgd skill update`
```

### `remove`

```console
$ cfgd skill remove module
Remove the module skill from 2 provider(s)? [y/N] y
Removing skill module (project scope)
  claude-code: .claude/skills/cfgd-module/SKILL.md
  codex: AGENTS.md
```

For whole-file providers `remove` deletes the file (and an emptied
`cfgd-<kind>` directory); for `codex` it excises only the
`cfgd:skill:<kind>` managed block from `AGENTS.md`, leaving every surrounding
byte untouched.

### `update`

`update <kind>` re-renders one installed kind to the current cfgd's output;
`update --all` re-renders every installed kind at the scope.

```console
$ cfgd skill update --all
Updating skills (project scope)
  claude-code/module: .claude/skills/cfgd-module/SKILL.md
  codex/module: AGENTS.md
```

`update` only touches skills that are already installed; it never installs a new
kind or reaches into the other scope.

### Structured output

Every subcommand emits a structured `Doc` under `-o json` (and `-o yaml`), so
CI and scripts can parse the per-provider outcome:

```console
$ cfgd skill install module -o json
{
  "kind": "module",
  "scope": "project",
  "cfgdVersion": "0.6.1",
  "results": [
    { "provider": "claude-code", "path": ".claude/skills/cfgd-module/SKILL.md", "status": "installed" },
    { "provider": "copilot", "status": "skipped", "reason": "not detected" }
  ]
}
```

`results[].status` is one of `installed` / `removed` / `updated` / `skipped` /
`failed`.

## Provider target matrix

Each agent has a different native primitive — there is no universal SKILL
format — so one logical skill renders to each. cfgd writes exactly these paths
and nothing else:

| Provider (`--provider`) | Project-scope path | User-scope path (`-g`) | Native format | Caveat |
|---|---|---|---|---|
| `claude-code` | `.claude/skills/cfgd-<kind>/SKILL.md` | `~/.claude/skills/cfgd-<kind>/SKILL.md` | frontmatter + markdown | invocable as `/cfgd-<kind>` |
| `gemini` | `.gemini/commands/cfgd-<kind>.toml` | `~/.gemini/commands/cfgd-<kind>.toml` | TOML `description` + `prompt` | flat file → slash command `/cfgd-<kind>` (nesting would `:`-namespace it) |
| `copilot` | `.github/prompts/cfgd-<kind>.prompt.md` | *(none)* | prompt frontmatter + body | **IDE-only** (VS Code / JetBrains). Copilot **CLI** cannot invoke prompt files yet; `-g` is unsupported — there is no user-scope primitive |
| `codex` | `AGENTS.md` (managed `cfgd:skill:<kind>` block) | `~/.codex/AGENTS.md` | appended managed section | always-on context, not per-skill invocable; cfgd edits only its delimited block |
| `cursor` | `.cursor/rules/cfgd-<kind>.mdc` | *(none)* | `.mdc` rule | **project-only**; `-g` is unsupported — there is no user-scope primitive |

`copilot` and `cursor` have no user-scope target. With `-g`, cfgd reports them as
a skipped warning rather than fabricating a path:

```console
$ cfgd skill install module -g --provider cursor
Installing skill module (user scope)
  cursor: cursor rules are project-only (.cursor/rules); no user-scope primitive
```

## Scope: project vs user

Scope mirrors cfgd's GitOps ethos exactly.

| Scope | Default? | Flag | Writes to | Why |
|---|---|---|---|---|
| **project** | yes | *(none)* | repo-relative provider dirs (`.claude/`, `.gemini/`, `.github/`, `.cursor/`, `AGENTS.md`) | committable — your team inherits the skill on clone |
| **user** | no | `-g` / `--global` | home provider dirs (`~/.claude/`, `~/.gemini/`, `~/.codex/`) | follows you across every repo; solo use |

Agent-native precedence (project shadows user) is left to each agent; cfgd does
not re-implement it.

## The quality bar (before / after)

A skill's job is not "produce valid YAML" — that is the floor. The bar is
*exhaustive field evaluation + external research + a documented reason for every
choice*. The skill ships a worked before/after exemplar so the agent has a
concrete target. The nvim Module below went from a box-checking draft to the
thorough version (**61 → 212 lines**, every package carrying a *why*):

**Before — box-checking.** One `prefer` list, no version investigation, no
rationale; valid, but it tells a future maintainer nothing:

```yaml
spec:
  packages:
  - name: neovim
    min-version: '0.10'
    prefer: [brew, snap]
    deny: [apt]
  - name: ripgrep
  - name: fd
    aliases: { apt: fd-find }
  - name: node
    prefer: [brew, apt]
    aliases: { apt: nodejs }
  - name: gcc
    aliases: { apt: build-essential, dnf: '@development-tools' }
    platforms: [linux]
  # ...nine packages, no comments
```

**After — thorough.** Transitive build deps made explicit, versions pinned with
a reason, platforms scoped, every entry annotated with *why it is there*:

```yaml
spec:
  packages:
  # --- Native build toolchain ---------------------------------------------
  # LuaSnip (jsregexp), telescope-fzf-native, CopilotChat (tiktoken),
  # nvim-treesitter parser compilation all shell out to `make` + a C compiler.
  - name: gcc
    aliases: { apt: build-essential, dnf: '@development-tools' }
    platforms: [linux]
  - name: make
    platforms: [linux]
  - name: unzip                   # Mason unpacks language-server archives with unzip
  - name: git                     # lazy.nvim cloning, fugitive, gitsigns, diffview

  # --- CLI helpers used directly by plugins -------------------------------
  - name: fd                      # telescope find_files
    # apt ships fd as `fdfind` (no symlink); brew + cargo install it as `fd`
    # directly. Prefer those so telescope finds it without a manual PATH alias.
    prefer: [brew, cargo, apt]
    aliases: { apt: fd-find, cargo: fd-find }
  # ...26 packages, each with rationale; minVersion set; platforms scoped
```

The rendered skill enforces this with a per-invocation protocol the agent
follows: confirm the toolchain → enumerate every field with `cfgd explain` →
research best practices → decide include/omit *with a why* for each field →
draft → `cfgd <kind> validate` until clean → self-critique against the rubric.

> The skill calls `cfgd` at author time (`cfgd explain`, `cfgd <kind> validate`).
> If `cfgd` is absent or older than the skill's stamped floor, the skill stops
> and tells the user to install/upgrade it, falling back to an embedded schema
> snapshot rather than guessing the shape. Keep skills current with
> `cfgd skill update` after a cfgd upgrade — see
> [Update behavior](configuration.md#update-behavior-specupdate).

## When to use `generate` vs the skill

Both `cfgd generate` and the authoring skill draw on the *same* knowledge core,
so they agree on what a high-quality resource looks like. The difference is the
delivery model — pick by how you want the LLM driven:

| | `cfgd generate` | the authoring skill |
|---|---|---|
| Who runs the LLM | cfgd itself, via its built-in AI loop | your own agent (Claude Code / Gemini / …) |
| Token cost | your API key, metered per run | your existing agent subscription; zero marginal API cost |
| Data egress | sends home-dir contents to the API (with a consent prompt) | none — writes skill files locally |
| Lifecycle | one-shot: generates YAML, you commit it | persistent: a reusable, invocable skill |
| Reach for it when | bootstrapping config from a system scan, outside an agent session | authoring interactively inside your coding agent |

Rule of thumb: **`generate`** scans your system and proposes config *now*;
**the skill** teaches *your* agent to author cfgd resources *well, ongoing*. See
[AI-guided generation](ai-generate.md) for the `generate` flow and MCP server.

## Validating what you author

The skill's validate step (and any manual check) runs the per-kind validator:

```sh
cfgd module validate module.yaml
cfgd clusterconfigpolicy validate - -o json   # read from stdin
```

For the CRD kinds (`machineconfig`, `configpolicy`, `clusterconfigpolicy`) this
runs the *same* checks the operator's admission webhook enforces, so a document
that passes `validate` is one the cluster will admit. See
[`cfgd <kind> validate`](cli-reference.md#cfgd-kind-validate) for exit codes and
the `-o json` shape, and [`cfgd explain`](cli-reference.md#cfgd-explain) — which
now documents every kind including ClusterConfigPolicy and the cluster-side
Module CRD — for the field reference the skill enumerates.
