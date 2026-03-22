# Enforce that Module CRDs only reference trusted OCI registries.
# Also validates ClusterConfigPolicy spec.security.trustedRegistries entries.
#
# Usage: conftest test --policy policies/opa/ <module-crd.yaml>
# OPA Gatekeeper: load as ConstraintTemplate

package cfgd.module.trusted_registries

import rego.v1

# Default trusted registries — override via data.trusted_registries
default_trusted := [
	"ghcr.io/cfgd-org/",
	"registry.cfgd.io/",
]

trusted := object.get(data, "trusted_registries", default_trusted)

# Module: ociArtifact must reference a trusted registry
violation contains msg if {
	input.kind == "Module"
	artifact := input.spec.ociArtifact
	artifact != ""
	not any_prefix_match(artifact, trusted)
	msg := sprintf("Module '%s' references untrusted registry '%s'. Allowed: %v", [
		input.metadata.name,
		artifact,
		trusted,
	])
}

# Module: deny if ociArtifact is missing entirely when policy requires it
violation contains msg if {
	input.kind == "Module"
	not input.spec.ociArtifact
	data.require_oci_artifact == true
	msg := sprintf("Module '%s' must specify an ociArtifact", [input.metadata.name])
}

# ClusterConfigPolicy: trustedRegistries entries must be in the approved list
violation contains msg if {
	input.kind == "ClusterConfigPolicy"
	some registry in input.spec.security.trustedRegistries
	not any_prefix_match(registry, trusted)
	msg := sprintf("ClusterConfigPolicy '%s' includes untrusted registry '%s' in spec.security.trustedRegistries. Allowed prefixes: %v", [
		input.metadata.name,
		registry,
		trusted,
	])
}

any_prefix_match(value, prefixes) if {
	some prefix in prefixes
	startswith(value, prefix)
}
