# Enforce that Module CRDs only reference trusted OCI registries.
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

any_prefix_match(value, prefixes) if {
	some prefix in prefixes
	startswith(value, prefix)
}

# Deny if ociArtifact is missing entirely when policy requires it
violation contains msg if {
	input.kind == "Module"
	not input.spec.ociArtifact
	data.require_oci_artifact == true
	msg := sprintf("Module '%s' must specify an ociArtifact", [input.metadata.name])
}
