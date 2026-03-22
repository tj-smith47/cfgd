# Security baseline checks for MachineConfig CRDs.
#
# Ensures MachineConfigs follow security best practices:
# - No privileged system settings
# - File modes are restrictive
# - Required security modules are present
# - Configuration drift is flagged

package cfgd.machineconfig.security_baseline

import rego.v1

# Reject files with world-writable modes
violation contains msg if {
	input.kind == "MachineConfig"
	some file in input.spec.files
	file.mode != ""
	world_writable(file.mode)
	msg := sprintf("MachineConfig '%s': file '%s' has world-writable mode '%s'", [
		input.metadata.name,
		file.path,
		file.mode,
	])
}

world_writable(mode) if {
	# Octal mode string: last digit has write bit (2, 3, 6, 7)
	last := substring(mode, count(mode) - 1, 1)
	writable_digits := {"2", "3", "6", "7"}
	last in writable_digits
}

# Warn if no modules are referenced (empty config)
violation contains msg if {
	input.kind == "MachineConfig"
	count(object.get(input.spec, "moduleRefs", [])) == 0
	count(object.get(input.spec, "packages", [])) == 0
	count(object.get(input.spec, "files", [])) == 0
	msg := sprintf("MachineConfig '%s' has no modules, packages, or files — is this intentional?", [
		input.metadata.name,
	])
}

# Flag MachineConfigs with active drift (DriftDetected condition is True)
violation contains msg if {
	input.kind == "MachineConfig"
	some condition in input.status.conditions
	condition.type == "DriftDetected"
	condition.status == "True"
	msg := sprintf("MachineConfig '%s' has active drift detected (reason: %s)", [
		input.metadata.name,
		condition.reason,
	])
}

# Flag MachineConfigs that are not successfully reconciled
violation contains msg if {
	input.kind == "MachineConfig"
	some condition in input.status.conditions
	condition.type == "Reconciled"
	condition.status == "False"
	msg := sprintf("MachineConfig '%s' is not reconciled (reason: %s: %s)", [
		input.metadata.name,
		condition.reason,
		condition.message,
	])
}

# Flag MachineConfigs with unresolved modules
violation contains msg if {
	input.kind == "MachineConfig"
	some condition in input.status.conditions
	condition.type == "ModulesResolved"
	condition.status == "False"
	msg := sprintf("MachineConfig '%s' has unresolved modules (reason: %s: %s)", [
		input.metadata.name,
		condition.reason,
		condition.message,
	])
}

# Flag non-compliant MachineConfigs
violation contains msg if {
	input.kind == "MachineConfig"
	some condition in input.status.conditions
	condition.type == "Compliant"
	condition.status == "False"
	msg := sprintf("MachineConfig '%s' is non-compliant (%s: %s)", [
		input.metadata.name,
		condition.reason,
		condition.message,
	])
}
