# `cfgd generate` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add AI-guided configuration generation (`cfgd generate`) that introspects a user's system, proposes layered profiles and modules, and walks through each component interactively — with a standalone CLI client and an MCP server sharing the same tool layer.

**Architecture:** Four layers built bottom-up: (1) core types and validation in cfgd-core, (2) tool implementations in cfgd binary's `generate/` module, (3) orchestration skill + embedded Anthropic API client, (4) MCP server. Each layer is independently shippable with its own tests and commits.

**Tech Stack:** Rust, serde/serde_json/serde_yaml, ureq (HTTP client for Anthropic API), inquire (interactive prompts), clap (CLI), existing cfgd-core infrastructure (Printer, PackageManager trait, atomic_write, config module).

**Spec:** `.claude/specs/2026-03-19-generate-design.md`

---

## File Structure

### cfgd-core (library crate) — new/modified files

| File | Purpose |
|------|---------|
| Modify: `crates/cfgd-core/src/config/mod.rs` | Add `AiConfig` struct, add `ai` field to `ConfigSpec` |
| Create: `crates/cfgd-core/src/generate/mod.rs` | Core generate types: `GenerateSession`, `PresentYamlRequest`, `PresentYamlResponse`, `ValidationResult`, `SchemaKind` |
| Create: `crates/cfgd-core/src/generate/schema.rs` | Hand-maintained annotated YAML schemas as const strings, `get_schema()` function |
| Create: `crates/cfgd-core/src/generate/validate.rs` | `validate_yaml()` — deserialize YAML into config structs, return structured errors |
| Create: `crates/cfgd-core/src/generate/session.rs` | `GenerateSession` — in-memory session state, write functions, repo root |
| Modify: `crates/cfgd-core/src/providers/mod.rs` | Add `PackageInfo` struct, `installed_packages_with_versions()` and `package_aliases()` default trait methods |
| Modify: `crates/cfgd-core/src/errors/mod.rs` | Add `GenerateError` enum and `CfgdError::Generate` variant |
| Modify: `crates/cfgd-core/src/lib.rs` | Add `pub mod generate;` declaration |

### cfgd (binary crate) — new/modified files

| File | Purpose |
|------|---------|
| Create: `crates/cfgd/src/generate/mod.rs` | Module root, re-exports |
| Create: `crates/cfgd/src/generate/scan.rs` | `scan_installed_packages`, `scan_dotfiles`, `scan_shell_config`, `scan_system_settings` |
| Create: `crates/cfgd/src/generate/inspect.rs` | `inspect_tool`, `query_package_manager` |
| Create: `crates/cfgd/src/generate/files.rs` | `read_file`, `list_directory`, `adopt_files` + security model (path validation, blocklist) |
| Create: `crates/cfgd/src/ai/mod.rs` | Module root |
| Create: `crates/cfgd/src/ai/client.rs` | Anthropic Messages API client — streaming, tool-use loop |
| Create: `crates/cfgd/src/ai/tools.rs` | Tool dispatch: API tool_use name → Rust function call → serialized result |
| Create: `crates/cfgd/src/ai/conversation.rs` | Conversation state: system prompt, message history, token tracking |
| Create: `crates/cfgd/src/mcp/mod.rs` | MCP module root |
| Create: `crates/cfgd/src/mcp/server.rs` | JSON-RPC stdin/stdout transport, request dispatch |
| Create: `crates/cfgd/src/mcp/tools.rs` | MCP tool definitions (JSON schemas), dispatch to generate/ functions |
| Create: `crates/cfgd/src/mcp/resources.rs` | MCP resource serving (skill, schemas) |
| Create: `crates/cfgd/src/mcp/prompts.rs` | MCP prompt definitions |
| Modify: `crates/cfgd/src/cli/mod.rs` | Add `Generate` and `McpServer` command variants, dispatch |
| Create: `crates/cfgd/src/cli/generate.rs` | Clap args, command handler, consent disclosure, conversation loop orchestration |
| Modify: `crates/cfgd/src/main.rs` | Add `mod generate; mod ai; mod mcp;` |
| Modify: `crates/cfgd/src/packages/mod.rs` | `installed_packages_with_versions()` + `package_aliases()` for each provider |
| Modify: `crates/cfgd/Cargo.toml` | Add `inquire`, `ureq` dependencies |

### Orchestration skill

| File | Purpose |
|------|---------|
| Create: `crates/cfgd/src/generate/skill.md` | The orchestration skill markdown (embedded as const string) |

### Documentation

| File | Purpose |
|------|---------|
| Modify: `docs/configuration.md` | Add `spec.ai` section |
| Modify: `docs/cli-reference.md` | Add `generate` and `mcp-server` commands |
| Modify: `docs/bootstrap.md` | Add AI-guided generation path |
| Create: `docs/ai-generate.md` | Full guide: CLI flow, MCP setup, model config, troubleshooting |

### CLAUDE.md updates

| File | Purpose |
|------|---------|
| Modify: `CLAUDE.md` | Add `generate/` to `std::process::Command` allowlist |

---

## Task 1: GenerateError type

**Files:**
- Modify: `crates/cfgd-core/src/errors/mod.rs`

- [ ] **Step 1: Write failing test for GenerateError**

In `crates/cfgd-core/src/errors/mod.rs`, add to the test module:

```rust
#[test]
fn generate_error_converts_to_cfgd_error() {
    let err = GenerateError::ValidationFailed {
        message: "missing apiVersion".into(),
    };
    let cfgd_err: CfgdError = err.into();
    assert!(matches!(cfgd_err, CfgdError::Generate(_)));
    assert!(cfgd_err.to_string().contains("missing apiVersion"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cfgd-core generate_error_converts`
Expected: Compilation error — `GenerateError` doesn't exist.

- [ ] **Step 3: Add GenerateError enum and CfgdError variant**

Add to `crates/cfgd-core/src/errors/mod.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("validation failed: {message}")]
    ValidationFailed { message: String },

    #[error("schema error: {message}")]
    SchemaError { message: String },

    #[error("file access denied: {path} — {reason}")]
    FileAccessDenied { path: PathBuf, reason: String },

    #[error("file too large: {path} ({size_bytes} bytes, max {max_bytes})")]
    FileTooLarge { path: PathBuf, size_bytes: u64, max_bytes: u64 },

    #[error("session error: {message}")]
    SessionError { message: String },

    #[error("AI provider error: {message}")]
    ProviderError { message: String },

    #[error("API key not found in environment variable '{env_var}'")]
    ApiKeyNotFound { env_var: String },
}
```

Add to `CfgdError`:

```rust
    #[error("generate error: {0}")]
    Generate(#[from] GenerateError),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p cfgd-core generate_error_converts`
Expected: PASS.

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p cfgd-core`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd-core/src/errors/mod.rs
git commit -m "feat(errors): add GenerateError enum for AI-guided generation"
```

---

## Task 2: AiConfig type and ConfigSpec integration

**Files:**
- Modify: `crates/cfgd-core/src/config/mod.rs:14-82`

- [ ] **Step 1: Write failing test for AiConfig deserialization**

In `crates/cfgd-core/src/config/mod.rs`, inside `#[cfg(test)] mod tests {}`, add:

Note: The config module uses `CfgdConfig` (not a polymorphic `CfgdDocument`). Tests deserialize into `CfgdConfig` directly, accessing `config.spec.ai`.

```rust
#[test]
fn test_ai_config_defaults() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec: {}
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    let ai = config.spec.ai.unwrap_or_default();
    assert_eq!(ai.provider, "claude");
    assert_eq!(ai.model, "claude-sonnet-4-6");
    assert_eq!(ai.api_key_env, "ANTHROPIC_API_KEY");
}

#[test]
fn test_ai_config_custom() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  ai:
    provider: claude
    model: claude-opus-4-6
    api-key-env: MY_CLAUDE_KEY
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    let ai = config.spec.ai.unwrap_or_default();
    assert_eq!(ai.model, "claude-opus-4-6");
    assert_eq!(ai.api_key_env, "MY_CLAUDE_KEY");
}

#[test]
fn test_existing_config_without_ai_still_parses() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work
  theme: default
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.spec.ai.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-core test_ai_config`
Expected: Compilation error — `AiConfig` doesn't exist, `ConfigSpec` has no `ai` field.

- [ ] **Step 3: Implement AiConfig struct and add to ConfigSpec**

In `crates/cfgd-core/src/config/mod.rs`, add the struct (near other config structs):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct AiConfig {
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    #[serde(default = "default_ai_model")]
    pub model: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
}

fn default_ai_provider() -> String { "claude".into() }
fn default_ai_model() -> String { "claude-sonnet-4-6".into() }
fn default_api_key_env() -> String { "ANTHROPIC_API_KEY".into() }
```

Add to `ConfigSpec` (after the `aliases` field, around line 80):

```rust
    #[serde(default)]
    pub ai: Option<AiConfig>,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-core test_ai_config`
Expected: All 3 tests PASS.

- [ ] **Step 5: Run full test suite to check for regressions**

Run: `cargo test -p cfgd-core`
Expected: All existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd-core/src/config/mod.rs
git commit -m "feat(config): add AiConfig type with provider, model, api-key-env fields"
```

---

## Task 2: PackageManager trait extensions

**Files:**
- Modify: `crates/cfgd-core/src/providers/mod.rs:13-32`

- [ ] **Step 1: Write failing test for new trait methods**

In `crates/cfgd-core/src/providers/mod.rs` inside the test module (around line 278), add:

Note: The existing `MockPackageManager` is `struct MockPackageManager { available: bool }` with `installed_packages()` returning an empty `HashSet`. The default trait implementations handle the conversion.

```rust
#[test]
fn test_default_installed_packages_with_versions_empty() {
    // MockPackageManager.installed_packages() returns empty set,
    // so the default installed_packages_with_versions() returns empty vec.
    let mock = MockPackageManager { available: true };
    let pkgs = mock.installed_packages_with_versions().unwrap();
    assert!(pkgs.is_empty());
}

#[test]
fn test_default_package_aliases_empty() {
    let mock = MockPackageManager { available: true };
    let aliases = mock.package_aliases("fd").unwrap();
    assert!(aliases.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-core test_default_installed`
Expected: Compilation error — methods don't exist.

- [ ] **Step 3: Add PackageInfo struct and trait methods with defaults**

In `crates/cfgd-core/src/providers/mod.rs`, add the struct (before the trait):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}
```

Add to the `PackageManager` trait (after `path_dirs`, around line 30):

```rust
    fn installed_packages_with_versions(&self) -> Result<Vec<PackageInfo>> {
        Ok(self.installed_packages()?
            .into_iter()
            .map(|name| PackageInfo { name, version: "unknown".into() })
            .collect())
    }

    fn package_aliases(&self, _canonical_name: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
```

Add necessary imports at the top: `use serde::{Serialize, Deserialize};` (if not already present).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-core test_default_installed test_default_package`
Expected: PASS.

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p cfgd-core`
Expected: All pass — default implementations mean no existing impls break.

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd-core/src/providers/mod.rs
git commit -m "feat(providers): add installed_packages_with_versions and package_aliases to PackageManager trait"
```

---

## Task 3: Generate core types and session state

**Files:**
- Create: `crates/cfgd-core/src/generate/mod.rs`
- Create: `crates/cfgd-core/src/generate/session.rs`
- Modify: `crates/cfgd-core/src/lib.rs`

- [ ] **Step 1: Create module structure with types**

Create `crates/cfgd-core/src/generate/mod.rs`:

```rust
pub mod schema;
pub mod session;
pub mod validate;

use serde::{Deserialize, Serialize};

/// Kinds of cfgd documents that can be generated/validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    Module,
    Profile,
    Config,
}

impl std::str::FromStr for SchemaKind {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "module" => Ok(Self::Module),
            "profile" => Ok(Self::Profile),
            "config" => Ok(Self::Config),
            _ => Err(format!("unknown schema kind: {}", s)),
        }
    }
}

impl SchemaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Module => "Module",
            Self::Profile => "Profile",
            Self::Config => "Config",
        }
    }
}

/// Request to present YAML for user review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentYamlRequest {
    pub content: String,
    pub kind: String,
    pub description: String,
}

/// User's response to a YAML presentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum PresentYamlResponse {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "reject")]
    Reject,
    #[serde(rename = "feedback")]
    Feedback { message: String },
    #[serde(rename = "step-through")]
    StepThrough,
}

/// Result of YAML validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
}
```

- [ ] **Step 2: Create session module**

Create `crates/cfgd-core/src/generate/session.rs`:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::atomic_write_str;
use crate::errors::{CfgdError, GenerateError};
use crate::generate::validate::validate_yaml;
use crate::generate::SchemaKind;

/// Tracks state for a generate session.
#[derive(Debug)]
pub struct GenerateSession {
    repo_root: PathBuf,
    generated: HashMap<String, GeneratedItem>,
}

#[derive(Debug, Clone)]
pub struct GeneratedItem {
    pub kind: SchemaKind,
    pub name: String,
    pub path: PathBuf,
}

impl GenerateSession {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            generated: HashMap::new(),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Write a module YAML after validation. Returns validation errors on failure.
    pub fn write_module_yaml(&mut self, name: &str, content: &str) -> Result<PathBuf, CfgdError> {
        let result = validate_yaml(content, SchemaKind::Module);
        if !result.valid {
            return Err(GenerateError::ValidationFailed {
                message: format!("Invalid module YAML: {}", result.errors.join("; ")),
            }.into());
        }
        let dir = self.repo_root.join("modules").join(name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("module.yaml");
        atomic_write_str(&path, content)?;
        let key = format!("module:{}", name);
        self.generated.insert(key, GeneratedItem {
            kind: SchemaKind::Module,
            name: name.to_string(),
            path: path.clone(),
        });
        Ok(path)
    }

    /// Write a profile YAML after validation. Returns validation errors on failure.
    pub fn write_profile_yaml(&mut self, name: &str, content: &str) -> Result<PathBuf, CfgdError> {
        let result = validate_yaml(content, SchemaKind::Profile);
        if !result.valid {
            return Err(GenerateError::ValidationFailed {
                message: format!("Invalid profile YAML: {}", result.errors.join("; ")),
            }.into());
        }
        let dir = self.repo_root.join("profiles");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.yaml", name));
        atomic_write_str(&path, content)?;
        let key = format!("profile:{}", name);
        self.generated.insert(key, GeneratedItem {
            kind: SchemaKind::Profile,
            name: name.to_string(),
            path: path.clone(),
        });
        Ok(path)
    }

    /// List items generated in this session.
    pub fn list_generated(&self) -> Vec<&GeneratedItem> {
        self.generated.values().collect()
    }

    /// List existing modules in the repo (on disk).
    pub fn get_existing_modules(&self) -> Result<Vec<String>, CfgdError> {
        let modules_dir = self.repo_root.join("modules");
        if !modules_dir.exists() {
            return Ok(vec![]);
        }
        let mut names = vec![];
        for entry in std::fs::read_dir(&modules_dir)? {
            let entry = entry?;
            if entry.path().is_dir() && entry.path().join("module.yaml").exists() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// List existing profiles in the repo (on disk).
    pub fn get_existing_profiles(&self) -> Result<Vec<String>, CfgdError> {
        let profiles_dir = self.repo_root.join("profiles");
        if !profiles_dir.exists() {
            return Ok(vec![]);
        }
        let mut names = vec![];
        for entry in std::fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_new() {
        let tmp = TempDir::new().unwrap();
        let session = GenerateSession::new(tmp.path().to_path_buf());
        assert_eq!(session.repo_root(), tmp.path());
        assert!(session.list_generated().is_empty());
    }

    #[test]
    fn test_get_existing_modules_empty() {
        let tmp = TempDir::new().unwrap();
        let session = GenerateSession::new(tmp.path().to_path_buf());
        let modules = session.get_existing_modules().unwrap();
        assert!(modules.is_empty());
    }

    #[test]
    fn test_get_existing_profiles_empty() {
        let tmp = TempDir::new().unwrap();
        let session = GenerateSession::new(tmp.path().to_path_buf());
        let profiles = session.get_existing_profiles().unwrap();
        assert!(profiles.is_empty());
    }

    #[test]
    fn test_get_existing_modules_finds_modules() {
        let tmp = TempDir::new().unwrap();
        let nvim_dir = tmp.path().join("modules").join("nvim");
        std::fs::create_dir_all(&nvim_dir).unwrap();
        std::fs::write(nvim_dir.join("module.yaml"), "test").unwrap();
        let tmux_dir = tmp.path().join("modules").join("tmux");
        std::fs::create_dir_all(&tmux_dir).unwrap();
        std::fs::write(tmux_dir.join("module.yaml"), "test").unwrap();

        let session = GenerateSession::new(tmp.path().to_path_buf());
        let modules = session.get_existing_modules().unwrap();
        assert_eq!(modules, vec!["nvim", "tmux"]);
    }

    #[test]
    fn test_get_existing_profiles_finds_profiles() {
        let tmp = TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("base.yaml"), "test").unwrap();
        std::fs::write(profiles_dir.join("work.yaml"), "test").unwrap();

        let session = GenerateSession::new(tmp.path().to_path_buf());
        let profiles = session.get_existing_profiles().unwrap();
        assert_eq!(profiles, vec!["base", "work"]);
    }
}
```

- [ ] **Step 3: Add `pub mod generate;` to lib.rs**

In `crates/cfgd-core/src/lib.rs`, add near the other module declarations:

```rust
pub mod generate;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd-core generate::session`
Expected: All session tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd-core/src/generate/ crates/cfgd-core/src/lib.rs
git commit -m "feat(generate): add core types, session state, and generate module structure"
```

---

## Task 4: YAML validation

**Files:**
- Create: `crates/cfgd-core/src/generate/validate.rs`

- [ ] **Step 1: Write failing tests for validate_yaml**

Create `crates/cfgd-core/src/generate/validate.rs`:

```rust
use crate::config::{CfgdConfig, ModuleDocument, ProfileDocument};
use crate::generate::{SchemaKind, ValidationResult};

/// Validate YAML content against a cfgd schema kind.
/// Attempts to deserialize into the appropriate config struct.
/// Returns structured errors so the AI can self-correct.
pub fn validate_yaml(content: &str, kind: SchemaKind) -> ValidationResult {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_module() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  packages:
    - name: neovim
"#;
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(result.valid, "Expected valid, got errors: {:?}", result.errors);
    }

    #[test]
    fn test_validate_valid_profile() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  modules: [nvim, tmux]
"#;
        let result = validate_yaml(yaml, SchemaKind::Profile);
        assert!(result.valid, "Expected valid, got errors: {:?}", result.errors);
    }

    #[test]
    fn test_validate_invalid_yaml_syntax() {
        let yaml = "not: [valid: yaml: {{";
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_validate_wrong_kind() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: test
spec: {}
"#;
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("kind")));
    }

    #[test]
    fn test_validate_missing_api_version() {
        let yaml = r#"
kind: Module
metadata:
  name: test
spec: {}
"#;
        let result = validate_yaml(yaml, SchemaKind::Module);
        // Should either fail or warn about missing apiVersion
        // The exact behavior depends on whether apiVersion is required in serde
        assert!(result.valid || !result.errors.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-core generate::validate`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement validate_yaml**

Replace the `todo!()` in `validate_yaml`:

```rust
pub fn validate_yaml(content: &str, kind: SchemaKind) -> ValidationResult {
    // Step 1: Parse as generic YAML to check syntax
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return ValidationResult {
                valid: false,
                errors: vec![format!("YAML syntax error: {}", e)],
            };
        }
    };

    // Step 2: Check kind field matches expected
    if let Some(doc_kind) = value.get("kind").and_then(|v| v.as_str()) {
        if doc_kind != kind.as_str() {
            return ValidationResult {
                valid: false,
                errors: vec![format!(
                    "Expected kind '{}', found '{}'",
                    kind.as_str(),
                    doc_kind
                )],
            };
        }
    }

    // Step 3: Attempt deserialization into the concrete document type for the kind.
    // The config module uses separate types: CfgdConfig, ModuleDocument, ProfileDocument.
    let deser_result = match kind {
        SchemaKind::Module => serde_yaml::from_str::<ModuleDocument>(content).map(|_| ()),
        SchemaKind::Profile => serde_yaml::from_str::<ProfileDocument>(content).map(|_| ()),
        SchemaKind::Config => serde_yaml::from_str::<CfgdConfig>(content).map(|_| ()),
    };

    match deser_result {
        Ok(()) => ValidationResult {
            valid: true,
            errors: vec![],
        },
        Err(e) => ValidationResult {
            valid: false,
            errors: vec![format!("Deserialization error: {}", e)],
        },
    }
}

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-core generate::validate`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd-core/src/generate/validate.rs
git commit -m "feat(generate): add validate_yaml for schema validation with structured errors"
```

---

## Task 5: Schema export

**Files:**
- Create: `crates/cfgd-core/src/generate/schema.rs`

- [ ] **Step 1: Write test for get_schema**

Create `crates/cfgd-core/src/generate/schema.rs`:

```rust
use crate::generate::SchemaKind;

/// Return annotated YAML schema documentation for a given kind.
/// These are hand-maintained examples that document every field.
pub fn get_schema(kind: SchemaKind) -> &'static str {
    match kind {
        SchemaKind::Module => MODULE_SCHEMA,
        SchemaKind::Profile => PROFILE_SCHEMA,
        SchemaKind::Config => CONFIG_SCHEMA,
    }
}

const MODULE_SCHEMA: &str = "";
const PROFILE_SCHEMA: &str = "";
const CONFIG_SCHEMA: &str = "";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_schema_not_empty() {
        let schema = get_schema(SchemaKind::Module);
        assert!(!schema.is_empty(), "Module schema should not be empty");
        assert!(schema.contains("apiVersion"), "Module schema must document apiVersion");
        assert!(schema.contains("kind: Module"), "Module schema must show kind");
        assert!(schema.contains("packages"), "Module schema must document packages field");
        assert!(schema.contains("files"), "Module schema must document files field");
        assert!(schema.contains("depends"), "Module schema must document depends field");
        assert!(schema.contains("env"), "Module schema must document env field");
        assert!(schema.contains("aliases"), "Module schema must document aliases field");
    }

    #[test]
    fn test_profile_schema_not_empty() {
        let schema = get_schema(SchemaKind::Profile);
        assert!(!schema.is_empty(), "Profile schema should not be empty");
        assert!(schema.contains("apiVersion"), "Profile schema must document apiVersion");
        assert!(schema.contains("kind: Profile"), "Profile schema must show kind");
        assert!(schema.contains("inherits"), "Profile schema must document inherits field");
        assert!(schema.contains("modules"), "Profile schema must document modules field");
        assert!(schema.contains("packages"), "Profile schema must document packages field");
        assert!(schema.contains("system"), "Profile schema must document system field");
    }

    #[test]
    fn test_config_schema_not_empty() {
        let schema = get_schema(SchemaKind::Config);
        assert!(!schema.is_empty(), "Config schema should not be empty");
        assert!(schema.contains("apiVersion"), "Config schema must document apiVersion");
        assert!(schema.contains("kind: Config"), "Config schema must show kind");
        assert!(schema.contains("ai"), "Config schema must document ai field");
    }

    #[test]
    fn test_module_schema_is_valid_yaml() {
        // The schema example itself should be parseable YAML
        let schema = get_schema(SchemaKind::Module);
        // Extract the YAML block (between ```yaml and ```)
        // For now, validate the raw content parses
        let _: serde_yaml::Value = serde_yaml::from_str(schema)
            .expect("Module schema must be valid YAML");
    }

    #[test]
    fn test_profile_schema_is_valid_yaml() {
        let schema = get_schema(SchemaKind::Profile);
        let _: serde_yaml::Value = serde_yaml::from_str(schema)
            .expect("Profile schema must be valid YAML");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-core generate::schema`
Expected: FAIL — empty schema strings.

- [ ] **Step 3: Write the annotated schema strings**

Replace the empty `MODULE_SCHEMA`, `PROFILE_SCHEMA`, and `CONFIG_SCHEMA` constants with comprehensive annotated YAML examples. These should document every field from `docs/modules.md`, `docs/profiles.md`, and `docs/configuration.md` respectively — using YAML comments to describe each field's purpose, type, and valid values.

Refer to:
- `crates/cfgd-core/src/config/mod.rs` for exact struct field names
- `docs/modules.md` for module field documentation
- `docs/profiles.md` for profile field documentation
- `docs/configuration.md` for config field documentation

Each schema const should be a complete, valid YAML document with inline `# comments` explaining every field. The YAML should use the `cfgd.io/v1alpha1` apiVersion and the correct `kind`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-core generate::schema`
Expected: All pass — schemas are non-empty, contain required field names, and parse as valid YAML.

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd-core/src/generate/schema.rs
git commit -m "feat(generate): add annotated YAML schema export for Module, Profile, Config"
```

---

## Task 6: Session write functions integration test

**Files:**
- Modify: `crates/cfgd-core/src/generate/session.rs`

- [ ] **Step 1: Write integration test for write_module_yaml**

Add to the test module in `session.rs`:

```rust
#[test]
fn test_write_module_yaml_valid() {
    let tmp = TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  packages:
    - name: neovim
"#;
    let path = session.write_module_yaml("nvim", yaml).unwrap();
    assert_eq!(path, tmp.path().join("modules/nvim/module.yaml"));
    assert!(path.exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), yaml);
    assert_eq!(session.list_generated().len(), 1);
}

#[test]
fn test_write_module_yaml_invalid_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let result = session.write_module_yaml("bad", "not valid yaml {{");
    assert!(result.is_err());
    assert!(session.list_generated().is_empty());
}

#[test]
fn test_write_profile_yaml_valid() {
    let tmp = TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  modules: [nvim]
"#;
    let path = session.write_profile_yaml("base", yaml).unwrap();
    assert_eq!(path, tmp.path().join("profiles/base.yaml"));
    assert!(path.exists());
    assert_eq!(session.list_generated().len(), 1);
}

#[test]
fn test_write_profile_yaml_wrong_kind_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut session = GenerateSession::new(tmp.path().to_path_buf());
    // Module YAML passed to write_profile_yaml
    let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec: {}
"#;
    let result = session.write_profile_yaml("nvim", yaml);
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p cfgd-core generate::session`
Expected: All pass (uses validate_yaml from Task 4).

- [ ] **Step 3: Commit**

```bash
git add crates/cfgd-core/src/generate/session.rs
git commit -m "test(generate): add write_module_yaml and write_profile_yaml integration tests"
```

---

## Task 7: Layer 1 commit — update docs

**Files:**
- Modify: `docs/configuration.md`

- [ ] **Step 1: Add spec.ai section to docs/configuration.md**

Add a new section documenting the `ai` config:

```markdown
## AI Configuration

Configure the AI provider for `cfgd generate`:

```yaml
spec:
  ai:
    provider: claude              # AI provider (default: claude)
    model: claude-sonnet-4-6      # Model ID (default: claude-sonnet-4-6)
    api-key-env: ANTHROPIC_API_KEY # Env var containing API key (default: ANTHROPIC_API_KEY)
```

API keys are never stored in config files. The `api-key-env` field names the environment variable to read. CLI flags `--model` and `--provider` override config values.
```

- [ ] **Step 2: Update CLAUDE.md module map**

Add `generate/` to the cfgd-core module map section and note the new generate module. Do NOT update the `std::process::Command` allowlist yet — that happens in Task 13 when the binary crate's `generate/` module (which shells out) is created.

- [ ] **Step 3: Run full test suite for Layer 1**

Run: `cargo test`
Expected: All pass.

- [ ] **Step 4: Commit Layer 1**

```bash
git add docs/configuration.md CLAUDE.md
git commit -m "docs: add AI config section, update cfgd-core module map for generate"
```

---

## Task 8: Generate tool implementations — scan_dotfiles and scan_shell_config

**Files:**
- Create: `crates/cfgd/src/generate/mod.rs`
- Create: `crates/cfgd/src/generate/scan.rs`
- Modify: `crates/cfgd/src/main.rs`

- [ ] **Step 1: Create generate module structure**

Create `crates/cfgd/src/generate/mod.rs`:

```rust
pub mod scan;
// pub mod inspect; — added in Task 10
// pub mod files;   — added in Task 9
```

Add to `crates/cfgd/src/main.rs`:

```rust
mod generate;
```

- [ ] **Step 2: Write failing tests for scan_dotfiles**

Create `crates/cfgd/src/generate/scan.rs` with types and test:

```rust
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use cfgd_core::errors::CfgdError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DotfileEntry {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub entry_type: String, // "file", "directory", "symlink"
    pub tool_guess: Option<String>,
}

/// Scan home directory for dotfiles and config directories.
pub fn scan_dotfiles(home: &Path) -> Result<Vec<DotfileEntry>, CfgdError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_scan_dotfiles_finds_dotfiles() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // Create some dotfiles
        std::fs::write(home.join(".zshrc"), "# zsh config").unwrap();
        std::fs::write(home.join(".gitconfig"), "[user]\nname = Test").unwrap();
        std::fs::create_dir_all(home.join(".config/nvim")).unwrap();
        std::fs::write(home.join(".config/nvim/init.lua"), "-- nvim").unwrap();

        let entries = scan_dotfiles(home).unwrap();
        assert!(entries.iter().any(|e| e.path.ends_with(".zshrc")));
        assert!(entries.iter().any(|e| e.path.ends_with(".gitconfig")));
        assert!(entries.iter().any(|e| e.path.ends_with(".config/nvim")));
    }

    #[test]
    fn test_scan_dotfiles_guesses_tools() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::write(home.join(".vimrc"), "set nocompatible").unwrap();
        std::fs::create_dir_all(home.join(".config/nvim")).unwrap();
        std::fs::write(home.join(".config/nvim/init.lua"), "").unwrap();
        std::fs::write(home.join(".tmux.conf"), "set -g mouse on").unwrap();

        let entries = scan_dotfiles(home).unwrap();
        let vim_entry = entries.iter().find(|e| e.path.ends_with(".vimrc")).unwrap();
        assert_eq!(vim_entry.tool_guess.as_deref(), Some("vim"));
        let nvim_entry = entries.iter().find(|e| e.path.ends_with(".config/nvim")).unwrap();
        assert_eq!(nvim_entry.tool_guess.as_deref(), Some("nvim"));
        let tmux_entry = entries.iter().find(|e| e.path.ends_with(".tmux.conf")).unwrap();
        assert_eq!(tmux_entry.tool_guess.as_deref(), Some("tmux"));
    }

    #[test]
    fn test_scan_dotfiles_empty_home() {
        let tmp = TempDir::new().unwrap();
        let entries = scan_dotfiles(tmp.path()).unwrap();
        assert!(entries.is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p cfgd scan_dotfiles`
Expected: FAIL — `todo!()`.

- [ ] **Step 4: Implement scan_dotfiles**

Replace `todo!()` with implementation that:
1. Scans `home` for files/dirs starting with `.` (skip `.git`, `.DS_Store`, etc.)
2. Scans `home/.config/` for tool config directories
3. For each entry, determines type (file/dir/symlink), size, and guesses the associated tool from a hardcoded map (`.zshrc`→zsh, `.vimrc`→vim, `.config/nvim`→nvim, `.tmux.conf`→tmux, `.gitconfig`→git, etc.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cfgd scan_dotfiles`
Expected: All pass.

- [ ] **Step 6: Write tests for scan_shell_config and implement**

Add to `scan.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfigResult {
    pub shell: String,
    pub config_files: Vec<PathBuf>,
    pub aliases: Vec<ScannedAlias>,
    pub exports: Vec<ScannedExport>,
    pub path_additions: Vec<String>,
    pub sourced_files: Vec<PathBuf>,
    pub plugin_manager: Option<String>,
}

// Named ScannedAlias/ScannedExport to avoid collision with
// cfgd_core::config::ShellAlias and cfgd_core::config::EnvVar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedAlias {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedExport {
    pub name: String,
    pub value: String,
}

/// Parse shell configuration files for aliases, exports, PATH additions.
pub fn scan_shell_config(shell: &str, home: &Path) -> Result<ShellConfigResult, CfgdError> {
    todo!()
}
```

Write tests for:
- Parsing `alias name='command'` and `alias name="command"` patterns
- Parsing `export NAME=value` and `export NAME="value"` patterns
- Detecting `PATH` additions
- Detecting `source` and `.` sourced files
- Detecting oh-my-zsh, zinit, zplug plugin managers
- Handling missing config files gracefully

Implement the parser — regex-based line-by-line parsing of shell rc files. Not a full shell parser, just common patterns.

- [ ] **Step 7: Run all scan tests**

Run: `cargo test -p cfgd generate::scan`
Expected: All pass.

- [ ] **Step 8: Commit**

```bash
git add crates/cfgd/src/generate/ crates/cfgd/src/main.rs
git commit -m "feat(generate): add scan_dotfiles and scan_shell_config tool implementations"
```

---

## Task 9: Generate tool implementations — file access with security model

**Files:**
- Create: `crates/cfgd/src/generate/files.rs`

- [ ] **Step 1: Write failing tests for security model**

Create `crates/cfgd/src/generate/files.rs` with security tests:

```rust
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use cfgd_core::errors::CfgdError;

const MAX_FILE_SIZE: u64 = 64 * 1024; // 64 KB

/// Patterns that are blocked from read_file for security.
const BLOCKED_PATTERNS: &[&str] = &[
    ".ssh/id_",
    ".gnupg/private-keys",
    ".pem",
    ".key",
    "credentials",
    "secret",
    "token",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadResult {
    pub path: PathBuf,
    pub content: String,
    pub size_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub entry_type: String,
    pub size_bytes: Option<u64>,
}

/// Check if a path is allowed (within home or repo, not in blocklist).
fn is_path_allowed(path: &Path, home: &Path, repo_root: &Path) -> Result<(), CfgdError> {
    todo!()
}

/// Read a file with security constraints.
pub fn read_file(path: &Path, home: &Path, repo_root: &Path) -> Result<FileReadResult, CfgdError> {
    todo!()
}

/// List directory contents with security constraints.
pub fn list_directory(path: &Path, home: &Path, repo_root: &Path) -> Result<Vec<DirectoryEntry>, CfgdError> {
    todo!()
}

/// Copy config files into module or profile directories.
pub fn adopt_files(
    source_paths: &[(PathBuf, PathBuf)], // (source, relative_dest)
    target_dir: &Path,
) -> Result<Vec<PathBuf>, CfgdError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_read_file_within_home() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let file = home.join(".zshrc");
        std::fs::write(&file, "# zsh config").unwrap();

        let result = read_file(&file, &home, &repo).unwrap();
        assert_eq!(result.content, "# zsh config");
        assert!(!result.truncated);
    }

    #[test]
    fn test_read_file_outside_home_rejected() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let outside = tmp.path().join("etc_shadow");
        std::fs::write(&outside, "secret").unwrap();

        let result = read_file(&outside, &home, &repo);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_file_ssh_key_blocked() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        let ssh_dir = home.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let key = ssh_dir.join("id_rsa");
        std::fs::write(&key, "private key").unwrap();

        let result = read_file(&key, &home, &repo);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_file_truncation() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let large_file = home.join("large.txt");
        let content = "x".repeat(128 * 1024); // 128 KB
        std::fs::write(&large_file, &content).unwrap();

        let result = read_file(&large_file, &home, &repo).unwrap();
        assert!(result.truncated);
        assert!(result.content.len() <= MAX_FILE_SIZE as usize + 100); // some margin for warning
    }

    #[test]
    fn test_list_directory_within_home() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let config_dir = home.join(".config/nvim");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(config_dir.join("init.lua"), "-- config").unwrap();

        let entries = list_directory(&config_dir, &home, &repo).unwrap();
        assert!(entries.iter().any(|e| e.name == "init.lua"));
    }

    #[test]
    fn test_adopt_files_copies_to_target() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("init.lua"), "-- nvim config").unwrap();
        let target = tmp.path().join("modules/nvim/config");

        let paths = adopt_files(
            &[(source.join("init.lua"), PathBuf::from("init.lua"))],
            &target,
        ).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(target.join("init.lua").exists());
        assert_eq!(std::fs::read_to_string(target.join("init.lua")).unwrap(), "-- nvim config");
    }

    #[test]
    fn test_symlink_escape_blocked() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "data").unwrap();

        // Create symlink inside home pointing outside
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside.join("secret"), home.join("escape_link")).unwrap();

        #[cfg(unix)]
        {
            let result = read_file(&home.join("escape_link"), &home, &repo);
            assert!(result.is_err());
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd generate::files`
Expected: FAIL — `todo!()`.

- [ ] **Step 3: Implement is_path_allowed, read_file, list_directory, adopt_files**

Implement using:
- `std::fs::canonicalize` for symlink resolution
- `cfgd_core::validate_path_within` for boundary checks
- `BLOCKED_PATTERNS` for credential blocklist
- `MAX_FILE_SIZE` for truncation
- `cfgd_core::atomic_write` for file adoption

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd generate::files`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd/src/generate/files.rs
git commit -m "feat(generate): add read_file, list_directory, adopt_files with security model"
```

---

## Task 10: Generate tool implementations — inspect_tool and query_package_manager

**Files:**
- Create: `crates/cfgd/src/generate/inspect.rs`

- [ ] **Step 1: Write types and tests**

Create `crates/cfgd/src/generate/inspect.rs`:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use cfgd_core::errors::CfgdError;
use cfgd_core::providers::{PackageInfo, PackageManager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInspection {
    pub name: String,
    pub version: Option<String>,
    pub config_paths: Vec<PathBuf>,
    pub plugin_system: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageQueryResult {
    pub package: String,
    pub manager: String,
    pub available: bool,
    pub version: Option<String>,
    pub aliases: Vec<String>,
}

/// Inspect a tool: version, config paths, plugin system.
pub fn inspect_tool(name: &str, home: &std::path::Path) -> Result<ToolInspection, CfgdError> {
    todo!()
}

/// Query a package manager about a specific package.
pub fn query_package_manager(
    manager: &dyn PackageManager,
    package: &str,
) -> Result<PackageQueryResult, CfgdError> {
    todo!()
}
```

Write tests for:
- `inspect_tool` with a known tool (use a mock approach — create a fake binary in temp dir and add to PATH, or test the config path discovery separately)
- `query_package_manager` with `MockPackageManager` from cfgd-core

- [ ] **Step 2: Implement**

`inspect_tool`:
1. Run `<name> --version` (or `<name> -V`, `-version`) via `std::process::Command`
2. Check common config paths: `~/.config/<name>/`, `~/.<name>rc`, `~/.<name>/`, `~/.<name>.conf`
3. Detect plugin systems from config files (look for lazy.nvim, tpm, oh-my-zsh patterns)

`query_package_manager`:
1. Call `manager.available_version(package)`
2. Call `manager.package_aliases(package)`
3. Combine into `PackageQueryResult`

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd generate::inspect`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd/src/generate/inspect.rs
git commit -m "feat(generate): add inspect_tool and query_package_manager tool implementations"
```

---

## Task 11: PackageManager provider implementations for new trait methods

**Files:**
- Modify: `crates/cfgd/src/packages/mod.rs` (and per-provider files if they exist separately)

- [ ] **Step 1: Write tests for brew installed_packages_with_versions**

Add test in the packages module for `BrewManager::installed_packages_with_versions()`. Since this shells out to `brew`, the test should be `#[ignore]` (requires brew to be installed) or use a mock. Write at least one unit test per provider that validates the parsing logic with sample command output.

- [ ] **Step 2: Implement installed_packages_with_versions for each provider**

For each provider:
- **brew**: Parse `brew list --versions` output (format: `package version`)
- **apt**: Parse `dpkg-query -W -f='${Package}\t${Version}\n'` output
- **cargo**: Parse `cargo install --list` output
- **npm**: Parse `npm list -g --depth=0 --json` output
- **pipx**: Parse `pipx list --json` output
- **dnf**: Parse `rpm -qa --queryformat '%{NAME}\t%{VERSION}\n'` output

- [ ] **Step 3: Implement package_aliases for providers that need them**

At minimum, apt and dnf need alias mappings for common packages:
- `fd` → `fd-find` (apt/dnf)
- `rg` → `ripgrep` (apt/dnf — package name differs from binary)
- `bat` → `batcat` (apt)
- `nvim` → `neovim` (apt/dnf)

These can be a hardcoded `HashMap` per provider for now — the AI's web search handles edge cases.

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd packages`
Expected: All pass (including existing tests — default impls ensure no regression).

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd/src/packages/
git commit -m "feat(packages): implement installed_packages_with_versions and package_aliases for all providers"
```

---

## Task 12: scan_installed_packages and scan_system_settings

**Files:**
- Modify: `crates/cfgd/src/generate/scan.rs`

- [ ] **Step 1: Write scan_installed_packages**

Add to `scan.rs`:

```rust
use cfgd_core::providers::{PackageInfo, PackageManager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageEntry {
    pub name: String,
    pub version: String,
    pub manager: String,
}

/// Scan installed packages across all available package managers.
pub fn scan_installed_packages(
    managers: &[&dyn PackageManager],
    filter_manager: Option<&str>,
) -> Result<Vec<InstalledPackageEntry>, CfgdError> {
    // ...
}
```

Test with MockPackageManager.

- [ ] **Step 2: Write scan_system_settings**

Add to `scan.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSettingsResult {
    pub macos_defaults: Option<serde_json::Value>,
    pub systemd_units: Vec<String>,
    pub launch_agents: Vec<String>,
}

/// Scan platform-specific system settings.
pub fn scan_system_settings() -> Result<SystemSettingsResult, CfgdError> {
    // Platform-conditional implementation
}
```

Test the parsing logic with sample outputs rather than live system calls.

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd generate::scan`
Expected: All pass.

- [ ] **Step 4: Commit Layer 2**

```bash
git add crates/cfgd/src/generate/
git commit -m "feat(generate): add scan_installed_packages and scan_system_settings"
```

---

## Task 13: Update CLAUDE.md Command allowlist

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add generate/ to std::process::Command allowlist**

In CLAUDE.md Hard Rule #6, add `generate/` to the list of allowed locations.

- [ ] **Step 2: Update Module Map**

Add `generate/` to the cfgd binary crate section of the module map.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add generate/ module to Command allowlist and module map"
```

---

## Task 14: Orchestration skill document

**Files:**
- Create: `crates/cfgd/src/generate/skill.md`

- [ ] **Step 1: Write the skill markdown**

Author the complete orchestration skill as described in the spec's "Orchestration Skill" section. Must cover:

1. Role and objective
2. Mode handling (full / module / profile)
3. Schema field coverage — for every field in Module and Profile schemas, the investigation depth expected:
   - `packages`: version constraints per manager, `prefer` ordering, `min-version`, aliases, companion packages, manager exclusions
   - `files`: config path discovery (XDG/legacy/platform), templating decisions, source strategy
   - `env`: related exports, recommended env vars
   - `aliases`: shell aliases, community conventions
   - `scripts.post-apply`: recommended post-install steps, verification
   - `depends`: decomposition heuristics (own config dir + multiple packages + post-apply → suggest separate module)
   - `system`: macOS defaults, systemd units, launch agents
   - `secrets`: encrypted file identification
   - `inherits`: profile layering strategy
4. Interaction protocol — when to call `present_yaml`, the accept/reject/feedback/step-through flow
5. Quality standards — every `prefer` list justified, every `min-version` grounded, no stubs
6. Web search guidance — "If web search is unavailable, state confidence level"

- [ ] **Step 2: Write the const string embedding**

In `crates/cfgd/src/generate/mod.rs`, add:

```rust
/// The orchestration skill, embedded at compile time.
pub const GENERATE_SKILL: &str = include_str!("skill.md");
```

- [ ] **Step 3: Write a test that the skill embeds correctly**

```rust
#[test]
fn test_skill_embeds() {
    assert!(!GENERATE_SKILL.is_empty());
    assert!(GENERATE_SKILL.contains("cfgd's configuration generator"));
}
```

- [ ] **Step 4: Run test**

Run: `cargo test -p cfgd generate::mod::tests::test_skill_embeds` (or equivalent)
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd/src/generate/skill.md crates/cfgd/src/generate/mod.rs
git commit -m "feat(generate): add orchestration skill document for AI-guided generation"
```

---

## Task 15: Anthropic API client

**Files:**
- Create: `crates/cfgd/src/ai/mod.rs`
- Create: `crates/cfgd/src/ai/client.rs`
- Modify: `crates/cfgd/Cargo.toml`
- Modify: `crates/cfgd/src/main.rs`

- [ ] **Step 1: Add dependencies**

Add to `crates/cfgd/Cargo.toml`:

```toml
inquire = "0.7"
ureq = { version = "2", features = ["json"] }
```

(`ureq` may already be pulled transitively via cfgd-core. Check and only add if needed.)

- [ ] **Step 2: Create AI module structure**

Create `crates/cfgd/src/ai/mod.rs`:

```rust
pub mod client;
pub mod conversation;
pub mod tools;
```

Add to `crates/cfgd/src/main.rs`:

```rust
mod ai;
```

- [ ] **Step 3: Write the API client**

Create `crates/cfgd/src/ai/client.rs`:

Implement `AnthropicClient` struct with:
- `new(api_key: String, model: String)` constructor
- `send_message(messages: &[Message], system: &str, tools: &[ToolDef]) -> Result<Response>` — sends to Anthropic Messages API
- Streaming support via SSE parsing (`ureq` response body read line-by-line)
- `Message`, `ContentBlock`, `ToolUse`, `ToolResult` serde types matching the Anthropic API schema

Write unit tests for:
- Request serialization (verify JSON matches expected Anthropic API format)
- Response deserialization (parse sample API responses including tool_use blocks)
- Error handling (non-200 responses, malformed JSON)

Use `serde_json` for serialization. No need for a full Anthropic SDK — the Messages API is a single endpoint.

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd ai::client`
Expected: All pass (no live API calls — just serialization/deserialization tests).

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd/src/ai/ crates/cfgd/Cargo.toml crates/cfgd/src/main.rs
git commit -m "feat(ai): add Anthropic Messages API client with streaming and tool-use support"
```

---

## Task 16: Tool dispatch layer

**Files:**
- Create: `crates/cfgd/src/ai/tools.rs`

- [ ] **Step 1: Define tool definitions matching Anthropic tool-use schema**

Create `crates/cfgd/src/ai/tools.rs`:

Define a `ToolRegistry` that maps tool names to:
- JSON schema for input parameters (used in API tool definitions)
- A dispatch function that calls the appropriate tool implementation

Tools to register:
- `scan_installed_packages` → `generate::scan::scan_installed_packages`
- `scan_dotfiles` → `generate::scan::scan_dotfiles`
- `scan_shell_config` → `generate::scan::scan_shell_config`
- `scan_system_settings` → `generate::scan::scan_system_settings`
- `detect_platform` → `cfgd_core::platform::Platform::detect`
- `inspect_tool` → `generate::inspect::inspect_tool`
- `query_package_manager` → `generate::inspect::query_package_manager`
- `read_file` → `generate::files::read_file`
- `list_directory` → `generate::files::list_directory`
- `adopt_files` → `generate::files::adopt_files`
- `get_schema` → `cfgd_core::generate::schema::get_schema`
- `validate_yaml` → `cfgd_core::generate::validate::validate_yaml`
- `write_module_yaml` → `GenerateSession::write_module_yaml`
- `write_profile_yaml` → `GenerateSession::write_profile_yaml`
- `present_yaml` → special handling (returns to conversation loop)
- `list_generated` → `GenerateSession::list_generated`
- `get_existing_modules` → `GenerateSession::get_existing_modules`
- `get_existing_profiles` → `GenerateSession::get_existing_profiles`

- [ ] **Step 2: Write tests for tool dispatch**

Test:
- Known tool name dispatches correctly (mock the underlying function or verify the return type)
- Unknown tool name returns error result
- Input parameter deserialization from JSON works for each tool
- Tool result serialization to JSON works

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd ai::tools`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd/src/ai/tools.rs
git commit -m "feat(ai): add tool dispatch registry mapping API tool calls to generate functions"
```

---

## Task 17: Conversation management

**Files:**
- Create: `crates/cfgd/src/ai/conversation.rs`

- [ ] **Step 1: Implement conversation state**

Create `crates/cfgd/src/ai/conversation.rs`:

```rust
use crate::ai::client::{Message, ContentBlock};

pub struct Conversation {
    system_prompt: String,
    messages: Vec<Message>,
    input_tokens: u64,
    output_tokens: u64,
}
```

Implement:
- `new(system_prompt: String)` — initialize with skill text + mode context
- `add_user_message(content: String)` — append user message
- `add_assistant_message(blocks: Vec<ContentBlock>)` — append assistant response
- `add_tool_result(tool_use_id: String, content: String)` — append tool result
- `messages()` — return message slice for API call
- `system_prompt()` — return system prompt
- `track_usage(input: u64, output: u64)` — accumulate token counts
- `total_tokens() -> (u64, u64)` — return (input, output) totals

- [ ] **Step 2: Write tests**

Test:
- Message history builds correctly
- Token tracking accumulates
- System prompt construction for each mode (full, module, profile)

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd ai::conversation`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd/src/ai/conversation.rs
git commit -m "feat(ai): add conversation state management with token tracking"
```

---

## Task 18: CLI generate command

**Files:**
- Create: `crates/cfgd/src/cli/generate.rs`
- Modify: `crates/cfgd/src/cli/mod.rs`

- [ ] **Step 1: Add Clap subcommand definition**

Create `crates/cfgd/src/cli/generate.rs`:

```rust
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct GenerateArgs {
    #[command(subcommand)]
    pub target: Option<GenerateTarget>,

    /// Override AI model
    #[arg(long)]
    pub model: Option<String>,

    /// Override AI provider
    #[arg(long)]
    pub provider: Option<String>,

    /// Skip confirmation prompts
    #[arg(long, short)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum GenerateTarget {
    /// Generate a module for a specific tool
    Module {
        /// Tool name to generate module for
        name: String,
    },
    /// Generate a profile
    Profile {
        /// Profile name to generate
        name: String,
    },
}
```

- [ ] **Step 2: Add Generate variant to Command enum in cli/mod.rs**

In `crates/cfgd/src/cli/mod.rs`, add to the `Command` enum (around line 530):

```rust
    /// AI-guided configuration generation
    Generate(GenerateArgs),
```

Add to the `execute()` dispatch (around line 1150):

```rust
    Command::Generate(args) => cmd_generate(cli, printer, args),
```

- [ ] **Step 3: Implement cmd_generate**

In `crates/cfgd/src/cli/generate.rs`, implement `cmd_generate`:

1. Resolve `AiConfig` from config file + CLI flag overrides
2. Resolve API key from environment variable
3. Display consent disclosure (unless `--yes`)
4. Create `GenerateSession` with repo root
5. Build system prompt from skill + mode
6. Create `AnthropicClient` and `Conversation`
7. Enter conversation loop:
   - Send messages to API
   - Stream text responses via `Printer`
   - Dispatch tool calls via `ToolRegistry`
   - Handle `present_yaml` specially (render + prompt)
   - Loop until completion
8. Display summary and offer commit

- [ ] **Step 4: Add McpServer command variant**

Also add to the `Command` enum:

```rust
    /// Start MCP server for AI editor integration
    McpServer,
```

(Implementation in Task 20.)

- [ ] **Step 5: Verify compilation**

Run: `cargo build -p cfgd`
Expected: Compiles successfully.

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd/src/cli/generate.rs crates/cfgd/src/cli/mod.rs
git commit -m "feat(cli): add cfgd generate command with full/module/profile modes"
```

---

## Task 19: Integration test for generate conversation loop

**Files:**
- Create: `crates/cfgd/tests/generate_integration.rs` (or add to existing integration test file)

- [ ] **Step 1: Write integration test with mock API**

Create a test that:
1. Sets up a `GenerateSession` with a tempdir
2. Creates a mock HTTP handler that returns pre-scripted API responses (tool_use blocks for scan_dotfiles, then a present_yaml call, then a write_module_yaml call)
3. Runs the conversation loop against the mock
4. Verifies the module YAML was written to disk
5. Verifies the session state tracks the generated item

This tests the full tool dispatch → session → file write pipeline without a live API.

- [ ] **Step 2: Run test**

Run: `cargo test -p cfgd generate_integration`
Expected: PASS.

- [ ] **Step 3: Update docs**

Update `docs/cli-reference.md` with the `generate` command documentation.
Update `docs/bootstrap.md` with the AI-guided generation path.

- [ ] **Step 4: Commit Layer 3**

```bash
git add crates/cfgd/tests/ docs/cli-reference.md docs/bootstrap.md
git commit -m "test(generate): add integration test for conversation loop; update CLI docs"
```

---

## Task 20: MCP server — JSON-RPC transport

**Files:**
- Create: `crates/cfgd/src/mcp/mod.rs`
- Create: `crates/cfgd/src/mcp/server.rs`
- Modify: `crates/cfgd/src/main.rs`

- [ ] **Step 1: Add mcp module**

Create `crates/cfgd/src/mcp/mod.rs`:

```rust
pub mod server;
pub mod tools;
pub mod resources;
pub mod prompts;
```

Add to `crates/cfgd/src/main.rs`:

```rust
mod mcp;
```

- [ ] **Step 2: Implement JSON-RPC transport**

Create `crates/cfgd/src/mcp/server.rs`:

Implement MCP protocol over stdin/stdout:
- `McpServer` struct with session state
- JSON-RPC message parsing (id, method, params)
- Method dispatch: `initialize`, `tools/list`, `tools/call`, `resources/list`, `resources/read`, `prompts/list`, `prompts/get`
- Response serialization

Write tests for:
- JSON-RPC request parsing
- JSON-RPC response serialization
- Error response formatting
- `initialize` handshake

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd mcp::server`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd/src/mcp/ crates/cfgd/src/main.rs
git commit -m "feat(mcp): add JSON-RPC transport layer for MCP server"
```

---

## Task 21: MCP tools, resources, and prompts

**Files:**
- Create: `crates/cfgd/src/mcp/tools.rs`
- Create: `crates/cfgd/src/mcp/resources.rs`
- Create: `crates/cfgd/src/mcp/prompts.rs`

- [ ] **Step 1: Implement MCP tool definitions**

In `tools.rs`, define JSON schemas for all tools (matching the Anthropic tool-use schemas from `ai/tools.rs`) and dispatch `tools/call` requests to the same underlying functions. All tools prefixed with `cfgd_`.

- [ ] **Step 2: Implement MCP resources**

In `resources.rs`:
- `cfgd://skill/generate` → return `GENERATE_SKILL` const
- `cfgd://schema/module` → return `get_schema(SchemaKind::Module)`
- `cfgd://schema/profile` → return `get_schema(SchemaKind::Profile)`
- `cfgd://schema/config` → return `get_schema(SchemaKind::Config)`

- [ ] **Step 3: Implement MCP prompts**

In `prompts.rs`:
- `cfgd_generate(mode?, name?)` — combine skill + mode instructions
- `cfgd_generate_module(name)` — convenience wrapper
- `cfgd_generate_profile(name)` — convenience wrapper

- [ ] **Step 4: Wire McpServer command in cli/mod.rs**

In the `execute()` dispatch, handle `Command::McpServer`:

```rust
Command::McpServer => mcp::server::run_mcp_server(cli, printer),
```

- [ ] **Step 5: Write tests**

Test:
- `tools/list` returns all tool definitions with correct schemas
- `tools/call` for each tool returns expected format
- `resources/list` returns all resources
- `resources/read` for each resource returns content
- `prompts/list` returns all prompts
- `prompts/get` for each prompt returns correct content

- [ ] **Step 6: Run tests**

Run: `cargo test -p cfgd mcp`
Expected: All pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cfgd/src/mcp/
git commit -m "feat(mcp): add tool definitions, resource serving, and prompt definitions"
```

---

## Task 22: MCP server integration test

**Files:**
- Create: `crates/cfgd/tests/mcp_integration.rs`

- [ ] **Step 1: Write integration test**

Create a test that:
1. Spawns `cfgd mcp-server` as a subprocess
2. Sends JSON-RPC `initialize` request via stdin
3. Verifies `initialize` response
4. Sends `tools/list` and verifies all tools are present
5. Sends `tools/call` for `cfgd_detect_platform` and verifies response
6. Sends `resources/read` for `cfgd://skill/generate` and verifies content
7. Sends shutdown

- [ ] **Step 2: Run test**

Run: `cargo test -p cfgd mcp_integration`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cfgd/tests/mcp_integration.rs
git commit -m "test(mcp): add integration test for MCP server JSON-RPC protocol"
```

---

## Task 23: Documentation

**Files:**
- Create: `docs/ai-generate.md`
- Modify: `docs/cli-reference.md` (if not already done)

- [ ] **Step 1: Write ai-generate.md**

Comprehensive guide covering:
- What `cfgd generate` does
- Full flow walkthrough (with example output)
- Scoped generation: `cfgd generate module <name>`, `cfgd generate profile <name>`
- Configuration: `spec.ai` in cfgd.yaml
- CLI flags: `--model`, `--provider`, `--yes`
- MCP server setup:
  - Claude Code: `mcp_servers` in settings
  - Cursor: `mcp.json` configuration
  - Generic: `cfgd mcp-server` stdin/stdout
- Security model: what data is sent, what's blocked
- Troubleshooting: API key errors, model errors, common issues

- [ ] **Step 2: Verify docs build / render**

Read through the docs to verify formatting and completeness.

- [ ] **Step 3: Commit Layer 4**

```bash
git add docs/ai-generate.md docs/cli-reference.md
git commit -m "docs: add comprehensive AI generate guide and update CLI reference"
```

---

## Task 24: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt -- --check`
Expected: No formatting issues.

- [ ] **Step 4: Run audit script**

Run: `.claude/scripts/audit.sh`
Expected: No violations.

- [ ] **Step 5: Final commit if any fixups needed**

```bash
git add -A
git commit -m "fix: address clippy/fmt/audit findings from generate implementation"
```
