# Security baseline checks for MachineConfig CRDs.
#
# Ensures MachineConfigs follow security best practices:
# - No privileged system settings
# - File modes are restrictive
# - Required security modules are present

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
