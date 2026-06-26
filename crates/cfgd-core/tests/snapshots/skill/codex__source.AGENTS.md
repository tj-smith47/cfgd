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
{"$schema":"https://json-schema.org/draft/2020-12/schema","title":"ConfigSourceSpec","type":"object","properties":{"policy":{"$ref":"#/$defs/ConfigSourcePolicy","default":{"constraints":{"allowSystemChanges":false,"allowedTargetPaths":[],"noScripts":true,"noSecretsRead":true,"requireSignedCommits":false},"locked":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"optional":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"recommended":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}},"required":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}}},"provides":{"$ref":"#/$defs/ConfigSourceProvides","default":{"modules":[],"platformProfiles":{},"profileDetails":[],"profiles":[]}}},"additionalProperties":false,"$defs":{"AptSpec":{"type":"object","properties":{"file":{"type":["string","null"],"default":null},"packages":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"BrewSpec":{"type":"object","properties":{"casks":{"type":"array","default":[],"items":{"type":"string"}},"file":{"type":["string","null"],"default":null},"formulae":{"type":"array","default":[],"items":{"type":"string"}},"taps":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"CargoSpec":{"description":"Cargo package spec. Supports both list form (`cargo: [bat, ripgrep]`)\nand object form (`cargo: { file: Cargo.toml, packages: [...] }`) via the\nshared `list_or_struct` deserializer on the `PackagesSpec::cargo` field.","type":"object","properties":{"file":{"type":["string","null"]},"packages":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"ConfigSourcePolicy":{"type":"object","properties":{"constraints":{"$ref":"#/$defs/SourceConstraints","default":{"allowSystemChanges":false,"allowedTargetPaths":[],"noScripts":true,"noSecretsRead":true,"requireSignedCommits":false}},"locked":{"$ref":"#/$defs/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"optional":{"$ref":"#/$defs/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"recommended":{"$ref":"#/$defs/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}},"required":{"$ref":"#/$defs/PolicyItems","default":{"aliases":[],"env":[],"files":[],"modules":[],"packages":null,"profiles":[],"secrets":[],"system":{}}}},"additionalProperties":false},"ConfigSourceProfileEntry":{"description":"Detailed profile entry in a ConfigSource manifest.\nWhen present, provides richer info than the flat `profiles` list.","type":"object","properties":{"description":{"type":["string","null"],"default":null},"inherits":{"type":"array","default":[],"items":{"type":"string"}},"name":{"type":"string"},"path":{"type":["string","null"],"default":null}},"additionalProperties":false,"required":["name"]},"ConfigSourceProvides":{"type":"object","properties":{"modules":{"type":"array","default":[],"items":{"type":"string"}},"platformProfiles":{"type":"object","additionalProperties":{"type":"string"},"default":{}},"profileDetails":{"type":"array","default":[],"items":{"$ref":"#/$defs/ConfigSourceProfileEntry"}},"profiles":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"CustomManagerSpec":{"type":"object","properties":{"check":{"type":"string"},"install":{"type":"string"},"listInstalled":{"type":"string"},"name":{"type":"string"},"packages":{"type":"array","default":[],"items":{"type":"string"}},"uninstall":{"type":"string"},"update":{"type":["string","null"],"default":null}},"additionalProperties":false,"required":["name","check","listInstalled","install","uninstall"]},"EncryptionConstraint":{"description":"Encryption constraint applied to files from a config source.","type":"object","properties":{"backend":{"description":"If set, restrict which backend is acceptable.","type":["string","null"]},"mode":{"description":"If set, restrict which encryption mode is acceptable.","anyOf":[{"$ref":"#/$defs/EncryptionMode"},{"type":"null"}]},"requiredTargets":{"description":"Glob patterns or explicit paths that must be encrypted.","type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"EncryptionMode":{"description":"Controls when encryption is required for a managed file.","oneOf":[{"description":"File must be encrypted when stored in the repository.","type":"string","const":"InRepo"},{"description":"File must always be encrypted, including at rest on disk.","type":"string","const":"Always"}]},"EncryptionSpec":{"description":"Encryption settings for a managed file.","type":"object","properties":{"backend":{"description":"The encryption backend to use (e.g. \"sops\", \"age\").","type":"string"},"mode":{"description":"When encryption must be enforced. Defaults to `InRepo`.","$ref":"#/$defs/EncryptionMode","default":"InRepo"}},"additionalProperties":false,"required":["backend"]},"EnvVar":{"type":"object","properties":{"name":{"type":"string"},"value":{"type":"string"}},"required":["name","value"]},"FileStrategy":{"description":"File deployment strategy.","oneOf":[{"description":"Create a symbolic link from target to source (default).","type":"string","const":"Symlink"},{"description":"Copy source content to target.","type":"string","const":"Copy"},{"description":"Render a Tera template and write the output (auto-selected for .tera files).","type":"string","const":"Template"},{"description":"Create a hard link from target to source.","type":"string","const":"Hardlink"}]},"FlatpakSpec":{"type":"object","properties":{"packages":{"type":"array","default":[],"items":{"type":"string"}},"remote":{"type":["string","null"],"default":null}},"additionalProperties":false},"ManagedFileSpec":{"type":"object","properties":{"encryption":{"description":"Encryption settings for this file.","anyOf":[{"$ref":"#/$defs/EncryptionSpec"},{"type":"null"}]},"permissions":{"description":"Unix permission bits (e.g. \"600\", \"644\") to apply after deployment.","type":["string","null"]},"private":{"description":"When true, the source file is local-only: auto-added to .gitignore,\nsilently skipped on machines where it doesn't exist.","type":"boolean"},"source":{"type":"string"},"strategy":{"description":"Per-file deployment strategy override. If None, uses the global default.","anyOf":[{"$ref":"#/$defs/FileStrategy"},{"type":"null"}]},"target":{"type":"string"}},"additionalProperties":false,"required":["source","target"]},"NpmSpec":{"type":"object","properties":{"file":{"type":["string","null"],"default":null},"global":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"PackagesSpec":{"type":"object","properties":{"apk":{"type":"array","default":[],"items":{"type":"string"}},"apt":{"anyOf":[{"$ref":"#/$defs/AptSpec"},{"type":"null"}],"default":null},"brew":{"anyOf":[{"$ref":"#/$defs/BrewSpec"},{"type":"null"}],"default":null},"cargo":{"anyOf":[{"$ref":"#/$defs/CargoSpec"},{"type":"null"}],"default":null},"chocolatey":{"type":"array","default":[],"items":{"type":"string"}},"custom":{"type":"array","default":[],"items":{"$ref":"#/$defs/CustomManagerSpec"}},"dnf":{"type":"array","default":[],"items":{"type":"string"}},"flatpak":{"anyOf":[{"$ref":"#/$defs/FlatpakSpec"},{"type":"null"}],"default":null},"go":{"type":"array","default":[],"items":{"type":"string"}},"nix":{"type":"array","default":[],"items":{"type":"string"}},"npm":{"anyOf":[{"$ref":"#/$defs/NpmSpec"},{"type":"null"}],"default":null},"pacman":{"type":"array","default":[],"items":{"type":"string"}},"pipx":{"type":"array","default":[],"items":{"type":"string"}},"pkg":{"type":"array","default":[],"items":{"type":"string"}},"scoop":{"type":"array","default":[],"items":{"type":"string"}},"snap":{"anyOf":[{"$ref":"#/$defs/SnapSpec"},{"type":"null"}],"default":null},"winget":{"type":"array","default":[],"items":{"type":"string"}},"yum":{"type":"array","default":[],"items":{"type":"string"}},"zypper":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"PolicyItems":{"type":"object","properties":{"aliases":{"type":"array","default":[],"items":{"$ref":"#/$defs/ShellAlias"}},"env":{"type":"array","default":[],"items":{"$ref":"#/$defs/EnvVar"}},"files":{"type":"array","default":[],"items":{"$ref":"#/$defs/ManagedFileSpec"}},"modules":{"type":"array","default":[],"items":{"type":"string"}},"packages":{"anyOf":[{"$ref":"#/$defs/PackagesSpec"},{"type":"null"}],"default":null},"profiles":{"type":"array","default":[],"items":{"type":"string"}},"secrets":{"type":"array","default":[],"items":{"$ref":"#/$defs/SecretSpec"}},"system":{"type":"object","additionalProperties":true,"default":{}}},"additionalProperties":false},"SecretSpec":{"type":"object","properties":{"backend":{"type":["string","null"]},"envs":{"type":["array","null"],"items":{"type":"string"}},"source":{"type":"string"},"target":{"type":["string","null"]},"template":{"type":["string","null"]}},"additionalProperties":false,"required":["source"]},"ShellAlias":{"type":"object","properties":{"command":{"type":"string"},"name":{"type":"string"}},"required":["name","command"]},"SnapSpec":{"type":"object","properties":{"classic":{"type":"array","default":[],"items":{"type":"string"}},"packages":{"type":"array","default":[],"items":{"type":"string"}}},"additionalProperties":false},"SourceConstraints":{"type":"object","properties":{"allowSystemChanges":{"type":"boolean","default":false},"allowedTargetPaths":{"type":"array","default":[],"items":{"type":"string"}},"encryption":{"description":"Encryption requirements imposed on files delivered by this source.","anyOf":[{"$ref":"#/$defs/EncryptionConstraint"},{"type":"null"}]},"noScripts":{"type":"boolean","default":true},"noSecretsRead":{"type":"boolean","default":true},"requireSignedCommits":{"description":"Require that the HEAD commit in this source's git repo has a valid\nGPG or SSH signature. Subscribers can bypass with `security.allow-unsigned`.","type":"boolean","default":false}},"additionalProperties":false}}}
```


<!-- /cfgd:skill:source -->
