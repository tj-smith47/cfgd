<!-- cfgd:skill:profile -->
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
{"$schema":"https://json-schema.org/draft-07/schema#","additionalProperties":false,"definitions":{"AptSpec":{"additionalProperties":false,"properties":{"file":{"default":null,"type":["string","null"]},"packages":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"BrewSpec":{"additionalProperties":false,"properties":{"casks":{"default":[],"items":{"type":"string"},"type":"array"},"file":{"default":null,"type":["string","null"]},"formulae":{"default":[],"items":{"type":"string"},"type":"array"},"taps":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"CargoSpec":{"additionalProperties":false,"description":"Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`) and object form (`cargo: { file: Cargo.toml, packages: [...] }`) via the shared `list_or_struct` deserializer on the `PackagesSpec::cargo` field.","properties":{"file":{"type":["string","null"]},"packages":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"CustomManagerSpec":{"additionalProperties":false,"properties":{"check":{"type":"string"},"install":{"type":"string"},"listInstalled":{"type":"string"},"name":{"type":"string"},"packages":{"default":[],"items":{"type":"string"},"type":"array"},"uninstall":{"type":"string"},"update":{"default":null,"type":["string","null"]}},"required":["name","check","listInstalled","install","uninstall"],"type":"object"},"EncryptionMode":{"description":"Controls when encryption is required for a managed file.","oneOf":[{"const":"InRepo","description":"File must be encrypted when stored in the repository.","type":"string"},{"const":"Always","description":"File must always be encrypted, including at rest on disk.","type":"string"}]},"EncryptionSpec":{"additionalProperties":false,"description":"Encryption settings for a managed file.","properties":{"backend":{"description":"The encryption backend to use (e.g. \"sops\", \"age\").","type":"string"},"mode":{"$ref":"#/definitions/EncryptionMode","default":"InRepo","description":"When encryption must be enforced. Defaults to `InRepo`."}},"required":["backend"],"type":"object"},"EnvScope":{"description":"How far `spec.env` exports reach across the current user's environment. The two env fields differ by *scope of affected users*: `spec.env` targets the current user, `spec.system.environment` targets all users (privileged). This knob narrows the *current-user* reach; it never widens beyond the user.","oneOf":[{"const":"All","description":"Every standard user entry point cfgd can safely touch: interactive + login shells, `systemd --user` / Wayland GUI sessions, macOS GUI apps, and an immediate live-session refresh. The default — no gotchas.","type":"string"},{"const":"Login","description":"Interactive shells plus login shells (`~/.zshenv`, `~/.profile`, and an existing `~/.bash_profile`). Excludes the GUI / `systemd --user` session surfaces and the live-session refresh.","type":"string"},{"const":"Interactive","description":"Interactive shells only (`~/.bashrc` / `~/.zshrc`, fish conf.d) — the historical behavior before full reach.","type":"string"}]},"EnvVar":{"properties":{"name":{"type":"string"},"value":{"type":"string"}},"required":["name","value"],"type":"object"},"FileStrategy":{"description":"File deployment strategy.","oneOf":[{"const":"Symlink","description":"Create a symbolic link from target to source (default).","type":"string"},{"const":"Copy","description":"Copy source content to target.","type":"string"},{"const":"Template","description":"Render a Tera template and write the output (auto-selected for .tera files).","type":"string"},{"const":"Hardlink","description":"Create a hard link from target to source.","type":"string"}]},"FilesSpec":{"additionalProperties":false,"properties":{"managed":{"default":[],"items":{"$ref":"#/definitions/ManagedFileSpec"},"type":"array"},"permissions":{"additionalProperties":{"type":"string"},"default":{},"type":"object"}},"type":"object"},"FlatpakSpec":{"additionalProperties":false,"properties":{"packages":{"default":[],"items":{"type":"string"},"type":"array"},"remote":{"default":null,"type":["string","null"]}},"type":"object"},"ManagedFileSpec":{"additionalProperties":false,"properties":{"encryption":{"anyOf":[{"$ref":"#/definitions/EncryptionSpec"},{"type":"null"}],"description":"Encryption settings for this file."},"permissions":{"description":"Unix permission bits (e.g. \"600\", \"644\") to apply after deployment.","type":["string","null"]},"private":{"description":"When true, the source file is local-only: auto-added to .gitignore, silently skipped on machines where it doesn't exist.","type":"boolean"},"source":{"type":"string"},"strategy":{"anyOf":[{"$ref":"#/definitions/FileStrategy"},{"type":"null"}],"description":"Per-file deployment strategy override. If None, uses the global default."},"target":{"type":"string"}},"required":["source","target"],"type":"object"},"NpmSpec":{"additionalProperties":false,"properties":{"file":{"default":null,"type":["string","null"]},"global":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"PackagesSpec":{"additionalProperties":false,"properties":{"apk":{"default":[],"items":{"type":"string"},"type":"array"},"apt":{"anyOf":[{"$ref":"#/definitions/AptSpec"},{"type":"null"}],"default":null},"brew":{"anyOf":[{"$ref":"#/definitions/BrewSpec"},{"type":"null"}],"default":null},"cargo":{"anyOf":[{"$ref":"#/definitions/CargoSpec"},{"type":"null"}],"default":null},"chocolatey":{"default":[],"items":{"type":"string"},"type":"array"},"custom":{"default":[],"items":{"$ref":"#/definitions/CustomManagerSpec"},"type":"array"},"dnf":{"default":[],"items":{"type":"string"},"type":"array"},"flatpak":{"anyOf":[{"$ref":"#/definitions/FlatpakSpec"},{"type":"null"}],"default":null},"go":{"default":[],"items":{"type":"string"},"type":"array"},"nix":{"default":[],"items":{"type":"string"},"type":"array"},"npm":{"anyOf":[{"$ref":"#/definitions/NpmSpec"},{"type":"null"}],"default":null},"pacman":{"default":[],"items":{"type":"string"},"type":"array"},"pipx":{"default":[],"items":{"type":"string"},"type":"array"},"pkg":{"default":[],"items":{"type":"string"},"type":"array"},"scoop":{"default":[],"items":{"type":"string"},"type":"array"},"snap":{"anyOf":[{"$ref":"#/definitions/SnapSpec"},{"type":"null"}],"default":null},"winget":{"default":[],"items":{"type":"string"},"type":"array"},"yum":{"default":[],"items":{"type":"string"},"type":"array"},"zypper":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"ScriptEntry":{"anyOf":[{"type":"string"},{"properties":{"continueOnError":{"type":["boolean","null"]},"creates":{"description":"Skip the script if this path already exists. A leading `~` expands to the home directory; a relative path resolves against the script's working directory. Existence follows symlinks.","type":["string","null"]},"idleTimeout":{"description":"Kill the script if it produces no stdout/stderr output for this duration. Prevents scripts from silently hanging on unresponsive resources. Format: \"30s\", \"2m\", etc. If unset, no idle timeout is enforced.","type":["string","null"]},"interactive":{"description":"Run the script attached to the terminal (inherited stdin/stdout/stderr, no spinner, no output capture, no idle timeout) so it can prompt the user — e.g. `echo \"press Enter when done\"; read`. Requires a TTY: when stdin is not a terminal (CI, piped input, or any daemon-run phase) the script is skipped with a warning rather than hanging on instant EOF.","type":"boolean"},"onlyIf":{"description":"Run the script only if this command exits zero. A non-zero exit skips the script (the condition for running was not met). Evaluated with the same shell, working directory, and environment as the body.","type":["string","null"]},"run":{"type":"string"},"shell":{"$ref":"#/definitions/ScriptShell","description":"Interpreter to use for inline commands. Ignored (and rejected) on file scripts."},"timeout":{"type":["string","null"]},"unless":{"description":"Run the script only if this command exits NON-zero. A zero exit (success) skips the script (the guarded state already holds). Evaluated with the same shell, working directory, and environment as the body.","type":["string","null"]},"workdir":{"description":"Working directory for the script. By default every lifecycle script runs in the user's home directory — never the config source tree — so a relative write can't pollute the user's GitOps repo. Set `workdir` to override: a leading `~` expands to home and `$VAR`/`${VAR}` expand against the script environment (which always carries `$CFGD_MODULE_DIR` and `$CFGD_CONFIG_DIR`), so `workdir: ~/.local/share/app`, `workdir: $CFGD_MODULE_DIR`, or an absolute path all work.","type":["string","null"]}},"required":["run"],"type":"object"}]},"ScriptShell":{"description":"Interpreter for inline lifecycle scripts.","oneOf":[{"enum":["sh","bash","zsh","pwsh","cmd"],"type":"string"},{"const":"auto","description":"Platform default: `sh` on Unix, `cmd.exe` on Windows.","type":"string"}]},"ScriptSpec":{"additionalProperties":false,"properties":{"onChange":{"default":[],"items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"onDrift":{"default":[],"items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"postApply":{"default":[],"items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"postReconcile":{"default":[],"items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"preApply":{"default":[],"items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"},"preReconcile":{"default":[],"items":{"$ref":"#/definitions/ScriptEntry"},"type":"array"}},"type":"object"},"SecretSpec":{"additionalProperties":false,"properties":{"backend":{"type":["string","null"]},"envs":{"items":{"type":"string"},"type":["array","null"]},"source":{"type":"string"},"target":{"type":["string","null"]},"template":{"type":["string","null"]}},"required":["source"],"type":"object"},"ShellAlias":{"properties":{"command":{"type":"string"},"name":{"type":"string"}},"required":["name","command"],"type":"object"},"SnapSpec":{"additionalProperties":false,"properties":{"classic":{"default":[],"items":{"type":"string"},"type":"array"},"packages":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"}},"properties":{"aliases":{"default":[],"items":{"$ref":"#/definitions/ShellAlias"},"type":"array"},"env":{"default":[],"items":{"$ref":"#/definitions/EnvVar"},"type":"array"},"envScope":{"anyOf":[{"$ref":"#/definitions/EnvScope"},{"type":"null"}],"description":"How far `spec.env` exports reach across the current user's environment. Omitted means \"inherit\" (a parent layer's value survives); the resolved default when no layer sets it is [`EnvScope::All`] — every standard user entry point cfgd can safely touch. Narrow it to `Login` or `Interactive` to opt out of the broader session surfaces."},"files":{"anyOf":[{"$ref":"#/definitions/FilesSpec"},{"type":"null"}],"default":null},"inherits":{"default":[],"items":{"type":"string"},"type":"array"},"modules":{"default":[],"items":{"type":"string"},"type":"array"},"packages":{"anyOf":[{"$ref":"#/definitions/PackagesSpec"},{"type":"null"}],"default":null},"scripts":{"anyOf":[{"$ref":"#/definitions/ScriptSpec"},{"type":"null"}],"default":null},"secrets":{"default":[],"items":{"$ref":"#/definitions/SecretSpec"},"type":"array"},"system":{"additionalProperties":true,"default":{},"type":"object"}},"title":"ProfileSpec","type":"object"}
```


<!-- /cfgd:skill:profile -->
