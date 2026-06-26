<!-- cfgd:skill:source -->
<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd Source

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition — confirm the toolchain is usable.** Run `command -v cfgd`; if it is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>. Run `cfgd --version`; if it is older than <CFGD_MIN_VERSION>, warn and prefer the embedded fallback schema below.
1. **Enumerate every field for this kind (live-first, snapshot-fallback).** Run `cfgd explain source -o json` for the authoritative live schema, and `cfgd explain source.<field> -o json` to drill into nested objects. If cfgd is absent or older than the stamp, use the embedded fallback schema below (stamped <CFGD_VERSION>).
2. **Research best practices externally for THIS subject.** For each field, consult external best practice before settling a value: the tool's own docs, the package managers that ship it, and community conventions. Record what you verified and your confidence level when a source was unavailable. Prefer live evidence over training-knowledge recall, and state explicitly when you could not confirm a claim.
3. **For EVERY field, decide include OR omit, and justify with a WHY comment.** Box-checking is a failure; meeting the rubric above is the target.
4. **Draft thoroughly:** transitive deps explicit, version constraints set, platforms scoped, multi-step scripts idempotent (timeout + continueOnError), comments-as-specification.
5. **Validate against the schema:** `cfgd source validate <file>` — fix until clean (validate against the embedded snapshot if cfgd is unavailable).
6. **Self-critique against the rubric:** "Box-checking or thorough? Which field did I skip, and was that deliberate?" Iterate until the answer holds.

## Ground-truth examples

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme-corp-dev
  version: "2.1.0"
  description: ACME Corp developer environment baseline
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
      - acme-frontend
    platformProfiles:
      macos: acme-base
      debian: acme-backend
      ubuntu: acme-backend
      fedora: acme-frontend
      linux: acme-base
    modules:
      - corp-vpn
      - corp-certs
      - approved-editor

  policy:
    required:
      packages:
        brew:
          formulae:
            - git-secrets
            - pre-commit
            - aws-cli
      files:
        - source: linting/.eslintrc.json
          target: ~/.eslintrc.json
      modules:
        - corp-vpn
        - corp-certs
    recommended:
      packages:
        brew:
          formulae:
            - k9s
            - stern
            - kubectx
      modules:
        - approved-editor
    optional:
      profiles:
        - acme-sre
    locked:
      files:
        - source: security/security-policy.yaml
          target: ~/.config/company/security-policy.yaml
    constraints:
      noScripts: true
      noSecretsRead: true
      allowedTargetPaths:
        - ~/.config/acme/
        - ~/.config/company/
```

## Fallback schema (if cfgd is unavailable)

Generated against cfgd <CFGD_VERSION>. Live `cfgd explain source` is authoritative when present.

```json
{"$schema":"https://json-schema.org/draft-07/schema#","additionalProperties":false,"definitions":{"AptSpec":{"additionalProperties":false,"properties":{"file":{"default":null,"type":["string","null"]},"packages":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"BrewSpec":{"additionalProperties":false,"properties":{"casks":{"default":[],"items":{"type":"string"},"type":"array"},"file":{"default":null,"type":["string","null"]},"formulae":{"default":[],"items":{"type":"string"},"type":"array"},"taps":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"CargoSpec":{"additionalProperties":false,"description":"Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`) and object form (`cargo: { file: Cargo.toml, packages: [...] }`) via the shared `list_or_struct` deserializer on the `PackagesSpec::cargo` field.","properties":{"file":{"type":["string","null"]},"packages":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"ConfigSourcePolicy":{"additionalProperties":false,"properties":{"constraints":{"$ref":"#/definitions/SourceConstraints","default":{"allowSystemChanges":false,"allowedTargetPaths":[],"noScripts":true,"noSecretsRead":true,"requireSignedCommits":false}},"locked":{"$ref":"#/definitions/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"optional":{"$ref":"#/definitions/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"recommended":{"$ref":"#/definitions/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"required":{"$ref":"#/definitions/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}}},"type":"object"},"ConfigSourceProfileEntry":{"additionalProperties":false,"description":"Detailed profile entry in a ConfigSource manifest. When present, provides richer info than the flat `profiles` list.","properties":{"description":{"default":null,"type":["string","null"]},"inherits":{"default":[],"items":{"type":"string"},"type":"array"},"name":{"type":"string"},"path":{"default":null,"type":["string","null"]}},"required":["name"],"type":"object"},"ConfigSourceProvides":{"additionalProperties":false,"properties":{"modules":{"default":[],"items":{"type":"string"},"type":"array"},"platformProfiles":{"additionalProperties":{"type":"string"},"default":{},"type":"object"},"profileDetails":{"default":[],"items":{"$ref":"#/definitions/ConfigSourceProfileEntry"},"type":"array"},"profiles":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"CustomManagerSpec":{"additionalProperties":false,"properties":{"check":{"type":"string"},"install":{"type":"string"},"listInstalled":{"type":"string"},"name":{"type":"string"},"packages":{"default":[],"items":{"type":"string"},"type":"array"},"uninstall":{"type":"string"},"update":{"default":null,"type":["string","null"]}},"required":["name","check","listInstalled","install","uninstall"],"type":"object"},"EncryptionConstraint":{"additionalProperties":false,"description":"Encryption constraint applied to files from a config source.","properties":{"backend":{"description":"If set, restrict which backend is acceptable.","type":["string","null"]},"mode":{"anyOf":[{"$ref":"#/definitions/EncryptionMode"},{"type":"null"}],"description":"If set, restrict which encryption mode is acceptable."},"requiredTargets":{"default":[],"description":"Glob patterns or explicit paths that must be encrypted.","items":{"type":"string"},"type":"array"}},"type":"object"},"EncryptionMode":{"description":"Controls when encryption is required for a managed file.","oneOf":[{"const":"InRepo","description":"File must be encrypted when stored in the repository.","type":"string"},{"const":"Always","description":"File must always be encrypted, including at rest on disk.","type":"string"}]},"EncryptionSpec":{"additionalProperties":false,"description":"Encryption settings for a managed file.","properties":{"backend":{"description":"The encryption backend to use (e.g. \"sops\", \"age\").","type":"string"},"mode":{"$ref":"#/definitions/EncryptionMode","default":"InRepo","description":"When encryption must be enforced. Defaults to `InRepo`."}},"required":["backend"],"type":"object"},"EnvVar":{"properties":{"name":{"type":"string"},"value":{"type":"string"}},"required":["name","value"],"type":"object"},"FileStrategy":{"description":"File deployment strategy.","oneOf":[{"const":"Symlink","description":"Create a symbolic link from target to source (default).","type":"string"},{"const":"Copy","description":"Copy source content to target.","type":"string"},{"const":"Template","description":"Render a Tera template and write the output (auto-selected for .tera files).","type":"string"},{"const":"Hardlink","description":"Create a hard link from target to source.","type":"string"}]},"FlatpakSpec":{"additionalProperties":false,"properties":{"packages":{"default":[],"items":{"type":"string"},"type":"array"},"remote":{"default":null,"type":["string","null"]}},"type":"object"},"ManagedFileSpec":{"additionalProperties":false,"properties":{"encryption":{"anyOf":[{"$ref":"#/definitions/EncryptionSpec"},{"type":"null"}],"description":"Encryption settings for this file."},"permissions":{"description":"Unix permission bits (e.g. \"600\", \"644\") to apply after deployment.","type":["string","null"]},"private":{"description":"When true, the source file is local-only: auto-added to .gitignore, silently skipped on machines where it doesn't exist.","type":"boolean"},"source":{"type":"string"},"strategy":{"anyOf":[{"$ref":"#/definitions/FileStrategy"},{"type":"null"}],"description":"Per-file deployment strategy override. If None, uses the global default."},"target":{"type":"string"}},"required":["source","target"],"type":"object"},"NpmSpec":{"additionalProperties":false,"properties":{"file":{"default":null,"type":["string","null"]},"global":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"PackagesSpec":{"additionalProperties":false,"properties":{"apk":{"default":[],"items":{"type":"string"},"type":"array"},"apt":{"anyOf":[{"$ref":"#/definitions/AptSpec"},{"type":"null"}],"default":null},"brew":{"anyOf":[{"$ref":"#/definitions/BrewSpec"},{"type":"null"}],"default":null},"cargo":{"anyOf":[{"$ref":"#/definitions/CargoSpec"},{"type":"null"}],"default":null},"chocolatey":{"default":[],"items":{"type":"string"},"type":"array"},"custom":{"default":[],"items":{"$ref":"#/definitions/CustomManagerSpec"},"type":"array"},"dnf":{"default":[],"items":{"type":"string"},"type":"array"},"flatpak":{"anyOf":[{"$ref":"#/definitions/FlatpakSpec"},{"type":"null"}],"default":null},"go":{"default":[],"items":{"type":"string"},"type":"array"},"nix":{"default":[],"items":{"type":"string"},"type":"array"},"npm":{"anyOf":[{"$ref":"#/definitions/NpmSpec"},{"type":"null"}],"default":null},"pacman":{"default":[],"items":{"type":"string"},"type":"array"},"pipx":{"default":[],"items":{"type":"string"},"type":"array"},"pkg":{"default":[],"items":{"type":"string"},"type":"array"},"scoop":{"default":[],"items":{"type":"string"},"type":"array"},"snap":{"anyOf":[{"$ref":"#/definitions/SnapSpec"},{"type":"null"}],"default":null},"winget":{"default":[],"items":{"type":"string"},"type":"array"},"yum":{"default":[],"items":{"type":"string"},"type":"array"},"zypper":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"PolicyItems":{"additionalProperties":false,"properties":{"aliases":{"default":[],"items":{"$ref":"#/definitions/ShellAlias"},"type":"array"},"env":{"default":[],"items":{"$ref":"#/definitions/EnvVar"},"type":"array"},"files":{"default":[],"items":{"$ref":"#/definitions/ManagedFileSpec"},"type":"array"},"modules":{"default":[],"items":{"type":"string"},"type":"array"},"packages":{"anyOf":[{"$ref":"#/definitions/PackagesSpec"},{"type":"null"}],"default":null},"profiles":{"default":[],"items":{"type":"string"},"type":"array"},"secrets":{"default":[],"items":{"$ref":"#/definitions/SecretSpec"},"type":"array"},"system":{"additionalProperties":true,"default":{},"type":"object"}},"type":"object"},"SecretSpec":{"additionalProperties":false,"properties":{"backend":{"type":["string","null"]},"envs":{"items":{"type":"string"},"type":["array","null"]},"source":{"type":"string"},"target":{"type":["string","null"]},"template":{"type":["string","null"]}},"required":["source"],"type":"object"},"ShellAlias":{"properties":{"command":{"type":"string"},"name":{"type":"string"}},"required":["name","command"],"type":"object"},"SnapSpec":{"additionalProperties":false,"properties":{"classic":{"default":[],"items":{"type":"string"},"type":"array"},"packages":{"default":[],"items":{"type":"string"},"type":"array"}},"type":"object"},"SourceConstraints":{"additionalProperties":false,"properties":{"allowSystemChanges":{"default":false,"type":"boolean"},"allowedTargetPaths":{"default":[],"items":{"type":"string"},"type":"array"},"encryption":{"anyOf":[{"$ref":"#/definitions/EncryptionConstraint"},{"type":"null"}],"description":"Encryption requirements imposed on files delivered by this source."},"noScripts":{"default":true,"type":"boolean"},"noSecretsRead":{"default":true,"type":"boolean"},"requireSignedCommits":{"default":false,"description":"Require that the HEAD commit in this source's git repo has a valid GPG or SSH signature. Subscribers can bypass with `security.allow-unsigned`.","type":"boolean"}},"type":"object"}},"properties":{"policy":{"$ref":"#/definitions/ConfigSourcePolicy","default":{"constraints":{"allowSystemChanges":false,"allowedTargetPaths":[],"noScripts":true,"noSecretsRead":true,"requireSignedCommits":false},"locked":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"optional":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"recommended":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"required":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}}},"provides":{"$ref":"#/definitions/ConfigSourceProvides","default":{"modules":[],"platformProfiles":{},"profileDetails":[],"profiles":[]}}},"title":"ConfigSourceSpec","type":"object"}
```


<!-- /cfgd:skill:source -->
