# Security baseline checks for ClusterConfigPolicy CRDs.
#
# Ensures cluster-wide policies follow security best practices:
# - Unsigned modules are not allowed
# - Trusted registries list is not empty
# - Namespace selector is configured (not wildcard)

package cfgd.clusterconfigpolicy.baseline

import rego.v1

# Reject policies that allow unsigned modules
violation contains msg if {
	input.kind == "ClusterConfigPolicy"
	input.spec.security.allowUnsigned == true
	msg := sprintf("ClusterConfigPolicy '%s' allows unsigned modules (spec.security.allowUnsigned must be false)", [
		input.metadata.name,
	])
}

# Warn if no trusted registries are configured
violation contains msg if {
	input.kind == "ClusterConfigPolicy"
	registries := object.get(input.spec.security, "trustedRegistries", [])
	count(registries) == 0
	msg := sprintf("ClusterConfigPolicy '%s' has no trusted registries configured (spec.security.trustedRegistries is empty)", [
		input.metadata.name,
	])
}

# Warn if namespace selector matches everything (empty matchLabels and no matchExpressions)
violation contains msg if {
	input.kind == "ClusterConfigPolicy"
	selector := input.spec.namespaceSelector
	count(object.get(selector, "matchLabels", {})) == 0
	count(object.get(selector, "matchExpressions", [])) == 0
	msg := sprintf("ClusterConfigPolicy '%s' has a wildcard namespaceSelector — consider restricting to specific namespaces", [
		input.metadata.name,
	])
}

# Flag non-enforced ClusterConfigPolicies
violation contains msg if {
	input.kind == "ClusterConfigPolicy"
	some condition in input.status.conditions
	condition.type == "Enforced"
	condition.status == "False"
	msg := sprintf("ClusterConfigPolicy '%s' is not enforced (%s: %s)", [
		input.metadata.name,
		condition.reason,
		condition.message,
	])
}
