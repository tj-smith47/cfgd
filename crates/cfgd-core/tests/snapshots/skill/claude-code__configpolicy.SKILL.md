---
name: cfgd-configpolicy
description: Investigate thoroughly and author a complete, validated cfgd ConfigPolicy resource.
user-invocable: true
cfgd-version: <CFGD_VERSION>
cfgd-min-version: <CFGD_MIN_VERSION>
---

<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd ConfigPolicy

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition — confirm the toolchain is usable.** Run `command -v cfgd`; if it is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>. Run `cfgd --version`; if it is older than <CFGD_MIN_VERSION>, warn and prefer the embedded fallback schema below.
1. **Enumerate every field for this kind (live-first, snapshot-fallback).** Run `cfgd explain configpolicy -o json` for the authoritative live schema, and `cfgd explain configpolicy.<field> -o json` to drill into nested objects. If cfgd is absent or older than the stamp, use the embedded fallback schema below (stamped <CFGD_VERSION>).
2. **Research best practices externally for THIS subject.** For each field, consult external best practice before settling a value: the tool's own docs, the package managers that ship it, and community conventions. Record what you verified and your confidence level when a source was unavailable. Prefer live evidence over training-knowledge recall, and state explicitly when you could not confirm a claim.
3. **For EVERY field, decide include OR omit, and justify with a WHY comment.** Box-checking is a failure; meeting the rubric above is the target.
4. **Draft thoroughly:** transitive deps explicit, version constraints set, platforms scoped, multi-step scripts idempotent (timeout + continueOnError), comments-as-specification.
5. **Validate against the schema:** `cfgd configpolicy validate <file>` — fix until clean (validate against the embedded snapshot if cfgd is unavailable).
6. **Self-critique against the rubric:** "Box-checking or thorough? Which field did I skip, and was that deliberate?" Iterate until the answer holds.

## Ground-truth examples

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: k8s-node-baseline
  namespace: team-platform
spec:
  requiredModules:
    - name: containerd
      required: true
    - name: kubelet
      required: true
    - name: apparmor
      required: true
  packages:
    - name: socat
    - name: conntrack
    - name: kubectl
      version: ">=1.28"
    - name: containerd
      version: ">=1.7"
  settings:
    net.ipv4.ip_forward: "1"
    net.bridge.bridge-nf-call-iptables: "1"
  targetSelector:
    matchLabels:
      cfgd.io/role: k8s-node
```

## Fallback schema (if cfgd is unavailable)

Generated against cfgd <CFGD_VERSION>. Live `cfgd explain configpolicy` is authoritative when present.

```json
{"$schema":"https://json-schema.org/draft/2020-12/schema","title":"ConfigPolicySpec","type":"object","properties":{"debugModules":{"description":"Modules staged as debug-only (CSI volume without volumeMount on declared containers).","type":"array","default":[],"items":{"$ref":"#/$defs/ModuleRef"}},"packages":{"type":"array","default":[],"items":{"$ref":"#/$defs/PackageRef"}},"requiredModules":{"type":"array","default":[],"items":{"$ref":"#/$defs/ModuleRef"}},"settings":{"type":"object","additionalProperties":true,"default":{}},"targetSelector":{"$ref":"#/$defs/LabelSelector","default":{"matchExpressions":[],"matchLabels":{}}}},"$defs":{"LabelSelector":{"description":"Kubernetes-style label selector with match_labels and match_expressions.","type":"object","properties":{"matchExpressions":{"type":"array","default":[],"items":{"$ref":"#/$defs/LabelSelectorRequirement"}},"matchLabels":{"type":"object","additionalProperties":{"type":"string"},"default":{}}}},"LabelSelectorRequirement":{"description":"A single requirement for label selector expressions.","type":"object","properties":{"key":{"type":"string"},"operator":{"$ref":"#/$defs/SelectorOperator"},"values":{"type":"array","default":[],"items":{"type":"string"}}},"required":["key","operator"]},"ModuleRef":{"description":"Reference to a module that should be installed on the machine.","type":"object","properties":{"name":{"type":"string"},"required":{"type":"boolean","default":false}},"required":["name"]},"PackageRef":{"description":"Reference to a package with optional version pin.","type":"object","properties":{"name":{"type":"string"},"version":{"type":["string","null"]}},"required":["name"]},"SelectorOperator":{"type":"string","enum":["In","NotIn","Exists","DoesNotExist"]}}}
```

