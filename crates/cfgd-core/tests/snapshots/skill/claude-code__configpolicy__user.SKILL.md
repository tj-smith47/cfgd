---
name: cfgd-configpolicy
description: Author a complete, validated cfgd ConfigPolicy; use whenever creating or reworking a ConfigPolicy YAML.
user-invocable: true
cfgd-version: <CFGD_VERSION>
cfgd-min-version: <CFGD_MIN_VERSION>
---

<!-- cfgd-version: <CFGD_VERSION> · cfgd-min-version: <CFGD_MIN_VERSION> -->

# Author a high-quality cfgd ConfigPolicy

Follow this protocol on every invocation. The quality bar is NOT "valid YAML". It is exhaustive field evaluation, external research, and a documented rationale for every choice. A box-checking resource (every field technically present, no investigation behind it) fails this bar. Evaluate EVERY field the kind exposes; for each, either populate it with a justified value or omit it only after investigating enough to conclude it does not apply. Ground every version, ordering, and strategy choice in evidence, never a guess.

## Protocol

0. **Precondition.** Run `cfgd --version`. If cfgd is absent, STOP and tell the user to install cfgd >= <CFGD_MIN_VERSION>; if it is older than <CFGD_MIN_VERSION>, warn that its field list may be incomplete and say so in the summary.
1. **Enumerate every field.** Run `cfgd explain configpolicy -o json` once. The payload is the complete field list step 3 walks: every field, nested ones under `children`, each with `type`, `description` and `required`; its `location` is the path the finished file goes to. `cfgd explain configpolicy.<field>` (no `-o`) prints one field's docs readably.
2. **Research THIS subject before choosing values.** Check the subject's own docs, the package managers that ship it (for a tool), and community conventions. On the target machine, `<tool> --version` and the manager's own query (`brew info`, `apt-cache policy`, …) are live evidence and outrank recall. Put what you verified, and where, in the field's WHY comment; where you could not confirm a claim, say so there and in your reply.
3. **Decide include or omit for EVERY field from step 1, and write the WHY as a comment beside each included one.** Omit a field the subject does not use or whose value would equal the default; note a non-obvious omission in a comment too.
4. **Draft.** Declare every dependency the subject needs at run time, transitive ones included. Set a version floor only where a feature needs it, and say which. Gate platform-specific entries with `platforms`. Make each script step safe to re-run (`onlyIf` / `unless` / `creates` where the kind offers them, or a command that is itself idempotent), give it a `timeout`, and set `continueOnError: true` only where a failure must not abort the apply. Never write a credential into a value; a secret belongs in the profile's `spec.secrets`. No placeholders, no stub comments.
5. **Validate:** `cfgd configpolicy validate <file>` (`-` reads stdin; add `-o json` for a parseable report). A non-zero exit lists every error with its line; fix and re-run until it prints `✓ … is valid`.
6. **Self-critique.** For each field in the step-1 list, name the evidence behind its value or its omission; a field you cannot account for goes back to step 2.

## Ground-truth examples

Validated resources of this kind, shown for shape and depth. A value like `you@example.com` is the example's placeholder; your draft carries the real one.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: k8s-node-baseline
  # replace with the namespace whose machines this policy governs — a
  # ConfigPolicy is namespaced, so it reaches no further than this
  namespace: team-platform
spec:
  requiredModules:
    # A node that cannot run one of these three is not a functioning node, so
    # every one is required rather than advisory.
    - name: containerd
      required: true
    - name: kubelet
      required: true
    - name: apparmor
      required: true
  packages:
    # kubelet shells out to both of these for service proxying and for
    # connection cleanup; neither is pulled in by the kubelet package itself.
    - name: socat
    - name: conntrack
    # Floors rather than pins: a node may run ahead of the floor, and pinning
    # here would fight the cluster's own upgrade cadence.
    - name: kubectl
      version: ">=1.28"
    - name: containerd
      version: ">=1.7"
  settings:
    # Both are hard preconditions for pod networking; kubelet refuses to start
    # without them on most CNI plugins.
    net.ipv4.ip_forward: "1"
    net.bridge.bridge-nf-call-iptables: "1"
  targetSelector:
    # Scoped by role rather than by name, so a node joining the pool inherits
    # the baseline without this file changing.
    matchLabels:
      cfgd.io/role: k8s-node
```

