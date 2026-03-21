# Upstream Kubernetes Work

Deferred until after adoption. These items require v1 CRD graduation complete + months of production usage demonstrating the annotation-driven pattern works. They are proposals to upstream Kubernetes, not cfgd implementation work.

---

## CRD versioning

Trigger: 3+ months production usage with stable schema (no breaking CRD field changes for 1 month). Do not start before that threshold — premature graduation creates conversion debt.

- [ ] Conversion webhook for v1alpha1 → v1beta1
- [ ] Both versions served simultaneously, v1beta1 as storage version
- [ ] Migration runbook: deploy, convert on read, storage migration, remove v1alpha1
- [ ] Graduation criteria documented and gates enforced in CI

## Upstream KEPs

Trigger: v1 CRD graduation complete + months of production usage demonstrating the annotation-driven pattern works.

- [ ] KEP: `spec.modules[].moduleRef` pod spec field — native PodSpec field for declaring module dependencies, replaces `cfgd.io/modules` annotation
- [ ] KEP: `cfgdModule:` volume type — native volume type alongside `configMap:` and `secret:`
- [ ] KEP: `kubectl debug --module` flag — extend `kubectl debug` for module injection into ephemeral debug containers
