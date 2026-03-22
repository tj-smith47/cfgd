# Baseline checks for DriftAlert CRDs.
#
# Ensures drift alerts are properly managed:
# - High/Critical severity alerts must be acknowledged
# - Resolved alerts should have a resolvedAt timestamp
# - Condition lifecycle is tracked

package cfgd.driftalert.baseline

import rego.v1

# High/Critical severity alerts must be acknowledged
violation contains msg if {
	input.kind == "DriftAlert"
	input.spec.severity in {"High", "Critical"}
	not has_condition_true(input, "Acknowledged")
	msg := sprintf("DriftAlert '%s' has %s severity but is not acknowledged", [
		input.metadata.name,
		input.spec.severity,
	])
}

# Critical alerts should be escalated
violation contains msg if {
	input.kind == "DriftAlert"
	input.spec.severity == "Critical"
	not has_condition_true(input, "Escalated")
	msg := sprintf("DriftAlert '%s' has Critical severity but has not been escalated", [
		input.metadata.name,
	])
}

# Resolved condition should have a resolvedAt timestamp
violation contains msg if {
	input.kind == "DriftAlert"
	has_condition_true(input, "Resolved")
	not input.status.resolvedAt
	msg := sprintf("DriftAlert '%s' has Resolved condition but missing status.resolvedAt timestamp", [
		input.metadata.name,
	])
}

# Alert must have at least one drift detail
violation contains msg if {
	input.kind == "DriftAlert"
	count(object.get(input.spec, "driftDetails", [])) == 0
	msg := sprintf("DriftAlert '%s' has no drift details", [
		input.metadata.name,
	])
}

has_condition_true(resource, condition_type) if {
	some condition in resource.status.conditions
	condition.type == condition_type
	condition.status == "True"
}
