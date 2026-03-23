use super::SchemaKind;

pub fn get_schema(kind: SchemaKind) -> &'static str {
    match kind {
        SchemaKind::Module => MODULE_SCHEMA,
        SchemaKind::Profile => PROFILE_SCHEMA,
        SchemaKind::Config => CONFIG_SCHEMA,
    }
}

const MODULE_SCHEMA: &str = r#"# cfgd Module schema — cfgd.io/v1alpha1
# A Module is a self-contained, portable configuration package.
# It bundles packages, config files, env vars, aliases, and scripts for one tool.

apiVersion: cfgd.io/v1alpha1  # required, always this value
kind: Module                   # required, always "Module"

metadata:
  name: example                # required, string — unique module name (used in profile modules list)
  description: Example module  # optional, string — human-readable description

spec:
  # List of module names this module depends on.
  # Dependencies are installed before this module. Circular deps are rejected.
  # optional, default: []
  depends:
    - node
    - python

  # Cross-platform package declarations.
  # Each entry describes one logical package with platform-aware resolution.
  # optional, default: []
  packages:
    - name: neovim             # required, string — canonical package name
      minVersion: "0.9"         # optional, string — minimum semver version (e.g. "0.9", "18.0.0")
      # optional, list of strings — ordered list of package managers to try.
      # Values: brew, apt, dnf, yum, pacman, apk, zypper, pkg, snap, flatpak, nix, go, cargo, npm, pipx, script.
      # If "script" is in the list, the script field must be present.
      # If omitted, uses the platform's native manager.
      prefer:
        - brew
        - snap
        - apt
      # optional, map of string->string — per-manager package name overrides.
      # Use when the package has a different name in a specific manager's repository.
      aliases:
        apt: neovim-nightly
        snap: nvim
      # optional, string — inline shell script or path (relative to module dir).
      # Used when prefer includes "script". Runs with /bin/sh -e.
      script: curl -fsSL https://example.com/install.sh | sh
      # optional, list of strings — package managers to never use for this package.
      deny:
        - snap
      # optional, list of strings — platform filter.
      # Values: OS (linux, macos), distro (ubuntu, fedora, arch, debian, alpine, opensuse, freebsd),
      # or arch (x86_64, aarch64). Package is skipped on non-matching platforms.
      platforms:
        - linux
        - macos

  # File entries to deploy. Source can be local (relative to module dir) or a git URL.
  # optional, default: []
  files:
    - source: config/           # required, string — local path relative to module dir
      target: ~/.config/nvim/   # required, string — absolute target path (~ is expanded)
      # optional, string — deployment strategy override: Symlink, Copy, Template, Hardlink.
      # If omitted, uses the global fileStrategy from Config (default: Symlink).
      strategy: Symlink
      # optional, bool, default: false — when true, file is local-only (added to .gitignore,
      # silently skipped on machines where it doesn't exist).
      private: false

    # Git URL source — clone/fetch from a remote repo
    - source: https://github.com/user/nvim-config.git@v2.1.0  # @tag or ?ref=branch, //subdir
      target: ~/.config/nvim/

  # Environment variables set by this module.
  # Merged with profile env vars; module wins on name conflict.
  # optional, default: []
  env:
    - name: EDITOR              # required, string — variable name
      value: nvim               # required, string — variable value

  # Shell aliases set by this module.
  # Merged with profile aliases; module wins on name conflict.
  # For bash/zsh: alias name="command". For fish: abbr -a name command.
  # optional, default: []
  aliases:
    - name: vim                 # required, string — alias name
      command: nvim             # required, string — alias command

  # Lifecycle scripts. Run after all packages and files are deployed.
  # optional
  scripts:
    # List of shell commands to run after apply. Each runs with /bin/sh -e
    # in the module directory. If one fails, subsequent scripts are skipped.
    # optional, default: []
    postApply:
      - nvim --headless "+Lazy! sync" +qa
      - nvim --headless -c "MasonInstallAll" -c "qa"
"#;

const PROFILE_SCHEMA: &str = r#"# cfgd Profile schema — cfgd.io/v1alpha1
# A Profile declares the desired state of a machine: packages, files, env, system settings.
# Profiles can inherit from other profiles to share common configuration.

apiVersion: cfgd.io/v1alpha1  # required, always this value
kind: Profile                  # required, always "Profile"

metadata:
  name: work                   # required, string — profile name (filename without .yaml)

spec:
  # Parent profiles to inherit from, processed left-to-right.
  # The active profile is applied last and wins on conflicts.
  # Merge rules: env/aliases=override by name, packages=union, files=overlay by target,
  # system=deep merge, secrets=append (dedup by target), scripts=append, modules=union.
  # optional, default: []
  inherits:
    - base
    - macos

  # Module names to include. Modules provide portable, cross-platform tool configs.
  # Use "<registry>/<module>" for registry modules (e.g. "community/tmux").
  # optional, default: []
  modules:
    - nvim
    - tmux
    - community/tmux

  # Environment variables — name/value pairs exported to shell and available in Tera templates.
  # Later profile overrides earlier for the same name. Module env wins over profile env.
  # optional, default: []
  env:
    - name: EDITOR              # required, string — variable name
      value: nvim               # required, string — variable value
    - name: GIT_AUTHOR_NAME
      value: Jane Doe

  # Shell aliases — name/command pairs written to ~/.cfgd.env (bash/zsh) or fish conf.d.
  # Later profile overrides earlier for the same name. Module aliases win over profile aliases.
  # optional, default: []
  aliases:
    - name: vim                 # required, string — alias name
      command: nvim             # required, string — alias command
    - name: k
      command: kubectl

  # Per-manager package declarations.
  # optional
  packages:
    # Homebrew (macOS, Linux)
    brew:
      file: Brewfile            # optional, string — path to Brewfile
      taps:                     # optional, list of strings — custom taps to add
        - homebrew/cask-fonts
      formulae:                 # optional, list of strings — formulae to install
        - git
        - ripgrep
        - jq
      casks:                    # optional, list of strings — casks to install
        - 1password
        - wezterm

    # APT (Debian, Ubuntu)
    apt:
      file: packages.txt        # optional, string — path to package list file
      packages:                  # optional, list of strings
        - build-essential
        - curl

    # Cargo (Rust) — accepts list form or object form
    # List form: cargo: [bat, eza]
    # Object form shown below:
    cargo:
      file: Cargo.toml          # optional, string — path to Cargo.toml with [dependencies]
      packages:                  # optional, list of strings
        - bat
        - eza

    # npm (Node.js)
    npm:
      file: package.json         # optional, string — path to package.json
      global:                    # optional, list of strings — globally installed packages
        - typescript
        - prettier

    # pipx (Python)
    # list of strings
    pipx:
      - httpie
      - ruff

    # DNF (Fedora, RHEL 8+)
    # list of strings
    dnf:
      - gcc
      - make

    # APK (Alpine)
    # list of strings
    apk:
      - build-base

    # Pacman (Arch, Manjaro)
    # list of strings
    pacman:
      - base-devel

    # Zypper (openSUSE)
    # list of strings
    zypper:
      - gcc

    # Yum (RHEL 7, CentOS 7)
    # list of strings
    yum:
      - gcc

    # pkg (FreeBSD)
    # list of strings
    pkg:
      - curl

    # Snap (Ubuntu, other Linux)
    snap:
      packages:                  # optional, list of strings — strict confinement snaps
        - firefox
      classic:                   # optional, list of strings — classic confinement snaps
        - code

    # Flatpak
    flatpak:
      packages:                  # optional, list of strings — Flatpak app IDs
        - org.mozilla.firefox
      remote: flathub            # optional, string — remote name (default: flathub)

    # Nix
    # list of strings — Nix package names
    nix:
      - direnv

    # Go
    # list of strings — Go package paths (go install)
    go:
      - golang.org/x/tools/gopls@latest

    # winget (Windows Package Manager)
    # list of strings — winget package IDs
    winget:
      - Microsoft.VisualStudioCode
      - Git.Git

    # Chocolatey (Windows)
    # list of strings — Chocolatey package names
    chocolatey:
      - nodejs
      - 7zip

    # Scoop (Windows)
    # list of strings — Scoop package names
    scoop:
      - ripgrep
      - fd

    # Custom package managers
    # list of objects, each defining how to interact with a custom manager
    custom:
      - name: my-manager        # required, string — manager name
        check: which my-mgr     # required, string — command to check if manager exists
        listInstalled: my-mgr list   # required, string — command to list installed packages
        install: my-mgr install      # required, string — install command template
        uninstall: my-mgr remove     # required, string — uninstall command template
        update: my-mgr upgrade       # optional, string — update command template
        packages:                     # optional, list of strings
          - custom-pkg

  # File management — source files deployed to target locations.
  # optional
  files:
    # List of managed file entries.
    # optional, default: []
    managed:
      - source: shell/.zshrc       # required, string — path relative to config repo files/ dir
        target: ~/.zshrc            # required, string — absolute target path (~ expanded)
        strategy: Symlink           # optional: Symlink (default), Copy, Template, Hardlink
        private: false              # optional, bool, default: false — local-only file
      - source: git/.gitconfig.tera # .tera extension auto-selects template strategy
        target: ~/.gitconfig

    # Permission overrides — map of target path to octal permission string.
    # optional, default: {}
    permissions:
      "~/.ssh/config": "600"
      "~/.ssh": "700"

  # System configurator settings — a map of configurator name to its config value.
  # Available configurators depend on the platform. Unknown keys are silently skipped.
  # optional, default: {}
  system:
    # Set default login shell (all platforms)
    shell: /bin/zsh               # string — path to shell binary

    # macOS defaults domains (macOS only)
    macosDefaults:
      NSGlobalDomain:
        AppleShowAllExtensions: true
      com.apple.dock:
        autohide: true
        tilesize: 48

    # LaunchAgent plist management (macOS only)
    launchAgents:
      - name: com.example.myservice    # required, string — service label
        program: /usr/local/bin/myservice  # required, string — executable path
        args:                              # optional, list of strings
          - "--config"
          - "/etc/myservice.conf"
        runAtLoad: true                    # optional, bool

    # systemd user unit management (Linux only)
    systemdUnits:
      - name: myservice.service    # required, string — unit name
        unitFile: systemd/myservice.service  # required, string — path to unit file
        enabled: true              # optional, bool

    # Shell environment file management (all platforms)
    environment:
      GOPATH: ~/go
      EDITOR: nvim

    # Kernel sysctl parameters (Linux, typically node/k8s context)
    sysctl:
      net.ipv4.ip_forward: 1
      vm.max_map_count: 262144

    # Kernel modules to load (Linux, typically node/k8s context)
    kernelModules:
      - br_netfilter
      - overlay

    # containerd configuration (Linux, node/k8s context)
    containerd:
      configPath: /etc/containerd/config.toml

    # kubelet configuration (Linux, node/k8s context)
    kubelet:
      configPath: /var/lib/kubelet/config.yaml

    # AppArmor profiles (Linux, node/k8s context)
    apparmor:
      profiles:
        - /etc/apparmor.d/my-profile

    # Seccomp profiles (Linux, node/k8s context)
    seccomp:
      profilesDir: /var/lib/kubelet/seccomp

    # Certificate management (Linux, node/k8s context)
    certificates:
      caCerts:
        - /usr/local/share/ca-certificates/my-ca.crt

    # Windows registry settings (Windows only)
    # Keys are registry paths (e.g. HKCU\Software\...), values are name-value maps
    windowsRegistry:
      HKCU\Software\Example:
        MyValue: hello
        MyDword: 42

    # Windows Service management (Windows only)
    windowsServices:
      - name: MyService              # required, string — service name
        displayName: My Service      # optional, string — display name
        binaryPath: C:\svc\svc.exe  # required (for create), string — executable path
        startType: auto              # optional: auto, manual, disabled (default: auto)
        state: running               # optional: running, stopped (default: running)

  # Secret file declarations — SOPS-encrypted files or external secret provider references.
  # optional, default: []
  secrets:
    - source: secrets/api-keys.yaml        # required, string — SOPS file path or provider URI
      target: ~/.config/api-keys.yaml      # required, string — target path for decrypted output
      template: "token: ${secret:value}"   # optional, string — template for formatting output
      backend: sops                        # optional, string — override backend (sops, 1password, bitwarden, vault)
    - source: 1password://Work/GitHub/token
      target: ~/.config/gh/token

  # Lifecycle scripts — hook into apply, reconcile, drift, and change events.
  # Each hook accepts a list of script entries. Entries can be simple strings
  # (command to run) or objects with run, timeout, and continueOnError fields.
  # optional
  scripts:
    # Scripts run before apply. Paths relative to config repo or inline commands.
    # optional, default: []
    preApply:
      - scripts/pre-setup.sh
    # Scripts run after apply.
    # optional, default: []
    postApply:
      - run: scripts/post-setup.sh
        timeout: 60s
        continueOnError: false
    # Scripts run before reconcile.
    # optional, default: []
    preReconcile:
      - scripts/reconcile-pre.sh
    # Scripts run after reconcile.
    # optional, default: []
    postReconcile:
      - scripts/reconcile-post.sh
    # Scripts run when drift is detected.
    # optional, default: []
    onDrift:
      - scripts/on-drift.sh
    # Scripts run when configuration changes.
    # optional, default: []
    onChange:
      - scripts/on-change.sh
"#;

const CONFIG_SCHEMA: &str = r#"# cfgd Config schema — cfgd.io/v1alpha1
# The root configuration file (cfgd.yaml). Entry point for cfgd.
# Tells cfgd which profile to activate, where config is stored, and how the daemon behaves.

apiVersion: cfgd.io/v1alpha1  # required, always this value
kind: Config                   # required, always "Config"

metadata:
  name: my-workstation         # required, string — config name (typically machine name)

spec:
  # Active profile name — the profile YAML file to activate (without .yaml extension).
  # required, string
  profile: work

  # Git origin(s) for the config repository. Can be a single object or a list.
  # optional, default: []
  origin:
    type: Git                  # required: "Git" or "Server"
    url: git@github.com:me/machine-config.git  # required, string — repository URL
    branch: master             # optional, string, default: "master"
    auth: ssh                  # optional, string — auth method hint

  # Daemon configuration — controls the background reconciliation loop.
  # optional
  daemon:
    enabled: true              # optional, bool, default: false

    # Reconciliation settings
    reconcile:
      # How often to check for drift (e.g. "1m", "5m", "1h").
      # optional, string, default: "5m"
      interval: 5m
      # Reconcile immediately when config files change (via file watcher).
      # optional, bool, default: false
      onChange: true
      # Auto-apply detected changes without prompting.
      # optional, bool, default: false
      autoApply: false
      # Drift reconciliation policy for daemon auto-reconciliation.
      # Values: Auto (silently apply), NotifyOnly (notify but don't apply, default), Prompt (future).
      # optional, default: NotifyOnly
      driftPolicy: NotifyOnly
      # Auto-apply policy — controls behavior for different change categories.
      # optional
      policy:
        newRecommended: Notify     # optional: Notify (default), Accept, Reject, Ignore
        newOptional: Ignore        # optional: Notify, Accept, Reject, Ignore (default)
        lockedConflict: Notify     # optional: Notify (default), Accept, Reject, Ignore
      # Per-module or per-profile reconcile overrides (kustomize-style patches).
      # Each patch targets a specific Module or Profile by name and overrides
      # individual reconcile fields. Precedence: Module patch > Profile patch > global.
      # optional, default: []
      patches:
        - kind: Module             # required: "Module" or "Profile"
          name: nvim               # optional, string — target name (omit to apply to all of this kind)
          interval: 1m             # optional, string — override interval
          autoApply: true          # optional, bool — override auto-apply
          driftPolicy: Auto        # optional: Auto, NotifyOnly, Prompt

    # Git sync settings
    sync:
      autoPull: true           # optional, bool, default: false — auto-pull from remote
      autoPush: false          # optional, bool, default: false — auto-commit and push local changes
      interval: 5m             # optional, string, default: "1h" — sync interval

    # Notification settings
    notify:
      drift: true              # optional, bool, default: false — notify on drift detection
      method: Desktop          # optional: Desktop (default), Stdout, Webhook
      webhookUrl: https://hooks.example.com/cfgd   # optional, string — webhook URL (when method=webhook)

  # Secrets backend configuration.
  # optional
  secrets:
    backend: sops              # optional, string, default: "sops" — primary backend
    sops:
      ageKey: ~/.config/cfgd/age-key.txt   # optional, string — path to age key file
    # External secret provider integrations.
    # optional, default: []
    integrations:
      - name: 1password        # required, string — provider name (1password, bitwarden, vault)
      - name: bitwarden
      - name: vault

  # Multi-source config management — subscribe to team/org config sources.
  # optional, default: []
  sources:
    - name: acme-corp          # required, string — source name
      origin:
        type: Git              # required: "Git" or "Server"
        url: git@github.com:acme-corp/dev-config.git  # required, string
        branch: master         # optional, string, default: "master"
      subscription:
        profile: acme-backend  # optional, string — specific profile from source to subscribe to
        priority: 500          # optional, u32, default: 500 — merge priority (higher wins)
        acceptRecommended: true   # optional, bool, default: false — auto-accept recommended items
        optIn:                 # optional, list of strings — explicitly opt into optional items
          - extra-tools
        overrides: {}          # optional, YAML value — local overrides applied on top of source
        reject: {}             # optional, YAML value — items to reject from source
      sync:
        interval: 1h           # optional, string, default: "1h"
        autoApply: false       # optional, bool, default: false
        pinVersion: v1.2.3     # optional, string — pin to a specific source version

  # Theme configuration — controls terminal output styling.
  # Can be a string (theme name) or an object with name and overrides.
  # optional, default: "default"
  # String form: theme: dracula
  # Object form:
  theme:
    name: default              # optional, string, default: "default"
    overrides:                 # optional — override individual theme colors/icons
      success: green           # optional, string — color name
      warning: yellow          # optional, string
      error: red               # optional, string
      info: blue               # optional, string
      muted: gray              # optional, string
      header: cyan             # optional, string
      subheader: white         # optional, string
      key: cyan                # optional, string
      value: white             # optional, string
      diffAdd: green           # optional, string
      diffRemove: red          # optional, string
      diffContext: gray        # optional, string
      iconSuccess: "✓"         # optional, string — icon character
      iconWarning: "⚠"         # optional, string
      iconError: "✗"           # optional, string
      iconInfo: "ℹ"            # optional, string
      iconPending: "○"         # optional, string
      iconArrow: "→"           # optional, string

  # Module registries and security settings.
  # optional
  modules:
    # Git repos containing reusable modules in modules/<name>/module.yaml structure.
    # optional, default: []
    registries:
      - name: community        # required, string — short name / alias
        url: https://github.com/cfgd-community/modules.git  # required, string — git URL
    # Module security settings.
    # optional
    security:
      # Require GPG/SSH signatures on all remote module tags.
      # When true, unsigned modules are rejected unless --allow-unsigned is passed.
      # optional, bool, default: false
      requireSignatures: false

  # Global default file deployment strategy. Per-file overrides take precedence.
  # optional, default: Symlink
  # Values: Symlink, Copy, Template, Hardlink
  fileStrategy: Symlink

  # Security settings for source signature verification.
  # optional
  security:
    # Allow unsigned source content even when the source requires signed commits.
    # Intended for development/testing environments.
    # optional, bool, default: false
    allowUnsigned: false

  # CLI command aliases — map of alias name to command string.
  # Built-in defaults (add, remove) can be overridden or extended.
  # optional, default: {}
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
    up: "apply --yes"
    s: status
    pkg: "profile update --package"

  # AI assistant configuration for cfgd generate.
  # optional
  ai:
    # AI provider to use.
    # optional, string, default: "claude"
    provider: claude
    # Model name/ID.
    # optional, string, default: "claude-sonnet-4-6"
    model: claude-sonnet-4-6
    # Environment variable name containing the API key.
    # optional, string, default: "ANTHROPIC_API_KEY"
    apiKeyEnv: ANTHROPIC_API_KEY
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_schema_not_empty() {
        let schema = get_schema(SchemaKind::Module);
        assert!(!schema.is_empty());
        assert!(schema.contains("apiVersion"));
        assert!(schema.contains("kind: Module"));
        assert!(schema.contains("packages"));
        assert!(schema.contains("files"));
        assert!(schema.contains("depends"));
        assert!(schema.contains("env"));
        assert!(schema.contains("aliases"));
    }

    #[test]
    fn test_profile_schema_not_empty() {
        let schema = get_schema(SchemaKind::Profile);
        assert!(!schema.is_empty());
        assert!(schema.contains("apiVersion"));
        assert!(schema.contains("kind: Profile"));
        assert!(schema.contains("inherits"));
        assert!(schema.contains("modules"));
        assert!(schema.contains("packages"));
        assert!(schema.contains("system"));
    }

    #[test]
    fn test_config_schema_not_empty() {
        let schema = get_schema(SchemaKind::Config);
        assert!(!schema.is_empty());
        assert!(schema.contains("apiVersion"));
        assert!(schema.contains("kind: Config"));
        assert!(schema.contains("ai"));
    }

    #[test]
    fn test_module_schema_is_valid_yaml() {
        let schema = get_schema(SchemaKind::Module);
        let _: serde_yaml::Value =
            serde_yaml::from_str(schema).expect("Module schema must be valid YAML");
    }

    #[test]
    fn test_profile_schema_is_valid_yaml() {
        let schema = get_schema(SchemaKind::Profile);
        let _: serde_yaml::Value =
            serde_yaml::from_str(schema).expect("Profile schema must be valid YAML");
    }

    #[test]
    fn test_config_schema_is_valid_yaml() {
        let schema = get_schema(SchemaKind::Config);
        let _: serde_yaml::Value =
            serde_yaml::from_str(schema).expect("Config schema must be valid YAML");
    }

    #[test]
    fn test_module_schema_covers_all_spec_fields() {
        let schema = get_schema(SchemaKind::Module);
        // ModuleSpec fields
        assert!(schema.contains("depends"));
        assert!(schema.contains("packages"));
        assert!(schema.contains("files"));
        assert!(schema.contains("env"));
        assert!(schema.contains("aliases"));
        assert!(schema.contains("scripts"));
        // ModulePackageEntry fields
        assert!(schema.contains("minVersion"));
        assert!(schema.contains("prefer"));
        assert!(schema.contains("script"));
        assert!(schema.contains("deny"));
        assert!(schema.contains("platforms"));
        // ModuleFileEntry fields
        assert!(schema.contains("source"));
        assert!(schema.contains("target"));
        assert!(schema.contains("strategy"));
        assert!(schema.contains("private"));
        // ScriptSpec (unified — all hooks except onDrift for modules)
        assert!(schema.contains("postApply"));
    }

    #[test]
    fn test_profile_schema_covers_all_spec_fields() {
        let schema = get_schema(SchemaKind::Profile);
        // ProfileSpec fields
        assert!(schema.contains("inherits"));
        assert!(schema.contains("modules"));
        assert!(schema.contains("env"));
        assert!(schema.contains("aliases"));
        assert!(schema.contains("packages"));
        assert!(schema.contains("files"));
        assert!(schema.contains("system"));
        assert!(schema.contains("secrets"));
        assert!(schema.contains("scripts"));
        // PackagesSpec managers
        assert!(schema.contains("brew"));
        assert!(schema.contains("apt"));
        assert!(schema.contains("cargo"));
        assert!(schema.contains("npm"));
        assert!(schema.contains("pipx"));
        assert!(schema.contains("dnf"));
        assert!(schema.contains("apk"));
        assert!(schema.contains("pacman"));
        assert!(schema.contains("zypper"));
        assert!(schema.contains("yum"));
        assert!(schema.contains("pkg"));
        assert!(schema.contains("snap"));
        assert!(schema.contains("flatpak"));
        assert!(schema.contains("nix"));
        assert!(schema.contains("go"));
        assert!(schema.contains("winget"));
        assert!(schema.contains("chocolatey"));
        assert!(schema.contains("scoop"));
        assert!(schema.contains("custom"));
        // Windows system configurators
        assert!(schema.contains("windowsRegistry"));
        assert!(schema.contains("windowsServices"));
        // FilesSpec fields
        assert!(schema.contains("managed"));
        assert!(schema.contains("permissions"));
        // ScriptSpec (all 6 hook types)
        assert!(schema.contains("preApply"));
        assert!(schema.contains("postApply"));
        assert!(schema.contains("preReconcile"));
        assert!(schema.contains("postReconcile"));
        assert!(schema.contains("onDrift"));
        assert!(schema.contains("onChange"));
    }

    #[test]
    fn test_config_schema_covers_all_spec_fields() {
        let schema = get_schema(SchemaKind::Config);
        // ConfigSpec fields
        assert!(schema.contains("profile"));
        assert!(schema.contains("origin"));
        assert!(schema.contains("daemon"));
        assert!(schema.contains("secrets"));
        assert!(schema.contains("sources"));
        assert!(schema.contains("theme"));
        assert!(schema.contains("modules"));
        assert!(schema.contains("fileStrategy"));
        assert!(schema.contains("security"));
        assert!(schema.contains("aliases"));
        assert!(schema.contains("ai"));
        // DaemonConfig sub-fields
        assert!(schema.contains("reconcile"));
        assert!(schema.contains("sync"));
        assert!(schema.contains("notify"));
        // ReconcileConfig
        assert!(schema.contains("interval"));
        assert!(schema.contains("onChange"));
        assert!(schema.contains("autoApply"));
        assert!(schema.contains("driftPolicy"));
        assert!(schema.contains("patches"));
        // AiConfig
        assert!(schema.contains("provider"));
        assert!(schema.contains("model"));
        assert!(schema.contains("apiKeyEnv"));
    }
}
