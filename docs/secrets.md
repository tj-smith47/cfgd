# Secrets

Secrets in config repos are a common problem: you want API keys, tokens, and certificates version-controlled alongside your config, but you can't commit them in plaintext. External providers like 1Password solve storage but don't solve deployment — you still need to get the secret to the right file on the right machine.

cfgd handles both: encrypted secrets live in your git repo (safe to commit), and external provider references are resolved at apply time and placed at their target paths. You declare where secrets come from and where they go; cfgd handles the rest.

## Encryption Backends

cfgd supports two encryption backends. Use both — they serve different purposes.

### SOPS (primary) — for structured config files

[SOPS](https://github.com/getsops/sops) encrypts individual values within YAML/JSON files while leaving keys in plaintext. This means `git diff` shows which keys changed (even though values are opaque), and you can review the structure of encrypted files without decrypting them.

Best for: API key files, environment configs, credential YAML — anything where you want meaningful diffs.

### age (fallback) — for opaque files

[age](https://age-encryption.org/) encrypts entire files as opaque blobs. You can't see what's inside without the key.

Best for: binary files (TLS certs, keystores), or files where SOPS's structured encryption doesn't apply.

cfgd doesn't automatically fall back from SOPS to age. You choose per-file via the `backend` field in your profile. The default backend is SOPS.

## External Providers

External providers let you reference secrets stored in password managers or vaults. cfgd resolves the reference at apply time, fetches the value, and places it at the target path. The secret value is never written to your config repo.

| Provider | Reference Format | CLI Required |
|---|---|---|
| 1Password | `1password://Vault/Item/Field` or `op://Vault/Item/Field` | [`op`](https://developer.1password.com/docs/cli/) |
| Bitwarden | `bitwarden://folder/item` or `bw://folder/item` | [`bw`](https://bitwarden.com/help/cli/) |
| HashiCorp Vault | `vault://secret/path#key` | [`vault`](https://developer.hashicorp.com/vault/docs/commands) |

You can use external providers alongside encryption backends. For example, most secrets can be SOPS-encrypted in the repo, while a few high-sensitivity tokens are fetched from 1Password at apply time.

Secret references can be used in templates with `${secret:ref}` syntax.

## Configuration

Configure the secrets backend in `cfgd.yaml`:

```yaml
spec:
  secrets:
    backend: sops
    sops:
      ageKey: ~/.config/cfgd/age-key.txt
    integrations:
      - name: 1password
      - name: bitwarden
      - name: vault
```

## Profile Usage

```yaml
secrets:
  - source: secrets/api-keys.yaml       # SOPS-encrypted file
    target: ~/.config/api-keys.yaml
  - source: 1password://Work/GitHub/token  # external provider
    target: ~/.config/gh/token
    template: "token: ${secret:value}"     # optional template wrapping
  - source: secrets/tls-cert.pem
    target: /etc/ssl/certs/my-cert.pem
    backend: age                           # per-file backend override
```

## CLI Commands

`cfgd secret init` sets up encryption for your config repo — generates an [age](https://age-encryption.org/) key pair and creates a `.sops.yaml` configuration file that tells SOPS which files to encrypt and with which key.

```sh
cfgd secret init                    # generate age key + .sops.yaml
cfgd secret encrypt secrets.yaml    # encrypt values in place (keys stay readable, values become ciphertext)
cfgd secret decrypt secrets.yaml    # decrypt to stdout (original file unchanged)
cfgd secret edit secrets.yaml       # decrypt to temp file, open $EDITOR, re-encrypt on save
```
