package main

import (
	"context"
	"fmt"
	"regexp"
	"strings"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/resource/composed"
	"github.com/crossplane/function-sdk-go/response"
)

// Function fans out a TeamConfig XR into per-member MachineConfig CRDs and
// enforcement ConfigPolicy CRDs for the cfgd operator.
type Function struct {
	fnv1.UnimplementedFunctionRunnerServiceServer
	log logging.Logger
}

func (f *Function) RunFunction(_ context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	f.log.Info("Running function-cfgd")
	rsp := response.To(req, response.DefaultTTL)

	oxr, err := request.GetObservedCompositeResource(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot get observed composite resource"))
		return rsp, nil
	}

	spec := asMap(oxr.Resource.Object["spec"])
	if spec == nil {
		response.Fatal(rsp, errors.New("spec is missing"))
		return rsp, nil
	}

	team := asString(spec["team"])
	if team == "" {
		response.Fatal(rsp, errors.New("spec.team is required"))
		return rsp, nil
	}

	defaultProfile := asString(spec["profile"])

	membersRaw, ok := spec["members"].([]interface{})
	if !ok {
		response.Fatal(rsp, errors.New("spec.members must be an array"))
		return rsp, nil
	}

	policy := asMap(spec["policy"])
	required := asMap(policy["required"])
	recommended := asMap(policy["recommended"])
	locked := asMap(policy["locked"])

	// Collect resources from all policy tiers for MachineConfig population.
	// Locked files take dedup precedence, then required, then recommended.
	allPackages := unionStrings(
		flattenPackages(locked),
		flattenPackages(required),
		flattenPackages(recommended),
	)
	allFiles := collectFiles(locked, required, recommended)
	allSettings := mergeStringMaps(
		flattenSystemSettings(locked),
		flattenSystemSettings(required),
		flattenSystemSettings(recommended),
	)

	// Collect modules from spec.modules and policy tiers.
	specModules := extractModuleNames(spec["modules"])
	requiredModules := asStringSlice(policy["requiredModules"])
	recommendedModules := asStringSlice(policy["recommendedModules"])
	allModuleNames := unionStrings(specModules, requiredModules, recommendedModules)

	desired, err := request.GetDesiredComposedResources(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot get desired composed resources"))
		return rsp, nil
	}

	// Generate a MachineConfig for each team member.
	memberCount := 0
	for _, memberRaw := range membersRaw {
		member := asMap(memberRaw)
		username := asString(member["username"])
		if username == "" {
			continue
		}

		hostname := asString(member["hostname"])
		if hostname == "" {
			// Placeholder until the device checks in and reports its real hostname.
			hostname = fmt.Sprintf("pending-%s", sanitizeName(username))
		}

		profile := asString(member["profile"])
		if profile == "" {
			profile = defaultProfile
		}
		if profile == "" {
			response.Fatal(rsp, errors.Errorf("member %q has no profile and no team default profile is set", username))
			return rsp, nil
		}

		mc := composed.New()
		mc.SetAPIVersion("cfgd.io/v1alpha1")
		mc.SetKind("MachineConfig")
		mc.SetGenerateName(sanitizeName(team+"-"+username) + "-")
		mc.SetLabels(map[string]string{
			"cfgd.io/team":     team,
			"cfgd.io/username": username,
		})

		mcSpec := map[string]interface{}{
			"hostname": hostname,
			"profile":  profile,
		}
		if len(allModuleNames) > 0 {
			mcSpec["moduleRefs"] = buildModuleRefs(allModuleNames, requiredModules)
		}
		if len(allPackages) > 0 {
			mcSpec["packages"] = toInterfaceSlice(allPackages)
		}
		if len(allFiles) > 0 {
			mcSpec["files"] = allFiles
		}
		if len(allSettings) > 0 {
			mcSpec["systemSettings"] = allSettings
		}
		mc.Object["spec"] = mcSpec

		desired[resource.Name("mc-"+sanitizeName(username))] = &resource.DesiredComposed{Resource: mc}
		memberCount++
		f.log.Info("Generated MachineConfig", "member", username, "profile", profile)
	}

	// Generate a ConfigPolicy for the required tier.
	generateConfigPolicy(desired, team, "required", required, requiredModules, f.log)

	// Generate a ConfigPolicy for the locked tier.
	generateConfigPolicy(desired, team, "locked", locked, nil, f.log)

	if err := response.SetDesiredComposedResources(rsp, desired); err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot set desired composed resources"))
		return rsp, nil
	}

	response.Normalf(rsp, "Generated %d MachineConfig(s) for team %q", memberCount, team)
	return rsp, nil
}

// generateConfigPolicy creates a ConfigPolicy composed resource for a policy
// tier if it has any enforceable packages, settings, or required modules.
func generateConfigPolicy(
	desired map[resource.Name]*resource.DesiredComposed,
	team, tier string,
	tierData map[string]interface{},
	requiredModules []string,
	log logging.Logger,
) {
	pkgs := flattenPackages(tierData)
	settings := flattenSystemSettings(tierData)
	if len(pkgs) == 0 && len(settings) == 0 && len(requiredModules) == 0 {
		return
	}

	cp := composed.New()
	cp.SetAPIVersion("cfgd.io/v1alpha1")
	cp.SetKind("ConfigPolicy")
	cp.SetGenerateName(sanitizeName(team+"-"+tier) + "-")
	cp.SetLabels(map[string]string{
		"cfgd.io/team": team,
		"cfgd.io/tier": tier,
	})

	cpSpec := map[string]interface{}{
		"name": fmt.Sprintf("%s-%s", team, tier),
		"targetSelector": map[string]interface{}{
			"cfgd.io/team": team,
		},
	}
	if len(requiredModules) > 0 {
		cpSpec["requiredModules"] = toInterfaceSlice(requiredModules)
	}
	if len(pkgs) > 0 {
		cpSpec["packages"] = toInterfaceSlice(pkgs)
	}
	if len(settings) > 0 {
		cpSpec["settings"] = settings
	}
	cp.Object["spec"] = cpSpec

	desired[resource.Name("policy-"+tier)] = &resource.DesiredComposed{Resource: cp}
	log.Info("Generated ConfigPolicy", "tier", tier, "packages", len(pkgs), "settings", len(settings), "requiredModules", len(requiredModules))
}

// ---------------------------------------------------------------------------
// Policy extraction helpers
// ---------------------------------------------------------------------------

// flattenPackages extracts all package names from a policy tier's "packages"
// field. Handles both direct arrays (cargo, pipx, dnf) and nested objects
// with sub-arrays (brew.formulae, brew.casks, apt.install, npm.global).
func flattenPackages(tier map[string]interface{}) []string {
	if tier == nil {
		return nil
	}
	pkgMap := asMap(tier["packages"])
	if pkgMap == nil {
		return nil
	}

	var result []string
	for _, managerVal := range pkgMap {
		switch v := managerVal.(type) {
		case []interface{}:
			for _, item := range v {
				if s := asString(item); s != "" {
					result = append(result, s)
				}
			}
		case map[string]interface{}:
			for _, subVal := range v {
				if arr, ok := subVal.([]interface{}); ok {
					for _, item := range arr {
						if s := asString(item); s != "" {
							result = append(result, s)
						}
					}
				}
			}
		}
	}
	return result
}

// collectFiles extracts file specs from policy tiers and converts them to
// MachineConfig-compatible file objects. Tiers are passed in priority order
// (first wins on duplicate target paths).
func collectFiles(tiers ...map[string]interface{}) []interface{} {
	seen := make(map[string]bool)
	var result []interface{}

	for _, tier := range tiers {
		if tier == nil {
			continue
		}
		filesRaw, ok := tier["files"].([]interface{})
		if !ok {
			continue
		}

		for _, fileRaw := range filesRaw {
			file := asMap(fileRaw)
			if file == nil {
				continue
			}
			target := asString(file["target"])
			if target == "" {
				continue
			}
			if seen[target] {
				continue
			}
			seen[target] = true

			fileSpec := map[string]interface{}{
				"path": target,
				"mode": "0644",
			}
			if source := asString(file["source"]); source != "" {
				fileSpec["source"] = source
			}
			if content := asString(file["content"]); content != "" {
				fileSpec["content"] = content
			}
			if mode := asString(file["mode"]); mode != "" {
				fileSpec["mode"] = mode
			}
			result = append(result, fileSpec)
		}
	}
	return result
}

// flattenSystemSettings recursively flattens a tier's "system" field into a
// flat map of dot-delimited keys to string values.
func flattenSystemSettings(tier map[string]interface{}) map[string]interface{} {
	if tier == nil {
		return nil
	}
	systemMap := asMap(tier["system"])
	if systemMap == nil {
		return nil
	}

	result := make(map[string]interface{})
	flattenMap("", systemMap, result)
	return result
}

func flattenMap(prefix string, m map[string]interface{}, result map[string]interface{}) {
	for key, val := range m {
		fullKey := key
		if prefix != "" {
			fullKey = prefix + "." + key
		}
		switch v := val.(type) {
		case map[string]interface{}:
			flattenMap(fullKey, v, result)
		default:
			result[fullKey] = fmt.Sprintf("%v", v)
		}
	}
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

func asMap(v interface{}) map[string]interface{} {
	if v == nil {
		return nil
	}
	m, ok := v.(map[string]interface{})
	if !ok {
		return nil
	}
	return m
}

func asString(v interface{}) string {
	if v == nil {
		return ""
	}
	s, ok := v.(string)
	if !ok {
		return fmt.Sprintf("%v", v)
	}
	return s
}

// asStringSlice extracts a []string from an interface{} that may be []interface{}.
func asStringSlice(v interface{}) []string {
	if v == nil {
		return nil
	}
	arr, ok := v.([]interface{})
	if !ok {
		return nil
	}
	var result []string
	for _, item := range arr {
		if s := asString(item); s != "" {
			result = append(result, s)
		}
	}
	return result
}

// extractModuleNames extracts module names from spec.modules, which is an
// array of objects with a "name" field.
func extractModuleNames(v interface{}) []string {
	if v == nil {
		return nil
	}
	arr, ok := v.([]interface{})
	if !ok {
		return nil
	}
	var result []string
	for _, item := range arr {
		m := asMap(item)
		if name := asString(m["name"]); name != "" {
			result = append(result, name)
		}
	}
	return result
}

// buildModuleRefs creates moduleRefs entries for MachineConfig, marking which
// modules are required vs optional.
func buildModuleRefs(allModules, requiredModules []string) []interface{} {
	reqSet := make(map[string]bool, len(requiredModules))
	for _, m := range requiredModules {
		reqSet[m] = true
	}
	var refs []interface{}
	for _, name := range allModules {
		refs = append(refs, map[string]interface{}{
			"name":     name,
			"required": reqSet[name],
		})
	}
	return refs
}

// unionStrings deduplicates and concatenates string slices, preserving first
// occurrence order.
func unionStrings(slices ...[]string) []string {
	seen := make(map[string]bool)
	var result []string
	for _, s := range slices {
		for _, item := range s {
			if !seen[item] {
				seen[item] = true
				result = append(result, item)
			}
		}
	}
	return result
}

func toInterfaceSlice(ss []string) []interface{} {
	result := make([]interface{}, len(ss))
	for i, s := range ss {
		result[i] = s
	}
	return result
}

// mergeStringMaps merges maps with first-occurrence-wins semantics.
func mergeStringMaps(maps ...map[string]interface{}) map[string]interface{} {
	result := make(map[string]interface{})
	for _, m := range maps {
		for k, v := range m {
			if _, exists := result[k]; !exists {
				result[k] = v
			}
		}
	}
	if len(result) == 0 {
		return nil
	}
	return result
}

var invalidNameChars = regexp.MustCompile(`[^a-z0-9-]`)

// sanitizeName converts a string to a valid Kubernetes resource name fragment.
func sanitizeName(name string) string {
	name = strings.ToLower(name)
	name = invalidNameChars.ReplaceAllString(name, "-")
	name = strings.Trim(name, "-")
	if len(name) > 63 {
		name = name[:63]
	}
	return name
}
