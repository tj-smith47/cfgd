# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.x.x   | :white_check_mark: |

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.**

Instead, please report security issues by emailing **security@cfgd.io**. Include:

- Description of the vulnerability
- Steps to reproduce
- Impact assessment
- Any suggested fix (optional)

### Response Timeline

- **48 hours** — Acknowledgment of receipt
- **7 days** — Initial assessment and severity rating
- **30-90 days** — Resolution, depending on complexity

## Security Considerations

cfgd handles sensitive data including:

- **Secrets** — SOPS-encrypted files, age keys, external provider credentials (1Password, Bitwarden, Vault)
- **System configuration** — Shell settings, systemd units, macOS defaults
- **Package management** — Installs software with elevated privileges (sudo for apt, dnf)
- **File management** — Writes files to arbitrary paths with configurable permissions

### Best Practices

- Keep cfgd updated to the latest version
- Store age keys securely (`~/.config/cfgd/age-key.txt` with mode 600)
- Use `.sops.yaml` creation rules to prevent accidental plaintext commits
- Review `cfgd apply --dry-run` output before running `cfgd apply`
- Audit external secret provider access (1Password, Bitwarden, Vault)
- Run the daemon with least-privilege where possible

## Remote Modules Security Model

Remote modules let you install shared configuration packages from git repos. Because modules can install packages and run scripts, they are effectively **code execution from a URL**. cfgd takes this seriously.

### How Trust Works

When you add a remote module (`cfgd module add community/tmux@v1.0.0`), cfgd:

1. **Fetches the module** and shows you exactly what it will do: which packages it installs, which files it deploys, and which scripts it runs.
2. **Asks for your confirmation** before writing anything. You see the full spec before you commit.
3. **Locks the exact commit and content hash** in `modules.lock`. From this point on, cfgd will only use that exact version of the module.

Updates work the same way. `cfgd module update tmux` shows you a diff of what changed between versions and asks for confirmation.

### Pinned Refs

Remote modules **must** be pinned to a specific tag or commit. You cannot track a branch like `master` for a remote module. This is a hard rule because branch tracking means an upstream push silently changes what runs on your machine.

### Integrity Verification

Every locked module records a `sha256` hash of the module directory contents. On every load, cfgd recomputes the hash and compares it to the lockfile. If someone tampers with the cached checkout, cfgd catches it immediately.

### Signature Verification

If a module tag has a GPG or SSH signature, cfgd verifies it cryptographically using `git tag -v`. The behavior is:

- **Signed + valid**: cfgd reports the signature is verified and proceeds.
- **Signed + invalid**: cfgd **refuses to continue**. There is no override for a bad signature. If the signature doesn't verify, something is wrong.
- **Unsigned**: cfgd notes the tag is unsigned and proceeds by default.

If your organization requires signed modules, set `require-signatures` in your config:

```yaml
spec:
  module-security:
    require-signatures: true
```

With this enabled, unsigned modules are rejected. You can override on a per-command basis with `--allow-unsigned` if you trust the source.

### What cfgd Does Not Do

cfgd does not verify *who* signed a tag. It only checks that the signature is valid against your local GPG keyring or SSH allowed_signers file. Managing which keys you trust is your responsibility, the same way it works with `git tag -v` directly.

cfgd does not sandbox script execution. Post-apply scripts run with your user's privileges. This is by design: cfgd manages machine configuration, which inherently requires the ability to modify system state.

### Threat Summary

| Threat | How cfgd handles it |
|--------|-------------------|
| Malicious upstream push | Locked to specific commit + integrity hash. No branch tracking. |
| Tag rewriting (force-push) | Integrity hash covers content. Changed content fails verification even if the tag name is the same. |
| Compromised source repo | Signature verification catches tampering when tags are signed. |
| Man-in-the-middle | Standard git HTTPS/SSH transport security. |
| Cache poisoning | Integrity hash checked against lockfile on every load. |
| Lockfile tampering | Lockfile lives in your git-tracked config directory. Unauthorized changes show up in `git diff`. |
| Supply chain via dependencies | Module dependencies resolve from your config directory only. Remote modules cannot pull in arbitrary remote dependencies. |
