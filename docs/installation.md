# Installation

cfgd ships pre-built binaries for Linux, macOS, and Windows (x86_64 + aarch64)
through the [GitHub Releases page](https://github.com/tj-smith47/cfgd/releases)
and a number of platform-native package managers. Pick whichever channel best
fits how you manage tooling on each machine — the binary is identical across
channels.

## Linux / macOS

### Homebrew (Linux + macOS)

```sh
brew install tj-smith47/tap/cfgd
```

The tap also publishes the operator and CSI binaries under
`tj-smith47/tap/cfgd-operator` and `tj-smith47/tap/cfgd-csi` for cluster-side
installs.

### Install script

```sh
curl -fsSL https://github.com/tj-smith47/cfgd/releases/latest/download/install.sh | sh
```

The script detects the OS and architecture, downloads the matching tarball,
verifies the SHA256 + cosign signature, and drops the `cfgd` binary into
`/usr/local/bin` (or `~/.local/bin` if `/usr/local/bin` isn't writable).

### Cargo (any platform with a Rust toolchain)

```sh
cargo install cfgd
```

Useful when no pre-built binary exists for your platform, or when you want a
debug-symbol-stripped release-mode build compiled against your local toolchain.

### Direct download

Download the platform-specific archive from
[the latest release](https://github.com/tj-smith47/cfgd/releases/latest),
verify the checksum + signature, and place `cfgd` somewhere on `PATH`.

```sh
curl -L -o cfgd.tar.gz \
  https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd-linux-x86_64.tar.gz
tar -xzf cfgd.tar.gz
install -m 0755 cfgd /usr/local/bin/cfgd
```

## Windows

cfgd publishes signed installers to the three mainstream Windows package
managers. All three deliver the same binary — pick whichever you already use
to manage tooling on the machine.

### winget (Windows 11 / App Installer)

```powershell
winget install --id TJSmith.cfgd
```

`winget` ships with Windows 11 and recent Windows 10 installs (via the
Microsoft Store "App Installer"). Upgrade in place with:

```powershell
winget upgrade --id TJSmith.cfgd
```

The published manifest declares a dependency on
`Microsoft.VCRedist.2015+.x64`; winget pulls it in automatically the first
time you install cfgd.

### Scoop

```powershell
# Add the bucket once per machine
scoop bucket add tj-smith47 https://github.com/tj-smith47/scoop-bucket
scoop install cfgd
```

Scoop installs into `%USERPROFILE%\scoop\apps\cfgd\current\` and shims
`cfgd.exe` onto `PATH` without elevation. `scoop update cfgd` upgrades to the
latest release.

### Chocolatey

```powershell
choco install cfgd
```

Chocolatey is a community Windows package manager (sometimes already present
in CI runners or developer images). `choco upgrade cfgd` upgrades, and
`choco uninstall cfgd` removes it.

### Direct download

```powershell
Invoke-WebRequest -Uri https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd-windows-x86_64.zip -OutFile cfgd.zip
Expand-Archive cfgd.zip -DestinationPath C:\Tools\cfgd
# Add C:\Tools\cfgd to your PATH (System Properties → Environment Variables)
```

Verify the signature against the cosign public key published on the release
page before running the binary on shared hosts.

### Visual C++ runtime requirement

Rust/MSVC binaries dynamically link against the VC++ 2015+ x64 runtime. Most
modern Windows installs already have it (Windows Update, Office, Visual
Studio, and many games install it as a side-effect). If `cfgd.exe` fails to
start with `STATUS_DLL_NOT_FOUND` (exit code `-1073741515`), install the
runtime explicitly:

```powershell
winget install --id Microsoft.VCRedist.2015+.x64
```

The winget package above declares this as a dependency, so it's only an issue
for the direct-download path or for uncommon Windows variants.

## Verifying the install

Once installed by any channel, confirm the binary is on `PATH` and reports a
sensible version:

```sh
cfgd version
```

To verify the signature on the downloaded archive (if you used the install
script or direct download):

```sh
cosign verify-blob \
  --key https://github.com/tj-smith47/cfgd/releases/latest/download/cosign.pub \
  --signature cfgd-linux-x86_64.tar.gz.sig \
  cfgd-linux-x86_64.tar.gz
```

The same `cosign.pub` key signs all release artifacts (Linux, macOS, Windows,
container images, Helm charts).

## Containers and Kubernetes

For cluster-side installs (operator, CSI driver, agent DaemonSet) use the
Helm chart instead of a per-node binary:

```sh
helm install cfgd oci://ghcr.io/tj-smith47/charts/cfgd
```

See [docs/operator.md](operator.md) for the full Helm values reference and
[docs/multi-tenancy.md](multi-tenancy.md) for tenancy scoping.

The kubectl plugin (for debugging nodes from your workstation) ships inside
the same `cfgd` binary — install `cfgd` by any channel above and place a
symlink (or copy) named `kubectl-cfgd` somewhere on `PATH`. The binary
dispatches on `argv[0]`, so kubectl picks it up as the `cfgd` plugin:

```sh
ln -s "$(command -v cfgd)" "$HOME/.local/bin/kubectl-cfgd"
kubectl cfgd version
```

## Next steps

- [Bootstrap a config](bootstrap.md) — `cfgd init` against a git repo or a
  fresh local directory
- [Configuration reference](configuration.md) — full YAML schema (including
  Windows-specific behaviors)
- [Package manager reference](packages.md) — `winget`, `chocolatey`, `scoop`,
  and the cross-platform managers cfgd uses *inside* configs
- [Daemon setup](daemon.md) — installing the reconciliation daemon as a
  systemd service, launchd agent, or Windows Service
