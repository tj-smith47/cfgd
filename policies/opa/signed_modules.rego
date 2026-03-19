# Enforce that Module CRDs have cosign signatures configured.
#
# Usage: conftest test --policy policies/opa/ <module-crd.yaml>

package cfgd.module.signed

import rego.v1

violation contains msg if {
	input.kind == "Module"
	not has_signature(input)
	msg := sprintf("Module '%s' must have a cosign signature (spec.signature.cosign.publicKey required)", [
		input.metadata.name,
	])
}

has_signature(module) if {
	module.spec.signature.cosign.publicKey != ""
}
