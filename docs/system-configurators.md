# System Configurators

The `system:` section in profiles routes each key to a registered system configurator. Available configurators depend on the OS and context. Configurators that aren't available on the current platform are silently skipped.

Each configurator follows the same pattern: read what the system has now, compare against what you want, and apply the difference.

## Workstation Configurators

### `shell`

Sets the default login shell via `chsh`. Value is the path to the shell binary.

```yaml
system:
  shell: /bin/zsh
```

### `macos-defaults` (macOS only)

Reads and writes macOS [`defaults`](https://macos-defaults.com/) domains. Each key is a domain name, values are key-value pairs to set.

```yaml
system:
  macos-defaults:
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

### `launch-agents` (macOS only)

Manages [LaunchAgent](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html) plist files in `~/Library/LaunchAgents` — macOS's way of running background services for your user session.

```yaml
system:
  launch-agents:
    - name: com.example.myservice
      program: /usr/local/bin/myservice
      args: ["--config", "/etc/myservice.conf"]
      run-at-load: true
```

### `systemd-units` (Linux only)

Manages [systemd](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html) user unit files — Linux's service manager. Handles unit file placement, enabling, and starting.

```yaml
system:
  systemd-units:
    - name: myservice.service
      unit-file: systemd/myservice.service
      enabled: true
```

### `environment`

Manages environment variables by writing them to shell profile files (e.g., `~/.profile`, `~/.zshenv`).

```yaml
system:
  environment:
    GOPATH: ~/go
    EDITOR: nvim
```

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

### `kernel-modules`

Loads [kernel modules](https://wiki.archlinux.org/title/Kernel_module) — pluggable pieces of the Linux kernel that add support for networking features, filesystems, or hardware. Persists to `/etc/modules-load.d/cfgd.conf`.

```yaml
system:
  kernel-modules: [br_netfilter, overlay, ip_vs]
```

### `containerd`

Manages [containerd](https://containerd.io/) configuration — the container runtime that Kubernetes uses to run containers. Restarts containerd after changes.

```yaml
system:
  containerd:
    config-path: /etc/containerd/config.toml
    settings:
      SystemdCgroup: true
```

### `kubelet`

Manages [kubelet](https://kubernetes.io/docs/reference/command-line-tools-reference/kubelet/) configuration — the Kubernetes agent that runs on each node. Restarts kubelet after changes.

```yaml
system:
  kubelet:
    config-path: /var/lib/kubelet/config.yaml
    settings:
      maxPods: 110
```

### `apparmor`

Installs and loads [AppArmor](https://apparmor.net/) profiles — a Linux security framework that restricts what programs can do (file access, network, capabilities).

### `seccomp`

Installs [seccomp](https://kubernetes.io/docs/tutorials/security/seccomp/) JSON profiles — Linux syscall filters that restrict which kernel calls a container can make.

### `certificates`

Manages [X.509](https://en.wikipedia.org/wiki/X.509) certificate files (TLS/SSL certs) and enforces secure file permissions.
