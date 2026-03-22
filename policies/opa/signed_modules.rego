# Enforce that Module CRDs have cosign signatures configured.
#
# Accepts either static key verification (publicKey) or keyless verification
# (keyless + certificateIdentity + certificateOidcIssuer).
#
# Usage: conftest test --policy policies/opa/ <module-crd.yaml>

package cfgd.module.signed

import rego.v1

violation contains msg if {
	input.kind == "Module"
	not has_signature(input)
	msg := sprintf("Module '%s' must have a cosign signature (spec.signature.cosign with publicKey or keyless verification required)", [
		input.metadata.name,
	])
}

# Static key verification: publicKey is set
has_signature(module) if {
	module.spec.signature.cosign.publicKey != ""
}

# Keyless verification: keyless is true with certificate identity and OIDC issuer
has_signature(module) if {
	module.spec.signature.cosign.keyless == true
	module.spec.signature.cosign.certificateIdentity != ""
	module.spec.signature.cosign.certificateOidcIssuer != ""
}
