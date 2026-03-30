package main

import (
	"context"
	"testing"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"google.golang.org/protobuf/types/known/structpb"
)

// ---------------------------------------------------------------------------
// Helper function tests
// ---------------------------------------------------------------------------

func TestFlattenPackages(t *testing.T) {
	tests := []struct {
		name string
		tier map[string]interface{}
		want int
	}{
		{
			name: "nil tier",
			tier: nil,
			want: 0,
		},
		{
			name: "empty packages",
			tier: map[string]interface{}{"packages": map[string]interface{}{}},
			want: 0,
		},
		{
			name: "brew formulae and casks",
			tier: map[string]interface{}{
				"packages": map[string]interface{}{
					"brew": map[string]interface{}{
						"formulae": []interface{}{"git-secrets", "pre-commit"},
						"casks":    []interface{}{"1password-cli"},
					},
				},
			},
			want: 3,
		},
		{
			name: "direct array managers",
			tier: map[string]interface{}{
				"packages": map[string]interface{}{
					"cargo": []interface{}{"bat", "fd"},
					"pipx":  []interface{}{"black"},
				},
			},
			want: 3,
		},
		{
			name: "mixed managers",
			tier: map[string]interface{}{
				"packages": map[string]interface{}{
					"brew":  map[string]interface{}{"formulae": []interface{}{"ripgrep"}},
					"cargo": []interface{}{"bat"},
					"npm":   map[string]interface{}{"global": []interface{}{"typescript"}},
				},
			},
			want: 3,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := flattenPackages(tt.tier)
			if len(got) != tt.want {
				t.Errorf("flattenPackages() returned %d packages, want %d: %v", len(got), tt.want, got)
			}
		})
	}
}

func TestCollectFiles(t *testing.T) {
	locked := map[string]interface{}{
		"files": []interface{}{
			map[string]interface{}{"target": "~/.config/company/policy.yaml", "source": "policy.yaml"},
		},
	}
	required := map[string]interface{}{
		"files": []interface{}{
			map[string]interface{}{"target": "~/.eslintrc.json", "source": "linting/.eslintrc.json"},
			// Duplicate of locked — should be deduped.
			map[string]interface{}{"target": "~/.config/company/policy.yaml", "source": "other.yaml"},
		},
	}
	recommended := map[string]interface{}{
		"files": []interface{}{
			map[string]interface{}{"target": "~/.config/aliases.sh", "content": "alias ll='ls -la'"},
		},
	}

	files := collectFiles(locked, required, recommended)
	if len(files) != 3 {
		t.Fatalf("collectFiles() returned %d files, want 3", len(files))
	}

	// First file should be the locked one (first tier wins on dedup).
	first := files[0].(map[string]interface{})
	if first["path"] != "~/.config/company/policy.yaml" {
		t.Errorf("first file path = %v, want ~/.config/company/policy.yaml", first["path"])
	}
	if first["source"] != "policy.yaml" {
		t.Errorf("first file source = %v, want policy.yaml (from locked tier)", first["source"])
	}
}

func TestFlattenSystemSettings(t *testing.T) {
	tier := map[string]interface{}{
		"system": map[string]interface{}{
			"macos-defaults": map[string]interface{}{
				"com.apple.screensaver": map[string]interface{}{
					"askForPassword":      float64(1),
					"askForPasswordDelay": float64(0),
				},
			},
			"sysctl": map[string]interface{}{
				"net.ipv4.ip_forward": float64(1),
			},
		},
	}

	settings := flattenSystemSettings(tier)
	if settings == nil {
		t.Fatal("flattenSystemSettings() returned nil")
	}

	expected := map[string]string{
		"macos-defaults.com.apple.screensaver.askForPassword":      "1",
		"macos-defaults.com.apple.screensaver.askForPasswordDelay": "0",
		"sysctl.net.ipv4.ip_forward":                              "1",
	}
	for key, want := range expected {
		got, ok := settings[key]
		if !ok {
			t.Errorf("missing key %q", key)
			continue
		}
		if got != want {
			t.Errorf("settings[%q] = %v, want %v", key, got, want)
		}
	}
}

func TestSanitizeName(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{"acme-jdoe", "acme-jdoe"},
		{"ACME_Corp", "acme-corp"},
		{"john.doe@acme", "john-doe-acme"},
		{"---leading---", "leading"},
		{"a", "a"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			got := sanitizeName(tt.input)
			if got != tt.want {
				t.Errorf("sanitizeName(%q) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}

func TestUnionStrings(t *testing.T) {
	result := unionStrings(
		[]string{"a", "b"},
		[]string{"b", "c"},
		[]string{"c", "d"},
	)
	if len(result) != 4 {
		t.Errorf("unionStrings() returned %d items, want 4: %v", len(result), result)
	}
	expected := []string{"a", "b", "c", "d"}
	for i, want := range expected {
		if result[i] != want {
			t.Errorf("result[%d] = %q, want %q", i, result[i], want)
		}
	}
}

func TestMergeStringMaps(t *testing.T) {
	a := map[string]interface{}{"x": "1", "y": "2"}
	b := map[string]interface{}{"y": "9", "z": "3"}
	result := mergeStringMaps(a, b)

	if result["x"] != "1" {
		t.Errorf("x = %v, want 1", result["x"])
	}
	if result["y"] != "2" {
		t.Errorf("y = %v, want 2 (first occurrence wins)", result["y"])
	}
	if result["z"] != "3" {
		t.Errorf("z = %v, want 3", result["z"])
	}
}

// ---------------------------------------------------------------------------
// Integration test — full RunFunction
// ---------------------------------------------------------------------------

func TestRunFunction(t *testing.T) {
	xr := map[string]interface{}{
		"apiVersion": "cfgd.io/v1alpha1",
		"kind":       "TeamConfig",
		"metadata": map[string]interface{}{
			"name":      "test-team",
			"namespace": "cfgd-system",
		},
		"spec": map[string]interface{}{
			"team":    "acme",
			"profile": "base",
			"policy": map[string]interface{}{
				"required": map[string]interface{}{
					"packages": map[string]interface{}{
						"brew": map[string]interface{}{
							"formulae": []interface{}{"git-secrets", "pre-commit"},
						},
					},
				},
				"recommended": map[string]interface{}{
					"packages": map[string]interface{}{
						"brew": map[string]interface{}{
							"formulae": []interface{}{"k9s"},
						},
					},
				},
			},
			"members": []interface{}{
				map[string]interface{}{
					"username": "jdoe",
					"hostname": "jdoe-mac",
				},
				map[string]interface{}{
					"username": "asmith",
					"profile":  "frontend",
				},
			},
		},
	}

	xrStruct, err := structpb.NewStruct(xr)
	if err != nil {
		t.Fatalf("structpb.NewStruct: %v", err)
	}

	req := &fnv1.RunFunctionRequest{
		Observed: &fnv1.State{
			Composite: &fnv1.Resource{
				Resource: xrStruct,
			},
		},
	}

	f := &Function{log: logging.NewNopLogger()}
	rsp, err := f.RunFunction(context.Background(), req)
	if err != nil {
		t.Fatalf("RunFunction returned error: %v", err)
	}

	// Should not have fatal conditions.
	for _, result := range rsp.GetResults() {
		if result.GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
			t.Fatalf("RunFunction returned fatal: %s", result.GetMessage())
		}
	}

	// Should have 3 desired resources: 2 MachineConfigs + 1 required ConfigPolicy.
	desired := rsp.GetDesired().GetResources()
	if len(desired) != 3 {
		t.Fatalf("expected 3 desired resources, got %d: %v", len(desired), resourceNames(desired))
	}

	// Verify MachineConfig for jdoe.
	mcJdoe, ok := desired["mc-jdoe"]
	if !ok {
		t.Fatal("missing desired resource mc-jdoe")
	}
	jdoeObj := mcJdoe.GetResource().AsMap()
	jdoeSpec := asMap(jdoeObj["spec"])
	if jdoeSpec["hostname"] != "jdoe-mac" {
		t.Errorf("jdoe hostname = %v, want jdoe-mac", jdoeSpec["hostname"])
	}
	if jdoeSpec["profile"] != "base" {
		t.Errorf("jdoe profile = %v, want base (team default)", jdoeSpec["profile"])
	}

	// Verify MachineConfig for asmith uses member override profile and placeholder hostname.
	mcAsmith, ok := desired["mc-asmith"]
	if !ok {
		t.Fatal("missing desired resource mc-asmith")
	}
	asmithSpec := asMap(mcAsmith.GetResource().AsMap()["spec"])
	if asmithSpec["profile"] != "frontend" {
		t.Errorf("asmith profile = %v, want frontend", asmithSpec["profile"])
	}
	if asmithSpec["hostname"] != "pending-asmith" {
		t.Errorf("asmith hostname = %v, want pending-asmith (placeholder)", asmithSpec["hostname"])
	}

	// Verify required ConfigPolicy exists with correct targetSelector.
	policyReq, ok := desired["policy-required"]
	if !ok {
		t.Fatal("missing desired resource policy-required")
	}
	policySpec := asMap(policyReq.GetResource().AsMap()["spec"])
	selector := asMap(policySpec["targetSelector"])
	matchLabels := asMap(selector["matchLabels"])
	if matchLabels["cfgd.io/team"] != "acme" {
		t.Errorf("policy targetSelector.matchLabels[cfgd.io/team] = %v, want acme", matchLabels["cfgd.io/team"])
	}
}

func TestRunFunctionNoPolicy(t *testing.T) {
	xr := map[string]interface{}{
		"apiVersion": "cfgd.io/v1alpha1",
		"kind":       "TeamConfig",
		"metadata":   map[string]interface{}{"name": "minimal"},
		"spec": map[string]interface{}{
			"team":    "minimal-team",
			"profile": "default",
			"members": []interface{}{
				map[string]interface{}{"username": "user1"},
			},
		},
	}

	xrStruct, err := structpb.NewStruct(xr)
	if err != nil {
		t.Fatalf("structpb.NewStruct: %v", err)
	}

	req := &fnv1.RunFunctionRequest{
		Observed: &fnv1.State{
			Composite: &fnv1.Resource{Resource: xrStruct},
		},
	}

	f := &Function{log: logging.NewNopLogger()}
	rsp, err := f.RunFunction(context.Background(), req)
	if err != nil {
		t.Fatalf("RunFunction error: %v", err)
	}

	for _, result := range rsp.GetResults() {
		if result.GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
			t.Fatalf("fatal: %s", result.GetMessage())
		}
	}

	// 1 MachineConfig, no ConfigPolicies (no enforceable items).
	desired := rsp.GetDesired().GetResources()
	if len(desired) != 1 {
		t.Fatalf("expected 1 desired resource, got %d: %v", len(desired), resourceNames(desired))
	}
}

func TestRunFunctionMissingTeam(t *testing.T) {
	xr := map[string]interface{}{
		"apiVersion": "cfgd.io/v1alpha1",
		"kind":       "TeamConfig",
		"metadata":   map[string]interface{}{"name": "bad"},
		"spec": map[string]interface{}{
			"members": []interface{}{},
		},
	}

	xrStruct, err := structpb.NewStruct(xr)
	if err != nil {
		t.Fatalf("structpb.NewStruct: %v", err)
	}

	req := &fnv1.RunFunctionRequest{
		Observed: &fnv1.State{
			Composite: &fnv1.Resource{Resource: xrStruct},
		},
	}

	f := &Function{log: logging.NewNopLogger()}
	rsp, err := f.RunFunction(context.Background(), req)
	if err != nil {
		t.Fatalf("RunFunction error: %v", err)
	}

	// Should have a fatal result.
	hasFatal := false
	for _, result := range rsp.GetResults() {
		if result.GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
			hasFatal = true
			break
		}
	}
	if !hasFatal {
		t.Error("expected fatal result for missing spec.team")
	}
}

func TestRunFunctionMissingProfile(t *testing.T) {
	xr := map[string]interface{}{
		"apiVersion": "cfgd.io/v1alpha1",
		"kind":       "TeamConfig",
		"metadata":   map[string]interface{}{"name": "no-profile"},
		"spec": map[string]interface{}{
			"team": "acme",
			// No team-level profile, and member has no profile override.
			"members": []interface{}{
				map[string]interface{}{"username": "jdoe"},
			},
		},
	}

	xrStruct, err := structpb.NewStruct(xr)
	if err != nil {
		t.Fatalf("structpb.NewStruct: %v", err)
	}

	req := &fnv1.RunFunctionRequest{
		Observed: &fnv1.State{
			Composite: &fnv1.Resource{Resource: xrStruct},
		},
	}

	f := &Function{log: logging.NewNopLogger()}
	rsp, err := f.RunFunction(context.Background(), req)
	if err != nil {
		t.Fatalf("RunFunction error: %v", err)
	}

	hasFatal := false
	for _, result := range rsp.GetResults() {
		if result.GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
			hasFatal = true
			break
		}
	}
	if !hasFatal {
		t.Error("expected fatal result for member with no profile")
	}
}

func TestRunFunctionLockedPolicy(t *testing.T) {
	xr := map[string]interface{}{
		"apiVersion": "cfgd.io/v1alpha1",
		"kind":       "TeamConfig",
		"metadata":   map[string]interface{}{"name": "locked-test"},
		"spec": map[string]interface{}{
			"team":    "secure",
			"profile": "base",
			"policy": map[string]interface{}{
				"locked": map[string]interface{}{
					"files": []interface{}{
						map[string]interface{}{
							"target":  "~/.config/company/policy.yaml",
							"content": "immutable",
						},
					},
					"system": map[string]interface{}{
						"sshd": map[string]interface{}{
							"PasswordAuthentication": "no",
						},
					},
				},
			},
			"members": []interface{}{
				map[string]interface{}{"username": "user1", "hostname": "host1"},
			},
		},
	}

	xrStruct, err := structpb.NewStruct(xr)
	if err != nil {
		t.Fatalf("structpb.NewStruct: %v", err)
	}

	req := &fnv1.RunFunctionRequest{
		Observed: &fnv1.State{
			Composite: &fnv1.Resource{Resource: xrStruct},
		},
	}

	f := &Function{log: logging.NewNopLogger()}
	rsp, err := f.RunFunction(context.Background(), req)
	if err != nil {
		t.Fatalf("RunFunction error: %v", err)
	}

	for _, result := range rsp.GetResults() {
		if result.GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
			t.Fatalf("fatal: %s", result.GetMessage())
		}
	}

	desired := rsp.GetDesired().GetResources()

	// 1 MachineConfig + 1 locked ConfigPolicy (locked has system settings).
	if len(desired) != 2 {
		t.Fatalf("expected 2 desired resources, got %d: %v", len(desired), resourceNames(desired))
	}

	// Verify locked ConfigPolicy.
	policyLocked, ok := desired["policy-locked"]
	if !ok {
		t.Fatal("missing desired resource policy-locked")
	}
	policyObj := policyLocked.GetResource().AsMap()
	if asMap(policyObj["metadata"])["labels"].(map[string]interface{})["cfgd.io/tier"] != "locked" {
		t.Error("locked policy missing cfgd.io/tier=locked label")
	}

	// Verify MachineConfig has the locked file and system settings.
	mcUser, ok := desired["mc-user1"]
	if !ok {
		t.Fatal("missing desired resource mc-user1")
	}
	mcSpec := asMap(mcUser.GetResource().AsMap()["spec"])
	files, ok := mcSpec["files"].([]interface{})
	if !ok || len(files) != 1 {
		t.Fatalf("expected 1 file, got %v", mcSpec["files"])
	}
	firstFile := files[0].(map[string]interface{})
	if firstFile["path"] != "~/.config/company/policy.yaml" {
		t.Errorf("file path = %v, want ~/.config/company/policy.yaml", firstFile["path"])
	}

	settings := asMap(mcSpec["systemSettings"])
	if settings["sshd.PasswordAuthentication"] != "no" {
		t.Errorf("sshd setting = %v, want no", settings["sshd.PasswordAuthentication"])
	}
}

func resourceNames(resources map[string]*fnv1.Resource) []string {
	names := make([]string, 0, len(resources))
	for name := range resources {
		names = append(names, name)
	}
	return names
}

func TestAsStringSlice(t *testing.T) {
	tests := []struct {
		name  string
		input interface{}
		want  int
	}{
		{"nil", nil, 0},
		{"empty", []interface{}{}, 0},
		{"strings", []interface{}{"a", "b", "c"}, 3},
		{"non-array", "not-an-array", 0},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := asStringSlice(tt.input)
			if len(got) != tt.want {
				t.Errorf("asStringSlice() = %v (len %d), want len %d", got, len(got), tt.want)
			}
		})
	}
}

func TestBuildModuleRefs(t *testing.T) {
	allModules := []string{"corp-vpn", "corp-certs", "editor"}
	requiredModules := []string{"corp-vpn", "corp-certs"}

	refs := buildModuleRefs(allModules, requiredModules)
	if len(refs) != 3 {
		t.Fatalf("buildModuleRefs() returned %d refs, want 3", len(refs))
	}

	// corp-vpn should be required
	vpnRef := refs[0].(map[string]interface{})
	if vpnRef["name"] != "corp-vpn" {
		t.Errorf("refs[0].name = %v, want corp-vpn", vpnRef["name"])
	}
	if vpnRef["required"] != true {
		t.Errorf("refs[0].required = %v, want true", vpnRef["required"])
	}

	// editor should not be required
	editorRef := refs[2].(map[string]interface{})
	if editorRef["name"] != "editor" {
		t.Errorf("refs[2].name = %v, want editor", editorRef["name"])
	}
	if editorRef["required"] != false {
		t.Errorf("refs[2].required = %v, want false", editorRef["required"])
	}
}

func TestRunFunctionWithModules(t *testing.T) {
	xr := map[string]interface{}{
		"apiVersion": "cfgd.io/v1alpha1",
		"kind":       "TeamConfig",
		"metadata":   map[string]interface{}{"name": "modules-test"},
		"spec": map[string]interface{}{
			"team":    "acme",
			"profile": "base",
			"modules": []interface{}{
				map[string]interface{}{"name": "corp-vpn"},
				map[string]interface{}{"name": "corp-certs"},
				map[string]interface{}{"name": "approved-editor"},
			},
			"policy": map[string]interface{}{
				"requiredModules":    []interface{}{"corp-vpn", "corp-certs"},
				"recommendedModules": []interface{}{"approved-editor"},
			},
			"members": []interface{}{
				map[string]interface{}{"username": "jdoe", "hostname": "jdoe-mac"},
			},
		},
	}

	xrStruct, err := structpb.NewStruct(xr)
	if err != nil {
		t.Fatalf("structpb.NewStruct: %v", err)
	}

	req := &fnv1.RunFunctionRequest{
		Observed: &fnv1.State{
			Composite: &fnv1.Resource{Resource: xrStruct},
		},
	}

	f := &Function{log: logging.NewNopLogger()}
	rsp, err := f.RunFunction(context.Background(), req)
	if err != nil {
		t.Fatalf("RunFunction error: %v", err)
	}

	for _, result := range rsp.GetResults() {
		if result.GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
			t.Fatalf("fatal: %s", result.GetMessage())
		}
	}

	desired := rsp.GetDesired().GetResources()

	// 1 MachineConfig + 1 required ConfigPolicy (has requiredModules)
	if len(desired) != 2 {
		t.Fatalf("expected 2 desired resources, got %d: %v", len(desired), resourceNames(desired))
	}

	// Verify MachineConfig has moduleRefs
	mcJdoe, ok := desired["mc-jdoe"]
	if !ok {
		t.Fatal("missing desired resource mc-jdoe")
	}
	jdoeSpec := asMap(mcJdoe.GetResource().AsMap()["spec"])
	moduleRefs, ok := jdoeSpec["moduleRefs"].([]interface{})
	if !ok {
		t.Fatalf("moduleRefs is not an array: %T", jdoeSpec["moduleRefs"])
	}
	if len(moduleRefs) != 3 {
		t.Fatalf("expected 3 moduleRefs, got %d", len(moduleRefs))
	}

	// Verify required policy has requiredModules
	policyReq, ok := desired["policy-required"]
	if !ok {
		t.Fatal("missing desired resource policy-required")
	}
	policySpec := asMap(policyReq.GetResource().AsMap()["spec"])
	reqModules, ok := policySpec["requiredModules"].([]interface{})
	if !ok {
		t.Fatalf("requiredModules is not an array: %T", policySpec["requiredModules"])
	}
	if len(reqModules) != 2 {
		t.Fatalf("expected 2 requiredModules, got %d", len(reqModules))
	}
}
