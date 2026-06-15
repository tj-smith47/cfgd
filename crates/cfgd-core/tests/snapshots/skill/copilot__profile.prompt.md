---
mode: agent
description: Investigate thoroughly and author a complete, validated cfgd Profile resource.
cfgd-version: <CFGD_VERSION>
cfgd-min-version: <CFGD_MIN_VERSION>
---

<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd Profile

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition — confirm the toolchain is usable.** Run `command -v cfgd`; if it is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>. Run `cfgd --version`; if it is older than <CFGD_MIN_VERSION>, warn and prefer the embedded fallback schema below.
1. **Enumerate every field for this kind (live-first, snapshot-fallback).** Run `cfgd explain profile -o json` for the authoritative live schema, and `cfgd explain profile.<field> -o json` to drill into nested objects. If cfgd is absent or older than the stamp, use the embedded fallback schema below (stamped <CFGD_VERSION>).
2. **Research best practices externally for THIS subject.** For each field, consult external best practice before settling a value: the tool's own docs, the package managers that ship it, and community conventions. Record what you verified and your confidence level when a source was unavailable. Prefer live evidence over training-knowledge recall, and state explicitly when you could not confirm a claim.
3. **For EVERY field, decide include OR omit, and justify with a WHY comment.** Box-checking is a failure; meeting the rubric above is the target.
4. **Draft thoroughly:** transitive deps explicit, version constraints set, platforms scoped, multi-step scripts idempotent (timeout + continueOnError), comments-as-specification.
5. **Validate against the schema:** `cfgd profile validate <file>` — fix until clean (validate against the embedded snapshot if cfgd is unavailable).
6. **Self-critique against the rubric:** "Box-checking or thorough? Which field did I skip, and was that deliberate?" Iterate until the answer holds.

## Ground-truth examples

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  env:
    - name: EDITOR
      value: vim
    - name: GIT_AUTHOR_NAME
      value: Your Name
    - name: GIT_AUTHOR_EMAIL
      value: you@example.com

  packages:
    cargo:
      - bat
      - eza
      - fd-find
      - ripgrep

  files:
    managed:
      - source: shell/.zshrc
        target: ~/.zshrc
      - source: git/.gitconfig.tera
        target: ~/.gitconfig
    permissions:
      "~/.ssh": "700"
      "~/.ssh/config": "600"

  system:
    shell: /bin/zsh
```

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - base

  env:
    - name: EDITOR
      value: "code --wait"
    - name: GIT_AUTHOR_EMAIL
      value: you@company.com

  packages:
    brew:
      taps:
        - homebrew/cask-fonts
      formulae:
        - git
        - jq
        - kubectl
        - helm
      casks:
        - visual-studio-code
        - wezterm
    npm:
      global:
        - typescript
        - prettier

  files:
    managed:
      - source: k8s/kubeconfig.tera
        target: ~/.kube/config

  secrets:
    - source: 1password://Work/GitHub/token
      target: ~/.config/gh/token

  scripts:
    postReconcile:
      - scripts/setup-work-env.sh
```

## Fallback schema (if cfgd is unavailable)

Generated against cfgd <CFGD_VERSION>. Live `cfgd explain profile` is authoritative when present.

```json
{"$schema":"http://json-schema.org/draft-07/schema#","title":"ProfileSpec","type":"object","properties":{"aliases":{"default":[],"type":"array","items":{"$ref":"#/definitions/ShellAlias"}},"env":{"default":[],"type":"array","items":{"$ref":"#/definitions/EnvVar"}},"envScope":{"description":"How far `spec.env` exports reach across the current user's environment. Omitted means \"inherit\" (a parent layer's value survives); the resolved default when no layer sets it is [`EnvScope::All`] — every standard user entry point cfgd can safely touch. Narrow it to `Login` or `Interactive` to opt out of the broader session surfaces.","anyOf":[{"$ref":"#/definitions/EnvScope"},{"type":"null"}]},"files":{"default":null,"anyOf":[{"$ref":"#/definitions/FilesSpec"},{"type":"null"}]},"inherits":{"default":[],"type":"array","items":{"type":"string"}},"modules":{"default":[],"type":"array","items":{"type":"string"}},"packages":{"default":null,"anyOf":[{"$ref":"#/definitions/PackagesSpec"},{"type":"null"}]},"scripts":{"default":null,"anyOf":[{"$ref":"#/definitions/ScriptSpec"},{"type":"null"}]},"secrets":{"default":[],"type":"array","items":{"$ref":"#/definitions/SecretSpec"}},"system":{"default":{},"type":"object","additionalProperties":true}},"additionalProperties":false,"definitions":{"AptSpec":{"type":"object","properties":{"file":{"default":null,"type":["string","null"]},"packages":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"BrewSpec":{"type":"object","properties":{"casks":{"default":[],"type":"array","items":{"type":"string"}},"file":{"default":null,"type":["string","null"]},"formulae":{"default":[],"type":"array","items":{"type":"string"}},"taps":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"CargoSpec":{"description":"Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`) and object form (`cargo: { file: Cargo.toml, packages: [...] }`) via the shared `list_or_struct` deserializer on the `PackagesSpec::cargo` field.","type":"object","properties":{"file":{"type":["string","null"]},"packages":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"CustomManagerSpec":{"type":"object","required":["check","install","listInstalled","name","uninstall"],"properties":{"check":{"type":"string"},"install":{"type":"string"},"listInstalled":{"type":"string"},"name":{"type":"string"},"packages":{"default":[],"type":"array","items":{"type":"string"}},"uninstall":{"type":"string"},"update":{"default":null,"type":["string","null"]}},"additionalProperties":false},"EncryptionMode":{"description":"Controls when encryption is required for a managed file.","oneOf":[{"description":"File must be encrypted when stored in the repository.","type":"string","enum":["InRepo"]},{"description":"File must always be encrypted, including at rest on disk.","type":"string","enum":["Always"]}]},"EncryptionSpec":{"description":"Encryption settings for a managed file.","type":"object","required":["backend"],"properties":{"backend":{"description":"The encryption backend to use (e.g. \"sops\", \"age\").","type":"string"},"mode":{"description":"When encryption must be enforced. Defaults to `InRepo`.","default":"InRepo","allOf":[{"$ref":"#/definitions/EncryptionMode"}]}},"additionalProperties":false},"EnvScope":{"description":"How far `spec.env` exports reach across the current user's environment.\n\nThe two env fields differ by *scope of affected users*: `spec.env` targets the current user, `spec.system.environment` targets all users (privileged). This knob narrows the *current-user* reach; it never widens beyond the user.","oneOf":[{"description":"Every standard user entry point cfgd can safely touch: interactive + login shells, `systemd --user` / Wayland GUI sessions, macOS GUI apps, and an immediate live-session refresh. The default — no gotchas.","type":"string","enum":["All"]},{"description":"Interactive shells plus login shells (`~/.zshenv`, `~/.profile`, and an existing `~/.bash_profile`). Excludes the GUI / `systemd --user` session surfaces and the live-session refresh.","type":"string","enum":["Login"]},{"description":"Interactive shells only (`~/.bashrc` / `~/.zshrc`, fish conf.d) — the historical behavior before full reach.","type":"string","enum":["Interactive"]}]},"EnvVar":{"type":"object","required":["name","value"],"properties":{"name":{"type":"string"},"value":{"type":"string"}}},"FileStrategy":{"description":"File deployment strategy.","oneOf":[{"description":"Create a symbolic link from target to source (default).","type":"string","enum":["Symlink"]},{"description":"Copy source content to target.","type":"string","enum":["Copy"]},{"description":"Render a Tera template and write the output (auto-selected for .tera files).","type":"string","enum":["Template"]},{"description":"Create a hard link from target to source.","type":"string","enum":["Hardlink"]}]},"FilesSpec":{"type":"object","properties":{"managed":{"default":[],"type":"array","items":{"$ref":"#/definitions/ManagedFileSpec"}},"permissions":{"default":{},"type":"object","additionalProperties":{"type":"string"}}},"additionalProperties":false},"FlatpakSpec":{"type":"object","properties":{"packages":{"default":[],"type":"array","items":{"type":"string"}},"remote":{"default":null,"type":["string","null"]}},"additionalProperties":false},"ManagedFileSpec":{"type":"object","required":["source","target"],"properties":{"encryption":{"description":"Encryption settings for this file.","anyOf":[{"$ref":"#/definitions/EncryptionSpec"},{"type":"null"}]},"permissions":{"description":"Unix permission bits (e.g. \"600\", \"644\") to apply after deployment.","type":["string","null"]},"private":{"description":"When true, the source file is local-only: auto-added to .gitignore, silently skipped on machines where it doesn't exist.","type":"boolean"},"source":{"type":"string"},"strategy":{"description":"Per-file deployment strategy override. If None, uses the global default.","anyOf":[{"$ref":"#/definitions/FileStrategy"},{"type":"null"}]},"target":{"type":"string"}},"additionalProperties":false},"NpmSpec":{"type":"object","properties":{"file":{"default":null,"type":["string","null"]},"global":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"PackagesSpec":{"type":"object","properties":{"apk":{"default":[],"type":"array","items":{"type":"string"}},"apt":{"default":null,"anyOf":[{"$ref":"#/definitions/AptSpec"},{"type":"null"}]},"brew":{"default":null,"anyOf":[{"$ref":"#/definitions/BrewSpec"},{"type":"null"}]},"cargo":{"default":null,"anyOf":[{"$ref":"#/definitions/CargoSpec"},{"type":"null"}]},"chocolatey":{"default":[],"type":"array","items":{"type":"string"}},"custom":{"default":[],"type":"array","items":{"$ref":"#/definitions/CustomManagerSpec"}},"dnf":{"default":[],"type":"array","items":{"type":"string"}},"flatpak":{"default":null,"anyOf":[{"$ref":"#/definitions/FlatpakSpec"},{"type":"null"}]},"go":{"default":[],"type":"array","items":{"type":"string"}},"nix":{"default":[],"type":"array","items":{"type":"string"}},"npm":{"default":null,"anyOf":[{"$ref":"#/definitions/NpmSpec"},{"type":"null"}]},"pacman":{"default":[],"type":"array","items":{"type":"string"}},"pipx":{"default":[],"type":"array","items":{"type":"string"}},"pkg":{"default":[],"type":"array","items":{"type":"string"}},"scoop":{"default":[],"type":"array","items":{"type":"string"}},"snap":{"default":null,"anyOf":[{"$ref":"#/definitions/SnapSpec"},{"type":"null"}]},"winget":{"default":[],"type":"array","items":{"type":"string"}},"yum":{"default":[],"type":"array","items":{"type":"string"}},"zypper":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"ScriptEntry":{"anyOf":[{"type":"string"},{"type":"object","required":["run"],"properties":{"continueOnError":{"type":["boolean","null"]},"creates":{"description":"Skip the script if this path already exists. A leading `~` expands to the home directory; a relative path resolves against the script's working directory. Existence follows symlinks.","type":["string","null"]},"idleTimeout":{"description":"Kill the script if it produces no stdout/stderr output for this duration. Prevents scripts from silently hanging on unresponsive resources. Format: \"30s\", \"2m\", etc. If unset, no idle timeout is enforced.","type":["string","null"]},"interactive":{"description":"Run the script attached to the terminal (inherited stdin/stdout/stderr, no spinner, no output capture, no idle timeout) so it can prompt the user — e.g. `echo \"press Enter when done\"; read`. Requires a TTY: when stdin is not a terminal (CI, piped input, or any daemon-run phase) the script is skipped with a warning rather than hanging on instant EOF.","type":"boolean"},"onlyIf":{"description":"Run the script only if this command exits zero. A non-zero exit skips the script (the condition for running was not met). Evaluated with the same shell, working directory, and environment as the body.","type":["string","null"]},"run":{"type":"string"},"shell":{"description":"Interpreter to use for inline commands. Ignored (and rejected) on file scripts.","allOf":[{"$ref":"#/definitions/ScriptShell"}]},"timeout":{"type":["string","null"]},"unless":{"description":"Run the script only if this command exits NON-zero. A zero exit (success) skips the script (the guarded state already holds). Evaluated with the same shell, working directory, and environment as the body.","type":["string","null"]},"workdir":{"description":"Working directory for the script. By default every lifecycle script runs in the user's home directory — never the config source tree — so a relative write can't pollute the user's GitOps repo. Set `workdir` to override: a leading `~` expands to home and `$VAR`/`${VAR}` expand against the script environment (which always carries `$CFGD_MODULE_DIR` and `$CFGD_CONFIG_DIR`), so `workdir: ~/.local/share/app`, `workdir: $CFGD_MODULE_DIR`, or an absolute path all work.","type":["string","null"]}}}]},"ScriptShell":{"description":"Interpreter for inline lifecycle scripts.","oneOf":[{"type":"string","enum":["sh","bash","zsh","pwsh","cmd"]},{"description":"Platform default: `sh` on Unix, `cmd.exe` on Windows.","type":"string","enum":["auto"]}]},"ScriptSpec":{"type":"object","properties":{"onChange":{"default":[],"type":"array","items":{"$ref":"#/definitions/ScriptEntry"}},"onDrift":{"default":[],"type":"array","items":{"$ref":"#/definitions/ScriptEntry"}},"postApply":{"default":[],"type":"array","items":{"$ref":"#/definitions/ScriptEntry"}},"postReconcile":{"default":[],"type":"array","items":{"$ref":"#/definitions/ScriptEntry"}},"preApply":{"default":[],"type":"array","items":{"$ref":"#/definitions/ScriptEntry"}},"preReconcile":{"default":[],"type":"array","items":{"$ref":"#/definitions/ScriptEntry"}}},"additionalProperties":false},"SecretSpec":{"type":"object","required":["source"],"properties":{"backend":{"type":["string","null"]},"envs":{"type":["array","null"],"items":{"type":"string"}},"source":{"type":"string"},"target":{"type":["string","null"]},"template":{"type":["string","null"]}},"additionalProperties":false},"ShellAlias":{"type":"object","required":["command","name"],"properties":{"command":{"type":"string"},"name":{"type":"string"}}},"SnapSpec":{"type":"object","properties":{"classic":{"default":[],"type":"array","items":{"type":"string"}},"packages":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false}}}
```

