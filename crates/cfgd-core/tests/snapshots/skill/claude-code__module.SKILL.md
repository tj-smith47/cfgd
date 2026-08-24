---
name: cfgd-module
description: Investigate thoroughly and author a complete, validated cfgd Module resource.
user-invocable: true
cfgd-version: <CFGD_VERSION>
cfgd-min-version: <CFGD_MIN_VERSION>
---

<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd Module

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition — confirm the toolchain is usable.** Run `command -v cfgd`; if it is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>. Run `cfgd --version`; if it is older than <CFGD_MIN_VERSION>, warn and prefer the embedded fallback schema below.
1. **Enumerate every field for this kind (live-first, snapshot-fallback).** Run `cfgd explain module -o json` for the authoritative live schema, and `cfgd explain module.<field> -o json` to drill into nested objects. If cfgd is absent or older than the stamp, use the embedded fallback schema below (stamped <CFGD_VERSION>).
2. **Research best practices externally for THIS subject.** For each field, consult external best practice before settling a value: the tool's own docs, the package managers that ship it, and community conventions. Record what you verified and your confidence level when a source was unavailable. Prefer live evidence over training-knowledge recall, and state explicitly when you could not confirm a claim.
3. **For EVERY field, decide include OR omit, and justify with a WHY comment.** Box-checking is a failure; meeting the rubric above is the target.
4. **Draft thoroughly:** transitive deps explicit, version constraints set, platforms scoped, multi-step scripts idempotent (timeout + continueOnError), comments-as-specification.
5. **Validate against the schema:** `cfgd module validate <file>` — fix until clean (validate against the embedded snapshot if cfgd is unavailable).
6. **Self-critique against the rubric:** "Box-checking or thorough? Which field did I skip, and was that deliberate?" Iterate until the answer holds.

## Worked exemplar (the quality bar)

The before is a box-checking module: one prefer-list, no version investigation, no documented rationale. The after is the thorough version — every field evaluated, external best-practice research, and a documented reason for each choice — demonstrating the quality bar a skill must reach for.

Before (box-checking):

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  description: Neovim editor configuration
spec:
  packages:
  - name: neovim
    min-version: '0.10'
    prefer:
    - brew
    - snap
    deny:
    - apt
  - name: ripgrep
  - name: fd
    aliases:
      apt: fd-find
  - name: node
    prefer:
    - brew
    - apt
    aliases:
      apt: nodejs
  - name: python3
  - name: pip
    aliases:
      apt: python3-pip
    platforms:
    - linux
  - name: go
    prefer:
    - brew
    - apt
  - name: curl
  - name: gcc
    aliases:
      apt: build-essential
      dnf: '@development-tools'
    platforms:
    - linux
  files:
  - source: files/init.lua
    target: /root/.config/nvim/init.lua
  - source: files/lazy-lock.json
    target: /root/.config/nvim/lazy-lock.json
  - source: files/stylua.toml
    target: /root/.config/nvim/stylua.toml
  - source: files/lua
    target: /root/.config/nvim/lua
  - source: files/after
    target: /root/.config/nvim/after
  env:
  - name: EDITOR
    value: nvim
  aliases:
  - name: v
    command: nvim
  scripts:
    post-apply:
    - nvim --headless '+Lazy! sync' '+MasonToolsInstallSync' +qa
```

After (thorough):

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  description: Neovim editor — LazyVim-style plugin set with Mason-managed LSP, treesitter, copilot, sops.

spec:
  packages:
  # --- Editor ---------------------------------------------------------------
  - name: neovim
    minVersion: '0.11'
    prefer:
    - brew
    - snap
    deny:
    - apt                         # Ubuntu LTS apt nvim is too old for our plugin set

  # --- Native build toolchain ---------------------------------------------
  # LuaSnip (jsregexp), telescope-fzf-native, CopilotChat (tiktoken),
  # nvim-treesitter parser compilation all shell out to `make` + a C compiler.
  - name: gcc
    aliases:
      apt: build-essential
      dnf: '@development-tools'
    platforms:
    - linux
  - name: make
    platforms:
    - linux
  - name: unzip                   # Mason unpacks language-server archives with unzip
  - name: git                     # lazy.nvim cloning, fugitive, gitsigns, diffview
  - name: curl

  # --- CLI helpers used directly by plugins -------------------------------
  - name: ripgrep                 # telescope live_grep, todo-comments
  - name: fd                      # telescope find_files
    # apt ships fd as `fdfind` (no symlink); brew + cargo install the binary
    # as `fd` directly. Prefer those so telescope can find it without a manual
    # alias on the user's PATH.
    prefer:
    - brew
    - cargo
    - apt
    aliases:
      apt: fd-find
      cargo: fd-find
  - name: zoxide                  # telescope-zoxide
    prefer:
    - brew
    - cargo
    - apt

  # --- Linux desktop providers --------------------------------------------
  # X11 / Wayland clipboard providers. Harmless on headless boxes; nvim's
  # OSC52 fallback (configured in lua/config/options.lua under $SSH_TTY)
  # covers SSH-only setups like Termius.
  - name: xclip
    platforms:
    - linux
  - name: wl-clipboard
    platforms:
    - linux
  - name: xdg-utils               # url-open's `gx` → xdg-open
    platforms:
    - linux

  # --- Node toolchain -----------------------------------------------------
  # copilot.vim needs node ≥18. markdown-preview prefers yarn but falls back
  # to npm. Mason pulls dozens of JS-based language servers via npm.
  - name: node
    minVersion: '18'
    prefer:
    - brew
    - apt
    aliases:
      apt: nodejs
  - name: npm                     # apt's nodejs package doesn't always include npm
    platforms:
    - linux
  - name: yarn
    prefer:
    - npm
    - brew
  # Node provider for nvim — needed by remote/JS plugins. Without it,
  # :checkhealth reports "Missing 'neovim' npm package". Same canonical
  # `neovim` name as the editor entry above; cfgd's resolver treats per-manager
  # entries independently.
  - name: neovim
    prefer:
    - npm

  # --- Python toolchain ---------------------------------------------------
  # Mason installs many python-based linters/formatters via pip. dap-python
  # needs a real venv. pynvim is the python provider (vim.python3_host_prog).
  - name: python3
  - name: pip
    aliases:
      apt: python3-pip
    platforms:
    - linux
  - name: python3-venv            # pipx + debugpy + dap-python require venv
    aliases:
      dnf: python3-virtualenv
    platforms:
    - linux
  - name: pipx
    prefer:
    - brew
    - apt
  - name: pynvim                  # vim.python3 provider for nvim
    prefer:
    - pipx

  # --- Go toolchain -------------------------------------------------------
  # go.nvim's :GoInstallBinaries calls `go install` for dlv, gotests, iferr,
  # fillstruct, gomodifytags. Mason also installs gopls/gofumpt/golines via go.
  - name: go
    minVersion: '1.25'
    prefer:
    - brew
    - apt

  # --- Rust toolchain -----------------------------------------------------
  # stylua is a Rust binary; cargo provides a reliable fallback when brew isn't
  # available on the host.
  - name: cargo
    aliases:
      brew: rust
      apt: rustc
  - name: stylua                  # Lua formatter used by conform.nvim
    prefer:
    - cargo
    - brew

  # --- Secrets ------------------------------------------------------------
  - name: sops                    # sops.nvim + nvim-sops
    prefer:
    - brew
    - apt
  - name: age                     # sops age backend
    prefer:
    - brew
    - apt

  files:
  - source: files/init.lua
    target: ~/.config/nvim/init.lua
  - source: files/lazy-lock.json
    target: ~/.config/nvim/lazy-lock.json
  - source: files/stylua.toml
    target: ~/.config/nvim/stylua.toml
  - source: files/lua
    target: ~/.config/nvim/lua
  - source: files/after
    target: ~/.config/nvim/after

  env:
  - name: EDITOR
    value: nvim
  - name: VISUAL
    value: nvim
  - name: PATH
    value: $HOME/.local/bin:$HOME/go/bin:$HOME/.cargo/bin:$PATH

  aliases:
  - name: v
    command: nvim
  - name: vim
    command: nvim
  - name: vi
    command: nvim

  scripts:
    # First-run bootstrap. Each step is idempotent; cfgd will skip on second apply
    # only via timestamp tracking, but the underlying commands handle re-runs.
    # spec.env above (PATH, EDITOR, VISUAL) is injected into each step's process
    # environment, so the nvim binary and ~/.local/bin, go/bin, cargo/bin
    # toolchains are already on PATH — no per-step export needed.
    postApply:
    - run: |
        if command -v pipx >/dev/null 2>&1; then
          pipx install --force pynvim 2>&1 | tail -5 || true
        fi
      timeout: 120s
      continueOnError: true
    - run: |
        nvim --headless "+Lazy! restore" +qa
      timeout: 900s
    - run: |
        nvim --headless "+TSUpdateSync" +qa
      timeout: 600s
    - run: |
        nvim --headless "+MasonToolsInstallSync" +qa
      timeout: 900s
    - run: |
        nvim --headless "+GoInstallBinaries" +qa
      timeout: 300s
      continueOnError: true
    # markdown-preview.nvim's `build` function is gated on `#ui > 0`, so headless
    # installs skip the node-app install. Do it directly from the lazy plugin dir.
    - run: |
        mp="$HOME/.local/share/nvim/lazy/markdown-preview.nvim/app"
        if [ -d "$mp" ]; then
          cd "$mp"
          if command -v yarn >/dev/null 2>&1; then
            yarn install --frozen-lockfile
          else
            npm install --no-audit --no-fund
          fi
        fi
      timeout: 180s
      continueOnError: true
```

## Ground-truth examples

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  description: Neovim editor — LazyVim-style plugin set with Mason-managed LSP, treesitter, copilot, sops.

spec:
  packages:
  # --- Editor ---------------------------------------------------------------
  - name: neovim
    minVersion: '0.11'
    prefer:
    - brew
    - snap
    deny:
    - apt                         # Ubuntu LTS apt nvim is too old for our plugin set

  # --- Native build toolchain ---------------------------------------------
  # LuaSnip (jsregexp), telescope-fzf-native, CopilotChat (tiktoken),
  # nvim-treesitter parser compilation all shell out to `make` + a C compiler.
  - name: gcc
    aliases:
      apt: build-essential
      dnf: '@development-tools'
    platforms:
    - linux
  - name: make
    platforms:
    - linux
  - name: unzip                   # Mason unpacks language-server archives with unzip
  - name: git                     # lazy.nvim cloning, fugitive, gitsigns, diffview
  - name: curl

  # --- CLI helpers used directly by plugins -------------------------------
  - name: ripgrep                 # telescope live_grep, todo-comments
  - name: fd                      # telescope find_files
    # apt ships fd as `fdfind` (no symlink); brew + cargo install the binary
    # as `fd` directly. Prefer those so telescope can find it without a manual
    # alias on the user's PATH.
    prefer:
    - brew
    - cargo
    - apt
    aliases:
      apt: fd-find
      cargo: fd-find
  - name: zoxide                  # telescope-zoxide
    prefer:
    - brew
    - cargo
    - apt

  # --- Linux desktop providers --------------------------------------------
  # X11 / Wayland clipboard providers. Harmless on headless boxes; nvim's
  # OSC52 fallback (configured in lua/config/options.lua under $SSH_TTY)
  # covers SSH-only setups like Termius.
  - name: xclip
    platforms:
    - linux
  - name: wl-clipboard
    platforms:
    - linux
  - name: xdg-utils               # url-open's `gx` → xdg-open
    platforms:
    - linux

  # --- Node toolchain -----------------------------------------------------
  # copilot.vim needs node ≥18. markdown-preview prefers yarn but falls back
  # to npm. Mason pulls dozens of JS-based language servers via npm.
  - name: node
    minVersion: '18'
    prefer:
    - brew
    - apt
    aliases:
      apt: nodejs
  - name: npm                     # apt's nodejs package doesn't always include npm
    platforms:
    - linux
  - name: yarn
    prefer:
    - npm
    - brew
  # Node provider for nvim — needed by remote/JS plugins. Without it,
  # :checkhealth reports "Missing 'neovim' npm package". Same canonical
  # `neovim` name as the editor entry above; cfgd's resolver treats per-manager
  # entries independently.
  - name: neovim
    prefer:
    - npm

  # --- Python toolchain ---------------------------------------------------
  # Mason installs many python-based linters/formatters via pip. dap-python
  # needs a real venv. pynvim is the python provider (vim.python3_host_prog).
  - name: python3
  - name: pip
    aliases:
      apt: python3-pip
    platforms:
    - linux
  - name: python3-venv            # pipx + debugpy + dap-python require venv
    aliases:
      dnf: python3-virtualenv
    platforms:
    - linux
  - name: pipx
    prefer:
    - brew
    - apt
  - name: pynvim                  # vim.python3 provider for nvim
    prefer:
    - pipx

  # --- Go toolchain -------------------------------------------------------
  # go.nvim's :GoInstallBinaries calls `go install` for dlv, gotests, iferr,
  # fillstruct, gomodifytags. Mason also installs gopls/gofumpt/golines via go.
  - name: go
    minVersion: '1.25'
    prefer:
    - brew
    - apt

  # --- Rust toolchain -----------------------------------------------------
  # stylua is a Rust binary; cargo provides a reliable fallback when brew isn't
  # available on the host.
  - name: cargo
    aliases:
      brew: rust
      apt: rustc
  - name: stylua                  # Lua formatter used by conform.nvim
    prefer:
    - cargo
    - brew

  # --- Secrets ------------------------------------------------------------
  - name: sops                    # sops.nvim + nvim-sops
    prefer:
    - brew
    - apt
  - name: age                     # sops age backend
    prefer:
    - brew
    - apt

  files:
  - source: files/init.lua
    target: ~/.config/nvim/init.lua
  - source: files/lazy-lock.json
    target: ~/.config/nvim/lazy-lock.json
  - source: files/stylua.toml
    target: ~/.config/nvim/stylua.toml
  - source: files/lua
    target: ~/.config/nvim/lua
  - source: files/after
    target: ~/.config/nvim/after

  env:
  - name: EDITOR
    value: nvim
  - name: VISUAL
    value: nvim
  - name: PATH
    value: $HOME/.local/bin:$HOME/go/bin:$HOME/.cargo/bin:$PATH

  aliases:
  - name: v
    command: nvim
  - name: vim
    command: nvim
  - name: vi
    command: nvim

  scripts:
    # First-run bootstrap. Each step is idempotent; cfgd will skip on second apply
    # only via timestamp tracking, but the underlying commands handle re-runs.
    # spec.env above (PATH, EDITOR, VISUAL) is injected into each step's process
    # environment, so the nvim binary and ~/.local/bin, go/bin, cargo/bin
    # toolchains are already on PATH — no per-step export needed.
    postApply:
    - run: |
        if command -v pipx >/dev/null 2>&1; then
          pipx install --force pynvim 2>&1 | tail -5 || true
        fi
      timeout: 120s
      continueOnError: true
    - run: |
        nvim --headless "+Lazy! restore" +qa
      timeout: 900s
    - run: |
        nvim --headless "+TSUpdateSync" +qa
      timeout: 600s
    - run: |
        nvim --headless "+MasonToolsInstallSync" +qa
      timeout: 900s
    - run: |
        nvim --headless "+GoInstallBinaries" +qa
      timeout: 300s
      continueOnError: true
    # markdown-preview.nvim's `build` function is gated on `#ui > 0`, so headless
    # installs skip the node-app install. Do it directly from the lazy plugin dir.
    - run: |
        mp="$HOME/.local/share/nvim/lazy/markdown-preview.nvim/app"
        if [ -d "$mp" ]; then
          cd "$mp"
          if command -v yarn >/dev/null 2>&1; then
            yarn install --frozen-lockfile
          else
            npm install --no-audit --no-fund
          fi
        fi
      timeout: 180s
      continueOnError: true
```

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: clift
  description: clift framework for building custom CLIs with go-task

spec:
  packages:
    - name: go-task
      prefer: [brew, script]
      aliases:
        brew: go-task
      script: |
        sh -c "$(curl --location https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin

    - name: jq

    - name: yq

    - name: gum

  files:
    - source: https://github.com/tj-smith47/clift.git
      target: ~/.local/share/clift

  env:
    - name: CLIFT_DIR
      value: ~/.local/share/clift

  scripts:
    postApply:
      - touch ~/.local/share/clift/.cfgd-managed
```

## Fallback schema (if cfgd is unavailable)

Generated against cfgd <CFGD_VERSION>. Live `cfgd explain module` is authoritative when present.

```json
{"$schema":"https://json-schema.org/draft-07/schema#","additionalProperties":false,"definitions":{"EncryptionMode":{"description":"Controls when encryption is required for a managed file.","oneOf":[{"const":"InRepo","description":"File must be encrypted when stored in the repository.","type":"string"},{"const":"Always","description":"File must always be encrypted, including at rest on disk.","type":"string"}]},"EncryptionSpec":{"additionalProperties":false,"description":"Encryption settings for a managed file.","properties":{"backend":{"description":"The encryption backend to use (e.g. \"sops\", \"age\").","type":"string"},"mode":{"$ref":"#/definitions/EncryptionMode","default":"InRepo","description":"When encryption must be enforced. Defaults to `InRepo`."}},"required":["backend"],"type":"object"},"EnvVar":{"description":"A single `NAME=VALUE` environment variable entry.","properties":{"name":{"description":"Variable name. Must be shell-safe and not a reserved `CFGD_*` name.","type":"string"},"value":{"description":"Value assigned to the variable, exported verbatim into the shell environment.","type":"string"}},"required":["name","value"],"type":"object"},"FileStrategy":{"description":"File deployment strategy.","oneOf":[{"const":"Symlink","description":"Create a symbolic link from target to source (default).","type":"string"},{"const":"Copy","description":"Copy source content to target.","type":"string"},{"const":"Template","description":"Render a Tera template and write the output (auto-selected for .tera files).","type":"string"},{"const":"Hardlink","description":"Create a hard link from target to source.","type":"string"},{"const":"Patch","description":"Merge structured keys/values into the target, or pipe it through a script, leaving everything else untouched. Requires a `patch:` block.","type":"string"}]},"ModuleFileEntry":{"additionalProperties":false,"description":"One entry of `spec.files[]`: a file this module deploys. ```yaml files: - source: files/init.lua target: ~/.config/nvim/init.lua ```","properties":{"encryption":{"anyOf":[{"$ref":"#/definitions/EncryptionSpec"},{"type":"null"}],"description":"Encryption settings for this module file."},"patch":{"anyOf":[{"$ref":"#/definitions/PatchSpec"},{"type":"null"}],"description":"Structured merge or script configuration for `strategy: Patch`. Required when `strategy` is `Patch`, rejected otherwise (enforced by `validate_module_file_entries`, not the JSON schema)."},"permissions":{"description":"Unix permission bits (e.g. \"600\", \"644\") to apply after deployment.","type":["string","null"]},"private":{"description":"When true, the source file is local-only: auto-added to .gitignore, silently skipped on machines where it doesn't exist.","type":"boolean"},"source":{"default":"","description":"Path to the source file, relative to the module directory. Not required when `strategy` is `Patch`; required otherwise (enforced by `validate_module_file_entries`, not the JSON schema).","type":"string"},"strategy":{"anyOf":[{"$ref":"#/definitions/FileStrategy"},{"type":"null"}],"description":"Per-file deployment strategy override. If None, uses the global default."},"target":{"description":"Destination path on the machine. A leading `~` expands to the home directory.","type":"string"}},"required":["target"],"type":"object"},"ModulePackageEntry":{"additionalProperties":false,"description":"One entry of `spec.packages[]`: a package this module installs. ```yaml packages: - name: neovim minVersion: \"0.9\" prefer: [brew, apt] ```","properties":{"aliases":{"additionalProperties":{"type":"string"},"description":"Manager-specific package name aliases (e.g. `{apt: \"neovim\", brew: \"neovim\"}`) for a package named differently across managers.","type":"object"},"creates":{"description":"Skip the install script if this path already exists. A leading `~` expands to the home directory; a relative path resolves against the script's working directory. Existence follows symlinks. Only meaningful for a `prefer: [script]` install; ignored otherwise.","type":["string","null"]},"deny":{"description":"Package managers to never use for this package, even if otherwise available and preferred by the profile.","items":{"type":"string"},"type":"array"},"minVersion":{"description":"Minimum acceptable installed version, loosely parsed (`\"1.2\"`, `\"1\"`). A version below this is treated as not satisfying the module.","type":["string","null"]},"name":{"default":"","description":"The package name as the chosen manager knows it.","type":"string"},"onlyIf":{"description":"Run the install script only if this command exits zero. A non-zero exit skips the install (the condition for installing was not met). Only meaningful for a `prefer: [script]` install; ignored for manager-backed installs (those are idempotent via the manager's installed-package query).","type":["string","null"]},"platforms":{"description":"Platform tags gating this package alone. Empty means install on every platform the module itself is not already gated off of.","items":{"type":"string"},"type":"array"},"prefer":{"description":"Manager preference order for this package, overriding the profile's default manager priority (e.g. `[brew, apt]`, or `[script]` to force the install script below).","items":{"type":"string"},"type":"array"},"script":{"description":"Shell script to run instead of a manager install, selected via `prefer: [script]`.","type":["string","null"]},"unless":{"description":"Run the install script only if this command exits NON-zero. A zero exit (success) skips the install (the package already appears present). Only meaningful for a `prefer: [script]` install; ignored otherwise.","type":["string","null"]}},"type":"object"},"PatchFormat":{"description":"File format used to interpret and re-serialize a `Patch`-strategy target.","oneOf":[{"const":"Ini","description":"INI sections/keys, edited line-by-line to preserve comments and layout.","type":"string"},{"const":"Json","description":"JSON, re-serialized on write (no comments to preserve).","type":"string"},{"const":"Yaml","description":"YAML; comments are NOT preserved across a merge (see docs for the caveat).","type":"string"},{"const":"Toml","description":"TOML, edited via `toml_edit` to preserve comments and layout.","type":"string"}]},"PatchSpec":{"additionalProperties":false,"description":"Configuration for the `Patch` file strategy: a structured merge (`ensure`) or a content-rewriting script, applied on top of the target's current content.","properties":{"ensure":{"description":"Keys/values to deep-merge into the target, leaving unmentioned keys untouched. Values are literal (no template rendering). Mutually exclusive with `script`."},"format":{"anyOf":[{"$ref":"#/definitions/PatchFormat"},{"type":"null"}],"description":"File format to parse the target as. Inferred from the target's extension when omitted."},"script":{"description":"A script path or an inline command that receives the target's current content on stdin and writes the new content to stdout. A relative path resolves against the module directory for a module file (`spec.files[]`) and against the config directory for a profile file (`spec.files.managed[]`); a value that resolves to no file is run as an inline command. Mutually exclusive with `ensure`.","type":["string","null"]}},"type":"object"},"ScriptEntry":{"anyOf":[{"description":"A bare command string, run with the platform's default shell and no timeout/guard.","type":"string"},{"properties":{"continueOnError":{"description":"Treat a non-zero exit as success and continue reconciliation instead of failing the run. Default: `false`.","type":["boolean","null"]},"creates":{"description":"Skip the script if this path already exists. A leading `~` expands to the home directory; a relative path resolves against the script's working directory. Existence follows symlinks.","type":["string","null"]},"idleTimeout":{"description":"Kill the script if it produces no stdout/stderr output for this duration. Prevents scripts from silently hanging on unresponsive resources. Format: \"30s\", \"2m\", etc. If unset, no idle timeout is enforced.","type":["string","null"]},"interactive":{"description":"Run the script attached to the terminal (inherited stdin/stdout/stderr, no spinner, no output capture, no idle timeout) so it can prompt the user — e.g. `echo \"press Enter when done\"; read`. Requires a TTY: when stdin is not a terminal (CI, piped input, or any daemon-run phase) the script is skipped with a warning rather than hanging on instant EOF.","type":"boolean"},"onlyIf":{"description":"Run the script only if this command exits zero. A non-zero exit skips the script (the condition for running was not met). Evaluated with the same shell, working directory, and environment as the body.","type":["string","null"]},"run":{"description":"The command or script body to run.","type":"string"},"shell":{"$ref":"#/definitions/ScriptShell","description":"Interpreter to use for inline commands. Ignored (and rejected) on file scripts."},"timeout":{"description":"Kill the script if it runs longer than this duration (`\"30s\"`, `\"2m\"`). Unset means no timeout.","type":["string","null"]},"unless":{"description":"Run the script only if this command exits NON-zero. A zero exit (success) skips the script (the guarded state already holds). Evaluated with the same shell, working directory, and environment as the body.","type":["string","null"]},"workdir":{"description":"Working directory for the script. By default every lifecycle script runs in the user's home directory — never the config source tree — so a relative write can't pollute the user's GitOps repo. Set `workdir` to override: a leading `~` expands to home and `$VAR`/`${VAR}` expand against the script environment (which always carries `$CFGD_MODULE_DIR` and `$CFGD_CONFIG_DIR`), so `workdir: ~/.local/share/app`, `workdir: $CFGD_MODULE_DIR`, or an absolute path all work.","type":["string","null"]}},"required":["run"],"type":"object"}],"description":"A lifecycle script entry: either a bare command string, or a mapping for one that needs a timeout, shell, or guard condition. ```yaml preApply: \"echo starting\" # or postApply: run: brew update timeout: 2m onlyIf: command -v brew ```"},"ScriptShell":{"description":"Interpreter for inline lifecycle scripts.","oneOf":[{"enum":["sh","bash","zsh","pwsh","cmd"],"type":"string"},{"const":"auto","description":"Platform default: `sh` on Unix, `cmd.exe` on Windows.","type":"string"}]},"ScriptSpec":{"additionalProperties":false,"description":"`spec.scripts`: lifecycle hooks run at specific points in the reconcile cycle. ```yaml scripts: preApply: \"echo starting apply\" postApply: - run: brew cleanup continueOnError: true onDrift: \"notify-send 'cfgd: drift detected'\" ```","properties":{"onChange":{"default":[],"description":"Run when a watched file changes on disk (requires `daemon.reconcile.onChange`).","items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"onDrift":{"default":[],"description":"Run when the daemon detects drift, before any auto-apply decision.","items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"postApply":{"default":[],"description":"Run once after every action in an apply completes.","items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"postReconcile":{"default":[],"description":"Run once after a daemon reconcile tick completes.","items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"preApply":{"default":[],"description":"Run once before any action in an apply.","items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"preReconcile":{"default":[],"description":"Run once before a daemon reconcile tick begins.","items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"}},"type":"object"},"ShellAlias":{"description":"A single shell alias entry.","properties":{"command":{"description":"Command the alias expands to, written in the syntax of the shell it is generated for. It may carry arguments, pipes and quotes: cfgd quotes the whole value per dialect when it writes the alias definition, so the text reaches the shell exactly as declared. Required — an alias with no command has nothing to expand to.","type":"string"},"name":{"description":"Alias name, as typed at the shell prompt.","type":"string"}},"required":["name","command"],"type":"object"}},"description":"`spec`: the declared surface of a module — everything it contributes to a profile that includes it.","properties":{"aliases":{"description":"Shell aliases this module contributes.","items":{"$ref":"#/definitions/ShellAlias"},"type":"array"},"depends":{"description":"Names of other modules this one requires; cfgd resolves and applies them first.","items":{"type":"string"},"type":"array"},"env":{"description":"Environment variables this module contributes.","items":{"$ref":"#/definitions/EnvVar"},"type":"array"},"files":{"description":"Files this module deploys.","items":{"$ref":"#/definitions/ModuleFileEntry"},"type":"array"},"packages":{"description":"Packages this module installs.","items":{"$ref":"#/definitions/ModulePackageEntry"},"type":"array"},"platforms":{"description":"Platform tags gating the whole module. When non-empty and the current platform matches none of them, the module is skipped entirely (it appears as a Skipped action rather than vanishing). Tags are matched against OS / distro / arch via `Platform::matches_any`; the canonical macOS token is `macos`.","items":{"type":"string"},"type":"array"},"scripts":{"anyOf":[{"$ref":"#/definitions/ScriptSpec"},{"type":"null"}],"description":"Lifecycle scripts (`preApply`, `postApply`, …) this module runs."},"system":{"additionalProperties":true,"description":"System configurator settings contributed by this module. Deep-merged into the profile system map; module values override profile values at leaf level.","type":"object"}},"title":"ModuleSpec","type":"object"}
```

