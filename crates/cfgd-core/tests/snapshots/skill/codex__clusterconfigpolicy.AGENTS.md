<!-- cfgd:skill:clusterconfigpolicy -->
<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd ClusterConfigPolicy

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition — confirm the toolchain is usable.** Run `command -v cfgd`; if it is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>. Run `cfgd --version`; if it is older than <CFGD_MIN_VERSION>, warn and prefer the embedded fallback schema below.
1. **Enumerate every field for this kind (live-first, snapshot-fallback).** Run `cfgd explain clusterconfigpolicy -o json` for the authoritative live schema, and `cfgd explain clusterconfigpolicy.<field> -o json` to drill into nested objects. If cfgd is absent or older than the stamp, use the embedded fallback schema below (stamped <CFGD_VERSION>).
2. **Research best practices externally for THIS subject.** For each field, consult external best practice before settling a value: the tool's own docs, the package managers that ship it, and community conventions. Record what you verified and your confidence level when a source was unavailable. Prefer live evidence over training-knowledge recall, and state explicitly when you could not confirm a claim.
3. **For EVERY field, decide include OR omit, and justify with a WHY comment.** Box-checking is a failure; meeting the rubric above is the target.
4. **Draft thoroughly:** transitive deps explicit, version constraints set, platforms scoped, multi-step scripts idempotent (timeout + continueOnError), comments-as-specification.
5. **Validate against the schema:** `cfgd clusterconfigpolicy validate <file>` — fix until clean (validate against the embedded snapshot if cfgd is unavailable).
6. **Self-critique against the rubric:** "Box-checking or thorough? Which field did I skip, and was that deliberate?" Iterate until the answer holds.

## Ground-truth examples

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: org-baseline
spec:
  namespaceSelector:
    matchLabels:
      cfgd.io/managed: "true"
  requiredModules:
    - name: compliance-tools
      required: true
    - name: corp-certs
      required: true
  packages:
    - name: osquery
    - name: falco
      version: ">=0.37"
  settings:
    kernel.kptr_restrict: "2"
  security:
    trustedRegistries:
      - ghcr.io/acme-corp/
      - registry.acme.internal/
    allowUnsigned: false
```

## Fallback schema (if cfgd is unavailable)

Generated against cfgd <CFGD_VERSION>. Live `cfgd explain clusterconfigpolicy` is authoritative when present.

```json
{"$schema":"https://json-schema.org/draft-07/schema#","definitions":{"LabelSelector":{"description":"Kubernetes-style label selector with match_labels and match_expressions.","properties":{"matchExpressions":{"default":[],"description":"Set-based requirements a resource must satisfy, evaluated alongside `matchLabels`. Every requirement must hold.","items":{"$ref":"#/definitions/LabelSelectorRequirement"},"type":"array"},"matchLabels":{"additionalProperties":{"type":"string"},"default":{},"description":"Labels a resource must carry verbatim to match. Every entry must match; an empty map matches everything.","type":"object"}},"type":"object"},"LabelSelectorRequirement":{"description":"A single requirement for label selector expressions.","properties":{"key":{"description":"Label key this requirement tests.","type":"string"},"operator":{"$ref":"#/definitions/SelectorOperator","description":"How `values` is compared against the key: `In`, `NotIn`, `Exists`, or `DoesNotExist`."},"values":{"default":[],"description":"Values the key is tested against. Required for `In` and `NotIn`, and must be empty for `Exists` and `DoesNotExist`.","items":{"type":"string"},"type":"array"}},"required":["key","operator"],"type":"object"},"ModuleRef":{"description":"Reference to a module that should be installed on the machine.","properties":{"name":{"description":"Name of the cluster-scoped `Module` resource to install.","type":"string"},"required":{"default":false,"description":"Whether a failure to resolve or install this module fails the whole reconcile. Default: `false`, so a missing module is reported and the rest of the machine still converges.","type":"boolean"}},"required":["name"],"type":"object"},"PackageRef":{"description":"Reference to a package with optional version pin.","properties":{"name":{"description":"Package name as the machine's own package manager knows it.","type":"string"},"version":{"description":"Exact version to pin to. Omitted, whatever the manager currently offers is installed and left alone once present.","type":["string","null"]}},"required":["name"],"type":"object"},"SecurityPolicy":{"description":"Provenance rules a `Module` must satisfy to be admitted.","properties":{"allowUnsigned":{"default":false,"description":"Whether a module carrying an unsigned OCI artifact may be admitted. Default: `false` — creating any ClusterConfigPolicy at all starts rejecting unsigned modules.","type":"boolean"},"trustedRegistries":{"default":[],"description":"Registry prefixes a module artifact may be pulled from; a trailing `*` or `/` widens the match. Empty, any registry is accepted.","items":{"type":"string"},"type":"array"}},"type":"object"},"SelectorOperator":{"enum":["In","NotIn","Exists","DoesNotExist"],"type":"string"}},"properties":{"debugModules":{"default":[],"description":"Modules staged as debug-only across matching namespaces.","items":{"$ref":"#/definitions/ModuleRef"},"type":"array"},"namespaceSelector":{"$ref":"#/definitions/LabelSelector","default":{"matchExpressions":[],"matchLabels":{}},"description":"Select which namespaces this cluster policy applies to. Empty, it applies to every namespace in the cluster."},"packages":{"default":[],"description":"Packages every MachineConfig in a selected namespace must declare.","items":{"$ref":"#/definitions/PackageRef"},"type":"array"},"requiredModules":{"default":[],"description":"Modules every MachineConfig in a selected namespace must carry.","items":{"$ref":"#/definitions/ModuleRef"},"type":"array"},"security":{"$ref":"#/definitions/SecurityPolicy","default":{"allowUnsigned":false,"trustedRegistries":[]},"description":"Cluster-wide rules governing which modules may be admitted at all."},"settings":{"additionalProperties":true,"default":{},"description":"System settings every MachineConfig in a selected namespace must declare, keyed the same way as `MachineConfig.spec.systemSettings`.","type":"object"}},"title":"ClusterConfigPolicySpec","type":"object"}
```


<!-- /cfgd:skill:clusterconfigpolicy -->
