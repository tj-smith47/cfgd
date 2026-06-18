# Installation

cfgd ships pre-built binaries for Linux, macOS, and Windows (amd64 + arm64)
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

### AUR (Arch Linux)

cfgd is published to the [AUR](https://aur.archlinux.org/packages/cfgd) as a
source package — it compiles from source on install (the `rust` and `cargo`
build dependencies are pulled in automatically). Use any AUR helper:

```sh
yay -S cfgd      # or: paru -S cfgd
```

Or build it by hand:

```sh
git clone https://aur.archlinux.org/cfgd.git
cd cfgd && makepkg -si
```

### Linux native packages (deb / rpm / apk)

Each release publishes signed `.deb`, `.rpm`, and `.apk` packages. Download the
one matching your distro and install it with the native package manager:

```sh
# Debian / Ubuntu (and derivatives)
curl -L -o cfgd.deb \
  https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd_0.5.0_linux_amd64.deb
sudo dpkg -i cfgd.deb        # or: sudo apt install ./cfgd.deb

# Fedora / RHEL / Alma / Rocky / Amazon Linux (dnf or yum)
sudo dnf install \
  https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd_0.5.0_linux_amd64.rpm

# Alpine
curl -L -o cfgd.apk \
  https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd_0.5.0_linux_amd64.apk
sudo apk add --allow-untrusted cfgd.apk
```

These packages bundle a statically linked binary, so they install and run on
**any** Linux distribution regardless of its C library — musl (Alpine) as well
as older glibc releases (Enterprise Linux 7/8/9, Amazon Linux 2, and long-term
support distributions) that the dynamically linked tarball does not support. The
packages are also mirrored to a [CloudSmith](https://cloudsmith.io/~jarvispro/repos/cfgd/)
repository for apt/dnf/apk repo-based installs.

### Direct download

Download the platform-specific archive from
[the latest release](https://github.com/tj-smith47/cfgd/releases/latest),
verify the checksum + signature, and place `cfgd` somewhere on `PATH`. Release
assets are versioned and named `cfgd-<version>-<os>-<arch>.tar.gz`, where
`<arch>` is `amd64` or `arm64` (Windows ships `.zip`).

```sh
curl -L -o cfgd.tar.gz \
  https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd-0.5.0-linux-amd64.tar.gz
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
Invoke-WebRequest -Uri https://github.com/tj-smith47/cfgd/releases/latest/download/cfgd-0.5.0-windows-amd64.zip -OutFile cfgd.zip
Expand-Archive cfgd.zip -DestinationPath C:\Tools\cfgd
# Add C:\Tools\cfgd to your PATH (System Properties → Environment Variables)
```

Before running the binary on shared hosts, verify the signature with keyless
cosign — see [Verifying downloads](#verifying-downloads) below.

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

To verify the signature on a downloaded archive by hand, see
[Verifying downloads](#verifying-downloads) below.

## Verifying downloads

Each release artifact is signed with **keyless cosign** (Fulcio/OIDC + Rekor) —
there is no long-lived public key to trust. For every archive `<archive>` (for
example `cfgd-0.5.0-linux-amd64.tar.gz`) the release publishes:

| Asset | Purpose |
|---|---|
| `<archive>.sha256` | bare SHA256 hash of the archive (one file per artifact, not a combined `checksums.txt`) |
| `<archive>.sha256.cosign.bundle` | keyless cosign signature over the `.sha256` file (embeds the Fulcio cert + Rekor proof) |
| `<archive>.sha256.cosign.pem` | the Fulcio certificate (also published; optional — the bundle already embeds it) |

To verify a download, run the two steps below. This is exactly what
`cfgd upgrade` performs internally:

```sh
VER=0.5.0; ARCH=amd64; OS=linux          # adjust: amd64|arm64, linux|darwin|windows
A="cfgd-${VER}-${OS}-${ARCH}.tar.gz"
base="https://github.com/tj-smith47/cfgd/releases/download/v${VER}"
curl -fsSLO "$base/$A"
curl -fsSLO "$base/$A.sha256"
curl -fsSLO "$base/$A.sha256.cosign.bundle"

# 1. Verify the keyless cosign signature over the checksum file. This proves the
#    checksum came from cfgd's own release workflow, not just from GitHub asset
#    hosting.
cosign verify-blob \
  --bundle "$A.sha256.cosign.bundle" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  --certificate-identity-regexp '^https://github\.com/tj-smith47/cfgd/\.github/workflows/release\.yml@' \
  "$A.sha256"

# 2. Verify the archive matches the (now-trusted) checksum.
echo "$(cat "$A.sha256")  $A" | sha256sum -c
```

Notes:

- The issuer is the GitHub Actions OIDC provider, and the
  `--certificate-identity-regexp` pins the signer to cfgd's own `release.yml`
  workflow. A publisher-compromise attacker cannot mint a passing signature
  without running that exact workflow — replacing the binary and its `.sha256`
  on a mirror is not enough.
- Verification requires the [`cosign` CLI](https://docs.sigstore.dev/cosign/system_config/installation/).
  Keyless verification needs network access to the Fulcio/Rekor roots, which
  cosign fetches via the bundled Sigstore TUF root.
- On Windows, swap `OS=windows` and the `.tar.gz` suffix for `.zip`, and use a
  SHA256 tool such as `Get-FileHash` instead of `sha256sum`. The cosign step is
  identical.

## Upgrading

`cfgd upgrade` self-updates the binary in place from the latest GitHub release.
It downloads the platform archive, verifies it, and atomically replaces the
running binary (restarting the daemon if one is active).

```sh
cfgd upgrade                   # download, verify, and install the latest release
cfgd upgrade --check           # check only: exit 0 = current, 2 = update available, 1 = error
cfgd upgrade --require-cosign  # fail (don't degrade) if the cosign signature can't be verified
CFGD_REQUIRE_COSIGN=1 cfgd upgrade
```

The verification it performs is the same two steps as
[Verifying downloads](#verifying-downloads): it verifies the keyless cosign
signature over the `<archive>.sha256` file (pinned to cfgd's `release.yml`
workflow identity), then confirms the archive matches that trusted checksum.

By default, if the `cosign` CLI isn't installed locally — or the release lacks
the cosign bundle — `cfgd upgrade` emits a warning and **falls back to
SHA256-only** verification, which trusts GitHub Releases asset hosting alone.
Pass `--require-cosign` (or set `CFGD_REQUIRE_COSIGN=1`) to make signature
verification mandatory: any condition that would trigger the fallback fails the
upgrade instead. This is recommended for unattended and CI updaters, where a
silent downgrade to SHA256-only should never happen.

A binary old enough to predate this self-upgrade logic cannot bootstrap the
verified path. Reinstall once via any of the [install methods](#linux--macos)
above (Homebrew, the install script, etc.); subsequent `cfgd upgrade` runs then
work from the newer binary.

## Containers and Kubernetes

For cluster-side installs (operator, CSI driver, agent DaemonSet) use the
Helm chart instead of a per-node binary:

```sh
helm install cfgd oci://ghcr.io/tj-smith47/charts/cfgd
```

See [docs/operator.md](operator.md) for the full Helm values reference and
[docs/multi-tenancy.md](multi-tenancy.md) for tenancy scoping.

The kubectl plugin (for debugging nodes from your workstation) installs via
Krew:

```sh
kubectl krew install cfgd
```

The plugin ships inside the same `cfgd` binary and dispatches on `argv[0]`,
so installing via any other channel works too — symlink (or copy) the
binary as `kubectl-cfgd` somewhere on `PATH` and kubectl picks it up:

```sh
ln -s "$(command -v cfgd)" "$HOME/.local/bin/kubectl-cfgd"
kubectl cfgd version
```

### Plugin commands

| Command                            | Purpose                                                   |
|------------------------------------|----------------------------------------------------------|
| `kubectl cfgd debug <pod>`         | Attach an ephemeral debug container with modules mounted  |
| `kubectl cfgd exec <pod> -- <cmd>` | Run a command in a pod with module `PATH` extended        |
| `kubectl cfgd inject <kind>/<name>`| Patch a workload to mount modules on every replica        |
| `kubectl cfgd status`              | Per-node module status across the cluster                 |
| `kubectl cfgd version`             | Client, apiserver, operator, and CSI driver versions      |

Every plugin command accepts the global `-o`/`--output` flag (it mirrors the
workstation CLI), so output can be rendered as `table` (default), `wide`,
`json`, `yaml`, `name`, `jsonpath=EXPR`, `template=TMPL`, or
`template-file=PATH`. The flag is global, so it works before or after the
subcommand:

```sh
kubectl cfgd -o json version
kubectl cfgd status -o yaml
```

`kubectl cfgd version` reports four versions: the client plugin, the
Kubernetes apiserver, and the deployed cfgd **operator** and **CSI driver**.
The operator/CSI versions are read from the running images' tags in the cfgd
namespace (`cfgd-system` by default; override with `--namespace`). When the
cluster is unreachable, a component isn't deployed, or RBAC forbids the
lookup, that field degrades gracefully (`not connected` / `not deployed` /
`unknown (forbidden)`) and the command still exits 0:

```sh
$ kubectl cfgd version
Client        0.5.0
Server (k8s)  1.31
Operator      0.5.0
CSI           0.5.0

$ kubectl cfgd version --namespace cfgd-system -o json
{
  "version": "0.5.0",
  "kubectl": "1.31",
  "operator": "0.5.0",
  "csi": "0.5.0",
  "cfgd": "0.5.0"
}
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
