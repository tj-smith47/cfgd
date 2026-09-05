---
name: cfgd-machineconfig
description: Author a complete, validated cfgd MachineConfig; use whenever creating or reworking a MachineConfig YAML.
user-invocable: true
cfgd-version: <CFGD_VERSION>
cfgd-min-version: <CFGD_MIN_VERSION>
---

<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd MachineConfig

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition.** Run `cfgd --version`. If cfgd is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>; if it is older than <CFGD_MIN_VERSION>, warn and take the fallback branch in steps 1 and 5.
1. **Enumerate every field.** Run `cfgd explain machineconfig -o json` once. The payload is the complete field list step 3 walks: every field, nested ones under `children`, each with `type`, `description` and `required`; its `location` is the path the finished file goes to. `cfgd explain machineconfig.<field>` (no `-o`) prints one field's docs readably. Fallback: the embedded schema below (stamped <CFGD_VERSION>).
2. **Research THIS subject before choosing values.** Check the subject's own docs, the package managers that ship it (for a tool), and community conventions. On the target machine, `<tool> --version` and the manager's own query (`brew info`, `apt-cache policy`, …) are live evidence and outrank recall. Put what you verified, and where, in the field's WHY comment; where you could not confirm a claim, say so there and in your reply.
3. **Decide include or omit for EVERY field from step 1, and write the WHY as a comment beside each included one.** Omit a field the subject does not use or whose value would equal the default; note a non-obvious omission in a comment too.
4. **Draft.** Declare every dependency the subject needs at run time, transitive ones included. Set a version floor only where a feature needs it, and say which. Gate platform-specific entries with `platforms`. Make each script step safe to re-run (`onlyIf` / `unless` / `creates` where the kind offers them, or a command that is itself idempotent), give it a `timeout`, and set `continueOnError: true` only where a failure must not abort the apply. Never write a credential into a value; a secret belongs in the profile's `spec.secrets`. No placeholders, no stub comments.
5. **Validate:** `cfgd machineconfig validate <file>` (`-` reads stdin; add `-o json` for a parseable report). A non-zero exit lists every error with its line; fix and re-run until it prints `✓ … is valid`. Fallback: check the draft by hand against the embedded schema (required keys, types, enums) and tell the user it was not machine-validated.
6. **Self-critique.** For each field in the step-1 list, name the evidence behind its value or its omission; a field you cannot account for goes back to step 2.

## Ground-truth examples

Validated resources of this kind, shown for shape and depth. A value like `you@example.com` is the example's placeholder; your draft carries the real one.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  # replace with the machine's own name and the namespace its team owns
  name: alice-workstation
  namespace: team-platform
spec:
  # Must match the machine's real hostname: this is how the agent claims the
  # resource, so a mismatch leaves the config unapplied rather than misapplied.
  hostname: alice-mbp
  profile: work
  moduleRefs:
    # required: the reconcile fails if this module cannot be resolved.
    - name: kubectl
      required: true
    # optional: a missing module is reported and skipped, which is what a
    # convenience tool wants.
    - name: terraform
      required: false
  packages:
    - name: ripgrep
    - name: fd
    # Pinned exactly: these two must match the cluster they talk to, and a
    # floating version is how a workstation drifts a minor ahead of the API
    # server it is administering.
    - name: kubectl
      version: "1.28.3"
    - name: terraform
      version: "1.6.0"
  files:
    # replace with your own internal hosts. A separate file rather than an edit
    # of /etc/hosts, so cfgd owns the whole file it writes.
    - path: /etc/hosts.local
      content: "10.0.1.5  internal.acme.com\n"
      mode: "0644"
  systemSettings:
    # Required by the local container runtime this workstation runs.
    net.ipv4.ip_forward: "1"
```

## Fallback schema (if cfgd is unavailable)

Generated against cfgd <CFGD_VERSION>. Live `cfgd explain machineconfig` is authoritative when present.

```json
{"$schema":"https://json-schema.org/draft-07/schema#","definitions":{"FileSpec":{"description":"A file the operator writes on a managed machine.","properties":{"content":{"description":"Literal file body, written as-is. Mutually exclusive with `source`.","type":["string","null"]},"mode":{"default":"0644","description":"Octal permission bits applied after the write. Default: `0644`.","type":"string"},"path":{"description":"Destination path on the machine. A leading `~` expands to the home directory of the user the agent runs as.","type":"string"},"source":{"description":"Path the body is read from instead of `content`, resolved against the machine's config directory.","type":["string","null"]}},"required":["path"],"type":"object"},"ModuleRef":{"description":"Reference to a module that should be installed on the machine.","properties":{"name":{"description":"Name of the cluster-scoped `Module` resource to install.","type":"string"},"required":{"default":false,"description":"Whether a failure to resolve or install this module fails the whole reconcile. Default: `false`, so a missing module is reported and the rest of the machine still converges.","type":"boolean"}},"required":["name"],"type":"object"},"PackageRef":{"description":"Reference to a package with optional version pin.","properties":{"name":{"description":"Package name as the machine's own package manager knows it.","type":"string"},"version":{"description":"Exact version to pin to. Omitted, whatever the manager currently offers is installed and left alone once present.","type":["string","null"]}},"required":["name"],"type":"object"}},"properties":{"files":{"default":[],"description":"Files to write on the machine, either inline or fetched from a source path.","items":{"$ref":"#/definitions/FileSpec"},"type":"array"},"hostname":{"description":"Hostname of the machine this document describes. The agent reconciles only the MachineConfig whose hostname matches its own.","type":"string"},"moduleRefs":{"default":[],"description":"Modules to install on the machine, each naming a cluster-scoped `Module` resource.","items":{"$ref":"#/definitions/ModuleRef"},"type":"array"},"packages":{"default":[],"description":"Packages to install on top of whatever the profile declares, each optionally version-pinned.","items":{"$ref":"#/definitions/PackageRef"},"type":"array"},"profile":{"description":"Name of the profile the machine reconciles against. Resolved on the machine, from its own config directory or from a subscribed source.","type":"string"},"systemSettings":{"additionalProperties":true,"default":{},"description":"System configurator settings to apply, keyed by `<configurator>.<setting>` (e.g. `sysctl.net.ipv4.ip_forward`). Empty, no system settings are reconciled.","type":"object"}},"required":["hostname","profile"],"title":"MachineConfigSpec","type":"object"}
```

