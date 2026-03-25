# System Configurators

The `system:` section in profiles routes each key to a registered system configurator. Available configurators depend on the OS and context. Configurators that aren't available on the current platform are silently skipped.

Each configurator follows the same pattern: read what the system has now, compare against what you want, and apply the difference.

## Workstation Configurators

### `shell`

Sets the default login shell via `chsh` (macOS/Linux). Value is the path to the shell binary.

```yaml
system:
  shell: /bin/zsh
```

On Windows, `shell` sets the Windows Terminal default profile by writing to the Windows Terminal `settings.json`. Use the profile name as it appears in Windows Terminal settings.

```yaml
system:
  shell: PowerShell
```

### `macosDefaults` (macOS only)

Reads and writes macOS [`defaults`](https://macos-defaults.com/) domains. Each key is a domain name, values are key-value pairs to set.

```yaml
system:
  macosDefaults:
    NSGlobalDomain:
      AppleShowAllExtensions: true
      NSAutomaticSpellingCorrectionEnabled: false
    com.apple.dock:
      autohide: true
      tilesize: 48
    com.apple.screensaver:
      askForPassword: 1
      askForPasswordDelay: 0
```

### `launchAgents` (macOS only)

Manages [LaunchAgent](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html) plist files in `~/Library/LaunchAgents` — macOS's way of running background services for your user session.

```yaml
system:
  launchAgents:
    - name: com.example.myservice
      program: /usr/local/bin/myservice
      args: ["--config", "/etc/myservice.conf"]
      runAtLoad: true
```

### `systemdUnits` (Linux only)

Manages [systemd](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html) user unit files — Linux's service manager. Handles unit file placement, enabling, and starting.

```yaml
system:
  systemdUnits:
    - name: myservice.service
      unitFile: systemd/myservice.service
      enabled: true
```

### `gsettings` (Linux only)

Reads and writes [GNOME/GTK desktop settings](https://docs.gtk.org/gio/class.Settings.html) via the `gsettings` CLI. Covers GNOME, Cinnamon, MATE, Budgie, and Pantheon desktops. Each key is a gsettings schema name, values are key-value pairs to set.

```yaml
system:
  gsettings:
    org.gnome.desktop.interface:
      color-scheme: prefer-dark
      font-name: "Cantarell 11"
    org.gnome.desktop.wm.preferences:
      button-layout: "close,minimize,maximize:"
```

### `kdeConfig` (Linux only)

Reads and writes [KDE Plasma settings](https://userbase.kde.org/KDE_System_Administration/Configuration_Files) via `kwriteconfig5`/`kwriteconfig6`. Each top-level key is a config file name, containing groups, each containing key-value pairs.

```yaml
system:
  kdeConfig:
    kdeglobals:
      General:
        ColorScheme: BreezeDark
      KDE:
        LookAndFeelPackage: org.kde.breezedark.desktop
    kwinrc:
      Compositing:
        Backend: OpenGL
```

### `xfconf` (Linux only)

Reads and writes [XFCE desktop settings](https://docs.xfce.org/xfce/xfconf/start) via `xfconf-query`. Each key is a channel name, values are property-path to value mappings. Properties that don't yet exist are created automatically as strings.

```yaml
system:
  xfconf:
    xfwm4:
      /general/theme: Default
      /general/title_font: "Sans Bold 9"
    xsettings:
      /Net/ThemeName: Adwaita
```

### `environment`

Manages environment variables by writing them to shell profile files (e.g., `~/.profile`, `~/.zshenv`). On Windows, variables are written to the user environment via the registry (`HKCU\Environment`) using `setx`, and are available to new processes immediately after apply.

```yaml
system:
  environment:
    GOPATH: ~/go
    EDITOR: nvim
```

### `windowsRegistry` (Windows only)

Manages Windows Registry values using a mapping format. Each key is a full registry path (`HIVE\Key\Subkey`), and each value is a name-to-data mapping. The data type is inferred automatically: numeric values become `REG_DWORD`, strings become `REG_SZ`.

```yaml
system:
  windowsRegistry:
    HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced:
      HideFileExt: 0
      ShowHiddenFiles: 1
    HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize:
      AppsUseLightTheme: 0
```

### `windowsServices` (Windows only)

Manages Windows Services via `sc.exe`. cfgd ensures each service exists with the specified configuration, start type, and running state.

```yaml
system:
  windowsServices:
    - name: MyService
      displayName: My Background Service
      binaryPath: C:\Program Files\MyApp\svc.exe
      startType: auto
      state: running
    - name: LegacyService
      startType: disabled
      state: stopped
```

Supported `startType` values: `auto`, `manual`, `disabled`. Supported `state` values: `running`, `stopped`.

### `sshKeys`

Provisions SSH key pairs and enforces their type and permissions. cfgd checks whether the key at `path` exists. If absent, it generates one via `ssh-keygen`. If present, it verifies the key type and permissions. The parent `~/.ssh` directory is created with `700` permissions if it does not exist.

```yaml
system:
  sshKeys:
    - name: default
      type: ed25519
      path: ~/.ssh/id_ed25519
      comment: "jane@work.com"
      passphrase: 1password://Work/SSH/passphrase  # optional secret ref
      permissions: "600"
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Identifier for this key entry. |
| `type` | enum | No | `ed25519` | Key algorithm: `ed25519` or `rsa`. |
| `bits` | int | No | `4096` | RSA key size. Ignored for ed25519. |
| `path` | string | No | `~/.ssh/id_<type>` | Path to the private key file. |
| `comment` | string | No | | Key comment (typically an email address). |
| `passphrase` | string | No | | Secret provider reference for the key passphrase. Plaintext passphrases are not supported. |
| `permissions` | string | No | `"600"` | Private key file permissions. |

Drift is detected when the key is missing, has the wrong type, or has incorrect permissions. Key type verification reads the public key file (`<path>.pub`) to avoid passphrase prompts during drift checks.

### `gpgKeys`

Provisions GPG keys and tracks their validity. cfgd matches on the primary UID email and usage capabilities. Revoked keys are ignored. If no matching valid key exists, cfgd generates one via `gpg --batch --gen-key`. An existing key that has expired is reported as drift.

```yaml
system:
  gpgKeys:
    - name: work-signing
      type: ed25519
      realName: "Jane Doe"
      email: jane@work.com
      expiry: 2y
      usage: sign
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Identifier for this key entry. |
| `type` | enum | No | `ed25519` | Key algorithm: `ed25519` or `rsa4096`. |
| `realName` | string | Yes | | GPG uid real name. |
| `email` | string | Yes | | GPG uid email address. Used for key matching. |
| `expiry` | string | No | `2y` | GPG expiry notation (`0` = no expiry, `1y`, `2y`, etc.). |
| `usage` | string | No | `sign` | Comma-separated key capabilities: `sign`, `encrypt`, `auth`, or combinations. |

Key fingerprints are visible via `cfgd status`, making them easy to reference in `git` configurator settings.

### `git`

Manages global git configuration. Each key maps directly to `git config --global <key> <value>`. The configurator uses dotted key-value pairs (not nested YAML) to match git's internal model. Keys not declared by cfgd are not modified.

```yaml
system:
  git:
    user.name: "Jane Doe"
    user.email: jane@work.com
    user.signingKey: ~/.ssh/id_ed25519.pub
    commit.gpgSign: true
    gpg.format: ssh
    init.defaultBranch: main
```

Drift is detected when any managed key has a value that differs from the declared value. cfgd reads current values via `git config --global --get <key>` and applies only the keys that differ.

## Node Configurators

These are typically used when cfgd runs as a [DaemonSet](https://kubernetes.io/docs/concepts/workloads/controllers/daemonset/) on Kubernetes cluster nodes (see [operator.md](operator.md#daemonset-mode)). They manage low-level Linux system settings that affect how containers and the kubelet behave.

### `sysctl`

Manages [kernel parameters](https://www.kernel.org/doc/html/latest/admin-guide/sysctl/index.html) — settings that control networking, memory, and filesystem behavior at the OS level. Persists to `/etc/sysctl.d/99-cfgd.conf`.

```yaml
system:
  sysctl:
    net.ipv4.ip_forward: 1
    vm.max_map_count: 262144
    net.bridge.bridge-nf-call-iptables: 1
```

### `kernelModules`

Loads [kernel modules](https://wiki.archlinux.org/title/Kernel_module) — pluggable pieces of the Linux kernel that add support for networking features, filesystems, or hardware. Persists to `/etc/modules-load.d/cfgd.conf`.

```yaml
system:
  kernelModules: [br_netfilter, overlay, ip_vs]
```

### `containerd`

Manages [containerd](https://containerd.io/) configuration — the container runtime that Kubernetes uses to run containers. Restarts containerd after changes.

```yaml
system:
  containerd:
    configPath: /etc/containerd/config.toml
    settings:
      SystemdCgroup: true
```

### `kubelet`

Manages [kubelet](https://kubernetes.io/docs/reference/command-line-tools-reference/kubelet/) configuration — the Kubernetes agent that runs on each node. Restarts kubelet after changes.

```yaml
system:
  kubelet:
    configPath: /var/lib/kubelet/config.yaml
    settings:
      maxPods: 110
```

### `apparmor`

Installs and loads [AppArmor](https://apparmor.net/) profiles — a Linux security framework that restricts what programs can do (file access, network, capabilities).

### `seccomp`

Installs [seccomp](https://kubernetes.io/docs/tutorials/security/seccomp/) JSON profiles — Linux syscall filters that restrict which kernel calls a container can make.

### `certificates`

Manages [X.509](https://en.wikipedia.org/wiki/X.509) certificate files (TLS/SSL certs) and enforces secure file permissions.

See the [CLI reference](cli-reference.md) for `cfgd profile update --system` and `cfgd profile create --system` commands.
