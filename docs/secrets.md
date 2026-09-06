# Secrets

Secrets in config repos are a common problem: you want API keys, tokens, and certificates version-controlled alongside your config, but you can't commit them in plaintext. External providers like 1Password solve storage but not deployment: you still need to get the secret to the right file on the right machine.

cfgd handles both. Encrypted secrets live in your git repo (safe to commit), and external provider references are resolved at apply time and placed at their target paths.

## Encryption Backends

cfgd supports two encryption backends. They serve different purposes.

### SOPS (primary): for structured config files

[SOPS](https://github.com/getsops/sops) encrypts individual values within YAML/JSON files while leaving keys in plaintext. `git diff` shows which keys changed (values stay opaque), and you can review the structure of encrypted files without decrypting them.

Best for: API key files, environment configs, credential YAML, anything where you want meaningful diffs.

### age (fallback): for opaque files

[age](https://age-encryption.org/) encrypts entire files as opaque blobs.

Best for: binary files (TLS certs, keystores), or files where SOPS's structured encryption doesn't apply.

cfgd doesn't automatically fall back from SOPS to age. The default backend is SOPS; override per file via the `backend` field in your profile.

## External Providers

External providers reference secrets stored in password managers or vaults. cfgd resolves the reference at apply time, fetches the value, and places it at the target path. The secret value is never written to your config repo.

| Provider | Reference Format | CLI Required |
|---|---|---|
| 1Password | `1password://Vault/Item/Field` or `op://Vault/Item/Field` | [`op`](https://developer.1password.com/docs/cli/) |
| Bitwarden | `bitwarden://folder/item` or `bw://folder/item` | [`bw`](https://bitwarden.com/help/cli/) |
| LastPass | `lastpass://folder/item/field`, `lpass://folder/item/field`, or `lp://folder/item/field` | [`lpass`](https://github.com/lastpass/lastpass-cli) |
| HashiCorp Vault | `vault://secret/path#key` | [`vault`](https://developer.hashicorp.com/vault/docs/commands) |

Providers and encryption backends combine freely: most secrets SOPS-encrypted in the repo, a few high-sensitivity tokens fetched from 1Password at apply time.

Secret references also work inside deployed file content with `${secret:ref}` syntax (see [Secret References](templates.md#secret-references)).

`cfgd doctor` reports which provider CLIs are installed, along with sops/age key and `.sops.yaml` status.

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

`template` wraps a provider-resolved value before it is written: `${secret:value}` is replaced with the resolved secret and everything else is written verbatim, so a bare token can be delivered as the config line a tool expects. It applies to `target` and `envs` alike (each variable receives the rendered string). Two rules are enforced at parse time: the template must contain `${secret:value}`, and it is only accepted on a provider reference (`1password://`, `bitwarden://`, `lastpass://`, `vault://` and their aliases), never on an encrypted file, whose contents are already the file to write.

## Environment Variable Injection

Secrets can be injected directly into the shell environment without writing a file. Add an `envs` field to any secret entry with a list of environment variable names to populate. The daemon resolves the secret and writes the values to its managed shell env file alongside regular `env:` entries from your profile.

```yaml
secrets:
  # Inject only into the shell environment
  - source: 1password://Work/GitHub/token
    envs:
      - GITHUB_TOKEN

  # Write to a file and also inject as an env var
  - source: vault://secret/data/api#key
    target: ~/.config/api-key
    envs:
      - API_KEY
```

At least one of `target` or `envs` must be set on each entry. When both are set, the secret is placed at the target path and exported as an env var. When `envs` lists multiple names and the source resolves to a single value, all named variables receive the same value.

For secrets with multiple fields (e.g. a Vault path with separate access key and secret key), use one entry per field with an explicit fragment reference:

```yaml
secrets:
  - source: vault://secret/data/aws#aws_access_key_id
    envs:
      - AWS_ACCESS_KEY_ID
  - source: vault://secret/data/aws#aws_secret_access_key
    envs:
      - AWS_SECRET_ACCESS_KEY
```

The daemon refreshes secret-backed env vars on every reconcile cycle. Compliance snapshots record that the env var exists and its source; the value is never stored or logged.

## CLI Commands

`cfgd secret init` sets up encryption for your config repo: it generates an [age](https://age-encryption.org/) key pair and creates a `.sops.yaml` configuration file that tells SOPS which files to encrypt and with which key.

```sh
cfgd secret init                    # generate age key + .sops.yaml
cfgd secret encrypt secrets.yaml    # encrypt values in place (keys stay readable, values become ciphertext)
cfgd secret decrypt secrets.yaml    # decrypt to stdout (original file unchanged)
cfgd secret edit secrets.yaml       # decrypt to temp file, open $EDITOR, re-encrypt on save
```
