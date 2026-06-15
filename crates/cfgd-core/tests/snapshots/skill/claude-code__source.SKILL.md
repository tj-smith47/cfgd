---
name: cfgd-source
description: Investigate thoroughly and author a complete, validated cfgd Source resource.
user-invocable: true
cfgd-version: <CFGD_VERSION>
cfgd-min-version: <CFGD_MIN_VERSION>
---

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
{"$schema":"http://json-schema.org/draft-07/schema#","title":"ConfigSourceSpec","type":"object","properties":{"policy":{"default":{"constraints":{"allowSystemChanges":false,"allowedTargetPaths":[],"noScripts":true,"noSecretsRead":true,"requireSignedCommits":false},"locked":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"optional":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"recommended":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"required":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"allOf":[{"$ref":"#/definitions/ConfigSourcePolicy"}]},"provides":{"default":{"modules":[],"platformProfiles":{},"profileDetails":[],"profiles":[]},"allOf":[{"$ref":"#/definitions/ConfigSourceProvides"}]}},"additionalProperties":false,"definitions":{"AptSpec":{"type":"object","properties":{"file":{"default":null,"type":["string","null"]},"packages":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"BrewSpec":{"type":"object","properties":{"casks":{"default":[],"type":"array","items":{"type":"string"}},"file":{"default":null,"type":["string","null"]},"formulae":{"default":[],"type":"array","items":{"type":"string"}},"taps":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"CargoSpec":{"description":"Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`) and object form (`cargo: { file: Cargo.toml, packages: [...] }`) via the shared `list_or_struct` deserializer on the `PackagesSpec::cargo` field.","type":"object","properties":{"file":{"type":["string","null"]},"packages":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"ConfigSourcePolicy":{"type":"object","properties":{"constraints":{"default":{"allowSystemChanges":false,"allowedTargetPaths":[],"noScripts":true,"noSecretsRead":true,"requireSignedCommits":false},"allOf":[{"$ref":"#/definitions/SourceConstraints"}]},"locked":{"default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"allOf":[{"$ref":"#/definitions/PolicyItems"}]},"optional":{"default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"allOf":[{"$ref":"#/definitions/PolicyItems"}]},"recommended":{"default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"allOf":[{"$ref":"#/definitions/PolicyItems"}]},"required":{"default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"allOf":[{"$ref":"#/definitions/PolicyItems"}]}},"additionalProperties":false},"ConfigSourceProfileEntry":{"description":"Detailed profile entry in a ConfigSource manifest. When present, provides richer info than the flat `profiles` list.","type":"object","required":["name"],"properties":{"description":{"default":null,"type":["string","null"]},"inherits":{"default":[],"type":"array","items":{"type":"string"}},"name":{"type":"string"},"path":{"default":null,"type":["string","null"]}},"additionalProperties":false},"ConfigSourceProvides":{"type":"object","properties":{"modules":{"default":[],"type":"array","items":{"type":"string"}},"platformProfiles":{"default":{},"type":"object","additionalProperties":{"type":"string"}},"profileDetails":{"default":[],"type":"array","items":{"$ref":"#/definitions/ConfigSourceProfileEntry"}},"profiles":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"CustomManagerSpec":{"type":"object","required":["check","install","listInstalled","name","uninstall"],"properties":{"check":{"type":"string"},"install":{"type":"string"},"listInstalled":{"type":"string"},"name":{"type":"string"},"packages":{"default":[],"type":"array","items":{"type":"string"}},"uninstall":{"type":"string"},"update":{"default":null,"type":["string","null"]}},"additionalProperties":false},"EncryptionConstraint":{"description":"Encryption constraint applied to files from a config source.","type":"object","properties":{"backend":{"description":"If set, restrict which backend is acceptable.","type":["string","null"]},"mode":{"description":"If set, restrict which encryption mode is acceptable.","anyOf":[{"$ref":"#/definitions/EncryptionMode"},{"type":"null"}]},"requiredTargets":{"description":"Glob patterns or explicit paths that must be encrypted.","default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"EncryptionMode":{"description":"Controls when encryption is required for a managed file.","oneOf":[{"description":"File must be encrypted when stored in the repository.","type":"string","enum":["InRepo"]},{"description":"File must always be encrypted, including at rest on disk.","type":"string","enum":["Always"]}]},"EncryptionSpec":{"description":"Encryption settings for a managed file.","type":"object","required":["backend"],"properties":{"backend":{"description":"The encryption backend to use (e.g. \"sops\", \"age\").","type":"string"},"mode":{"description":"When encryption must be enforced. Defaults to `InRepo`.","default":"InRepo","allOf":[{"$ref":"#/definitions/EncryptionMode"}]}},"additionalProperties":false},"EnvVar":{"type":"object","required":["name","value"],"properties":{"name":{"type":"string"},"value":{"type":"string"}}},"FileStrategy":{"description":"File deployment strategy.","oneOf":[{"description":"Create a symbolic link from target to source (default).","type":"string","enum":["Symlink"]},{"description":"Copy source content to target.","type":"string","enum":["Copy"]},{"description":"Render a Tera template and write the output (auto-selected for .tera files).","type":"string","enum":["Template"]},{"description":"Create a hard link from target to source.","type":"string","enum":["Hardlink"]}]},"FlatpakSpec":{"type":"object","properties":{"packages":{"default":[],"type":"array","items":{"type":"string"}},"remote":{"default":null,"type":["string","null"]}},"additionalProperties":false},"ManagedFileSpec":{"type":"object","required":["source","target"],"properties":{"encryption":{"description":"Encryption settings for this file.","anyOf":[{"$ref":"#/definitions/EncryptionSpec"},{"type":"null"}]},"permissions":{"description":"Unix permission bits (e.g. \"600\", \"644\") to apply after deployment.","type":["string","null"]},"private":{"description":"When true, the source file is local-only: auto-added to .gitignore, silently skipped on machines where it doesn't exist.","type":"boolean"},"source":{"type":"string"},"strategy":{"description":"Per-file deployment strategy override. If None, uses the global default.","anyOf":[{"$ref":"#/definitions/FileStrategy"},{"type":"null"}]},"target":{"type":"string"}},"additionalProperties":false},"NpmSpec":{"type":"object","properties":{"file":{"default":null,"type":["string","null"]},"global":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"PackagesSpec":{"type":"object","properties":{"apk":{"default":[],"type":"array","items":{"type":"string"}},"apt":{"default":null,"anyOf":[{"$ref":"#/definitions/AptSpec"},{"type":"null"}]},"brew":{"default":null,"anyOf":[{"$ref":"#/definitions/BrewSpec"},{"type":"null"}]},"cargo":{"default":null,"anyOf":[{"$ref":"#/definitions/CargoSpec"},{"type":"null"}]},"chocolatey":{"default":[],"type":"array","items":{"type":"string"}},"custom":{"default":[],"type":"array","items":{"$ref":"#/definitions/CustomManagerSpec"}},"dnf":{"default":[],"type":"array","items":{"type":"string"}},"flatpak":{"default":null,"anyOf":[{"$ref":"#/definitions/FlatpakSpec"},{"type":"null"}]},"go":{"default":[],"type":"array","items":{"type":"string"}},"nix":{"default":[],"type":"array","items":{"type":"string"}},"npm":{"default":null,"anyOf":[{"$ref":"#/definitions/NpmSpec"},{"type":"null"}]},"pacman":{"default":[],"type":"array","items":{"type":"string"}},"pipx":{"default":[],"type":"array","items":{"type":"string"}},"pkg":{"default":[],"type":"array","items":{"type":"string"}},"scoop":{"default":[],"type":"array","items":{"type":"string"}},"snap":{"default":null,"anyOf":[{"$ref":"#/definitions/SnapSpec"},{"type":"null"}]},"winget":{"default":[],"type":"array","items":{"type":"string"}},"yum":{"default":[],"type":"array","items":{"type":"string"}},"zypper":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"PolicyItems":{"type":"object","properties":{"aliases":{"default":[],"type":"array","items":{"$ref":"#/definitions/ShellAlias"}},"env":{"default":[],"type":"array","items":{"$ref":"#/definitions/EnvVar"}},"files":{"default":[],"type":"array","items":{"$ref":"#/definitions/ManagedFileSpec"}},"modules":{"default":[],"type":"array","items":{"type":"string"}},"packages":{"default":null,"anyOf":[{"$ref":"#/definitions/PackagesSpec"},{"type":"null"}]},"profiles":{"default":[],"type":"array","items":{"type":"string"}},"secrets":{"default":[],"type":"array","items":{"$ref":"#/definitions/SecretSpec"}},"system":{"default":{},"type":"object","additionalProperties":true}},"additionalProperties":false},"SecretSpec":{"type":"object","required":["source"],"properties":{"backend":{"type":["string","null"]},"envs":{"type":["array","null"],"items":{"type":"string"}},"source":{"type":"string"},"target":{"type":["string","null"]},"template":{"type":["string","null"]}},"additionalProperties":false},"ShellAlias":{"type":"object","required":["command","name"],"properties":{"command":{"type":"string"},"name":{"type":"string"}}},"SnapSpec":{"type":"object","properties":{"classic":{"default":[],"type":"array","items":{"type":"string"}},"packages":{"default":[],"type":"array","items":{"type":"string"}}},"additionalProperties":false},"SourceConstraints":{"type":"object","properties":{"allowSystemChanges":{"default":false,"type":"boolean"},"allowedTargetPaths":{"default":[],"type":"array","items":{"type":"string"}},"encryption":{"description":"Encryption requirements imposed on files delivered by this source.","anyOf":[{"$ref":"#/definitions/EncryptionConstraint"},{"type":"null"}]},"noScripts":{"default":true,"type":"boolean"},"noSecretsRead":{"default":true,"type":"boolean"},"requireSignedCommits":{"description":"Require that the HEAD commit in this source's git repo has a valid GPG or SSH signature. Subscribers can bypass with `security.allow-unsigned`.","default":false,"type":"boolean"}},"additionalProperties":false}}}
```

