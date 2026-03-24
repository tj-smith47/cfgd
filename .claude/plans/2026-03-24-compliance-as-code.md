# Compliance-as-Code Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add compliance capabilities to cfgd: file encryption enforcement, secret-backed env injection, key provisioning (SSH/GPG/git), and continuous compliance snapshots.

**Architecture:** Four independent subsystems extending existing patterns. Config structs gain new fields, three new system configurators follow the `SystemConfigurator` trait, the reconciler gets encryption validation and env injection, and a new compliance subsystem collects/stores/exports machine state snapshots.

**Tech Stack:** Rust, serde, clap, rusqlite (StateStore), ssh-keygen/gpg CLI (key generation), git CLI (git configurator)

**Spec:** `.claude/specs/2026-03-24-compliance-as-code-design.md`

---

## File Structure

### New files
- `crates/cfgd/src/system/ssh_keys.rs` — SshKeysConfigurator
- `crates/cfgd/src/system/gpg_keys.rs` — GpgKeysConfigurator
- `crates/cfgd/src/system/git_config.rs` — GitConfigurator
- `crates/cfgd-core/src/compliance/mod.rs` — snapshot collection, storage, types

### Modified files
- `crates/cfgd-core/src/config/mod.rs` — `ManagedFileSpec`, `SecretSpec`, `ModuleSpec`, `ConfigSpec`, `PolicyItems`, `SourceConstraints` struct changes + new types
- `crates/cfgd-core/src/providers/mod.rs` — `SecretAction` gains env variants
- `crates/cfgd/src/files/mod.rs` — encryption validation in `FileManager::plan()`
- `crates/cfgd-core/src/reconciler/mod.rs` — secret env injection in `plan_env()`, env writing changes
- `crates/cfgd-core/src/composition/mod.rs` — encryption constraint enforcement
- `crates/cfgd-core/src/state/mod.rs` — `compliance_snapshots` table migration
- `crates/cfgd-core/src/daemon/mod.rs` — compliance snapshot timer
- `crates/cfgd-core/src/lib.rs` — `parse_duration_str` extended with `d` (days)
- `crates/cfgd/src/system/mod.rs` — re-export new configurators
- `crates/cfgd/src/cli/mod.rs` — register new configurators, add `Compliance` command, module system merge

---

## Subsystem 1: File Encryption Enforcement

### Task 1: Add encryption types to config

**Files:**
- Modify: `crates/cfgd-core/src/config/mod.rs`

- [ ] **Step 1: Write failing test for EncryptionSpec deserialization**

```rust
// In the existing #[cfg(test)] mod tests {} block at bottom of config/mod.rs
#[test]
fn parse_file_encryption() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: test
spec:
  files:
    managed:
      - source: ssh/config
        target: ~/.ssh/config
        permissions: "600"
        encryption:
          backend: sops
          mode: InRepo
"#;
    let profile: ProfileDocument = serde_yaml::from_str(yaml).unwrap();
    let file = &profile.spec.files.as_ref().unwrap().managed[0];
    assert_eq!(file.permissions.as_deref(), Some("600"));
    let enc = file.encryption.as_ref().unwrap();
    assert_eq!(enc.backend, "sops");
    assert_eq!(enc.mode, EncryptionMode::InRepo);
}

#[test]
fn parse_file_encryption_always() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: test
spec:
  files:
    managed:
      - source: secrets/cert.pem
        target: /etc/ssl/cert.pem
        encryption:
          backend: age
          mode: Always
"#;
    let profile: ProfileDocument = serde_yaml::from_str(yaml).unwrap();
    let enc = profile.spec.files.as_ref().unwrap().managed[0].encryption.as_ref().unwrap();
    assert_eq!(enc.mode, EncryptionMode::Always);
}

#[test]
fn parse_file_no_encryption() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: test
spec:
  files:
    managed:
      - source: shell/.zshrc
        target: ~/.zshrc
"#;
    let profile: ProfileDocument = serde_yaml::from_str(yaml).unwrap();
    assert!(profile.spec.files.as_ref().unwrap().managed[0].encryption.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-core parse_file_encryption -- --nocapture`
Expected: FAIL — `EncryptionSpec`, `EncryptionMode` not defined, `permissions` not a field on `ManagedFileSpec`

- [ ] **Step 3: Add EncryptionMode enum and EncryptionSpec struct**

Add before `ManagedFileSpec` in `crates/cfgd-core/src/config/mod.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncryptionMode {
    InRepo,
    Always,
}

impl Default for EncryptionMode {
    fn default() -> Self {
        Self::InRepo
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionSpec {
    pub backend: String,
    #[serde(default)]
    pub mode: EncryptionMode,
}
```

- [ ] **Step 4: Add `encryption` and `permissions` fields to ManagedFileSpec**

Add to `ManagedFileSpec` (around line 1171):

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub encryption: Option<EncryptionSpec>,

#[serde(default, skip_serializing_if = "Option::is_none")]
pub permissions: Option<String>,
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cfgd-core parse_file_encryption -- --nocapture`
Expected: PASS (all three tests)

- [ ] **Step 6: Add EncryptionConstraint to SourceConstraints**

Add struct and field:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionConstraint {
    #[serde(default)]
    pub required_targets: Vec<String>,
    pub backend: Option<String>,
    pub mode: Option<EncryptionMode>,
}
```

Add to `SourceConstraints`:
```rust
#[serde(default)]
pub encryption: Option<EncryptionConstraint>,
```

- [ ] **Step 7: Write test for EncryptionConstraint deserialization**

```rust
#[test]
fn parse_source_encryption_constraint() {
    let yaml = r#"
noScripts: true
encryption:
  requiredTargets:
    - "~/.ssh/*"
    - "~/.aws/*"
  backend: sops
  mode: InRepo
"#;
    let constraints: SourceConstraints = serde_yaml::from_str(yaml).unwrap();
    let enc = constraints.encryption.unwrap();
    assert_eq!(enc.required_targets.len(), 2);
    assert_eq!(enc.backend.as_deref(), Some("sops"));
}
```

- [ ] **Step 8: Run all config tests**

Run: `cargo test -p cfgd-core config::tests -- --nocapture`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/cfgd-core/src/config/mod.rs
git commit -m "feat: add encryption enforcement types to config structs"
```

### Task 2: Validate encryption in reconciler

**Files:**
- Modify: `crates/cfgd-core/src/reconciler/mod.rs`

- [ ] **Step 1: Write failing test for encryption validation**

Add to the reconciler's `#[cfg(test)]` block:

```rust
#[test]
fn plan_rejects_unencrypted_file_with_encryption_required() {
    // Create a ManagedFileSpec with encryption: { backend: sops, mode: InRepo }
    // pointing to a source file that is NOT encrypted
    // Assert that plan_files() returns an error or a Skip action with clear message
}
```

The exact test will depend on how `plan_files()` currently works — read `crates/cfgd-core/src/reconciler/mod.rs` `plan_files()` to understand the current signature and return type before writing.

- [ ] **Step 2: Implement encryption check in plan_files()**

In the file planning loop within `plan_files()`, after resolving the source path but before creating a `FileAction::Create`/`FileAction::Update`, add:

```rust
if let Some(ref enc) = file_spec.encryption {
    if !is_file_encrypted(&source_path, &enc.backend)? {
        return Err(CfgdError::Config(format!(
            "file '{}' requires encryption (backend: {}) but source is not encrypted",
            file_spec.source, enc.backend
        )));
    }
    if enc.mode == EncryptionMode::Always
        && matches!(file_spec.strategy, Some(FileStrategy::Symlink) | Some(FileStrategy::Hardlink))
    {
        return Err(CfgdError::Config(format!(
            "file '{}' has encryption mode Always but strategy {:?} — must use Copy or Template",
            file_spec.source, file_spec.strategy
        )));
    }
}
```

Helper function (add to reconciler or a shared location):
```rust
fn is_file_encrypted(path: &Path, backend: &str) -> Result<bool> {
    match backend {
        "sops" => {
            // SOPS files contain "sops" key in YAML/JSON, or MAC header
            let content = std::fs::read_to_string(path)?;
            Ok(content.contains("\"sops\"") || content.contains("sops:"))
        }
        "age" => {
            // age files start with "age-encryption.org" header
            let content = std::fs::read_to_string(path)?;
            Ok(content.starts_with("age-encryption.org"))
        }
        _ => Err(CfgdError::Config(format!("unknown encryption backend: {}", backend))),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd-core reconciler::tests -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd-core/src/reconciler/mod.rs
git commit -m "feat: validate file encryption requirements in reconciler"
```

### Task 3: Enforce encryption constraints in composition

**Files:**
- Modify: `crates/cfgd-core/src/composition/mod.rs`

- [ ] **Step 1: Read composition/mod.rs to understand current constraint enforcement**

Read `crates/cfgd-core/src/composition/mod.rs` — find where `allowed_target_paths` and other constraints are checked. The encryption constraint check should follow the same pattern.

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn composition_rejects_unencrypted_file_matching_required_target() {
    // Create a ConfigSource with constraints.encryption.requiredTargets = ["~/.ssh/*"]
    // Create a profile with a file targeting ~/.ssh/config but no encryption block
    // Assert composition returns an error
}
```

- [ ] **Step 3: Implement encryption constraint enforcement**

In the composition function where files are merged, after the `allowed_target_paths` check, add:

```rust
if let Some(ref enc_constraint) = source_policy.constraints.encryption {
    for file in &merged_files {
        let target_str = file.target.to_string_lossy();
        let matches_required = enc_constraint.required_targets.iter().any(|pattern| {
            glob_match(pattern, &target_str)
        });
        if matches_required && file.encryption.is_none() {
            return Err(CfgdError::Composition(format!(
                "file targeting '{}' matches encryption.requiredTargets but has no encryption block (source: {})",
                target_str, source_name
            )));
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd-core composition::tests -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd-core/src/composition/mod.rs
git commit -m "feat: enforce encryption constraints in source composition"
```

---

## Subsystem 2: Secret-Backed Env Injection

### Task 4: Update SecretSpec and PolicyItems

**Files:**
- Modify: `crates/cfgd-core/src/config/mod.rs`

- [ ] **Step 1: Write failing test for optional target + envs**

```rust
#[test]
fn parse_secret_with_envs() {
    let yaml = r#"
- source: 1password://Work/GitHub/token
  envs:
    - GITHUB_TOKEN
- source: vault://secret/data/api#key
  target: ~/.config/api-key
  envs:
    - API_KEY
"#;
    let secrets: Vec<SecretSpec> = serde_yaml::from_str(yaml).unwrap();
    assert!(secrets[0].target.is_none());
    assert_eq!(secrets[0].envs.as_ref().unwrap(), &["GITHUB_TOKEN"]);
    assert!(secrets[1].target.is_some());
    assert_eq!(secrets[1].envs.as_ref().unwrap(), &["API_KEY"]);
}

#[test]
#[should_panic(expected = "at least one of `target` or `envs`")]
fn parse_secret_rejects_no_target_no_envs() {
    let yaml = r#"
- source: 1password://Work/GitHub/token
"#;
    let secrets: Vec<SecretSpec> = serde_yaml::from_str(yaml).unwrap();
    // Validation should reject this
    validate_secret_specs(&secrets).unwrap();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-core parse_secret_with_envs -- --nocapture`
Expected: FAIL — `envs` field doesn't exist, `target` is not `Option`

- [ ] **Step 3: Update SecretSpec**

Change `SecretSpec` (line ~1191):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretSpec {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    pub template: Option<String>,
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envs: Option<Vec<String>>,
}
```

Add validation function:

```rust
pub fn validate_secret_specs(specs: &[SecretSpec]) -> Result<(), CfgdError> {
    for spec in specs {
        if spec.target.is_none() && spec.envs.is_none() {
            return Err(CfgdError::Config(format!(
                "secret '{}': at least one of `target` or `envs` must be set",
                spec.source
            )));
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Fix all compilation errors from target: PathBuf → Option<PathBuf>**

Search for all uses of `SecretSpec.target` across the codebase. Each access like `spec.target` becomes `spec.target.as_ref().unwrap()` or pattern-matched. Key files:
- `crates/cfgd-core/src/reconciler/mod.rs` — `plan_secrets()`, `apply_secret()`
- `crates/cfgd-core/src/daemon/mod.rs` — drift detection
- `crates/cfgd/src/secrets/mod.rs` — backend resolution

Run: `cargo build -p cfgd-core -p cfgd 2>&1 | head -50` to find all breakages.

- [ ] **Step 5: Add `secrets` field to PolicyItems**

Add to `PolicyItems` (line ~624):

```rust
#[serde(default)]
pub secrets: Vec<SecretSpec>,
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p cfgd-core -- --nocapture`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/cfgd-core/src/config/mod.rs crates/cfgd-core/src/reconciler/mod.rs crates/cfgd-core/src/daemon/mod.rs crates/cfgd/src/secrets/mod.rs
git commit -m "feat: add envs field to SecretSpec, make target optional"
```

### Task 5: Inject secret-backed envs into env file

**Files:**
- Modify: `crates/cfgd-core/src/reconciler/mod.rs`
- Modify: `crates/cfgd-core/src/providers/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn plan_env_includes_secret_backed_vars() {
    // Create a SecretSpec with envs: ["GITHUB_TOKEN"], no target
    // Call the env planning logic
    // Assert the generated env file content includes GITHUB_TOKEN
}
```

- [ ] **Step 2: Extend SecretAction with env variant**

Add to `SecretAction` in `providers/mod.rs`:

```rust
pub enum SecretAction {
    Decrypt { source: PathBuf, target: PathBuf, backend: String, origin: String },
    Resolve { provider: String, reference: String, target: PathBuf, origin: String },
    ResolveEnv { provider: String, reference: String, envs: Vec<String>, origin: String },
    Skip { source: String, reason: String, origin: String },
}
```

- [ ] **Step 3: Handle ResolveEnv in reconciler apply phase**

In the secret apply function, when encountering `ResolveEnv`:
1. Resolve the secret value from the provider
2. Store the resolved env vars in a collection passed to `plan_env()`
3. The env file writer merges these with regular env vars

- [ ] **Step 4: Update plan_env() to accept secret-backed envs**

Modify `plan_env()` signature to accept an additional parameter:

```rust
fn plan_env(
    profile_env: &[EnvVar],
    profile_aliases: &[ShellAlias],
    modules: &[ResolvedModule],
    secret_envs: &[(String, String)],  // (name, value) pairs from resolved secrets
) -> (Vec<Action>, Vec<String>) {
```

Merge `secret_envs` into the env var list before generating the env file content.

- [ ] **Step 5: Run tests**

Run: `cargo test -p cfgd-core reconciler::tests -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd-core/src/reconciler/mod.rs crates/cfgd-core/src/providers/mod.rs
git commit -m "feat: inject secret-backed env vars into shell env file"
```

---

## Subsystem 3: Key & Credential Provisioning

### Task 6: Add system field to ModuleSpec

**Files:**
- Modify: `crates/cfgd-core/src/config/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn parse_module_with_system() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: corp-signing
spec:
  system:
    git:
      commit.gpgSign: true
      gpg.format: ssh
    sshKeys:
      - name: corp
        type: ed25519
"#;
    let module: ModuleDocument = serde_yaml::from_str(yaml).unwrap();
    assert!(module.spec.system.contains_key("git"));
    assert!(module.spec.system.contains_key("sshKeys"));
}
```

- [ ] **Step 2: Add system field to ModuleSpec**

Add to `ModuleSpec` (line ~709):

```rust
#[serde(default, skip_serializing_if = "HashMap::is_empty")]
pub system: HashMap<String, serde_yaml::Value>,
```

- [ ] **Step 3: Update module merge logic**

In the module resolution code (likely in `reconciler/mod.rs` or `config/mod.rs` merge functions), deep-merge module `system` values into the profile's `system` HashMap. Module system values override profile system values for the same configurator key (consistent with how module env/aliases override profile values).

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd-core -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd-core/src/config/mod.rs
git commit -m "feat: add system field to ModuleSpec for configurator support"
```

### Task 7: Implement SshKeysConfigurator

**Files:**
- Create: `crates/cfgd/src/system/ssh_keys.rs`
- Modify: `crates/cfgd/src/system/mod.rs`
- Modify: `crates/cfgd/src/cli/mod.rs`

- [ ] **Step 1: Write failing test**

In `ssh_keys.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::providers::SystemConfigurator;

    #[test]
    fn name_is_ssh_keys() {
        let c = SshKeysConfigurator;
        assert_eq!(c.name(), "sshKeys");
    }

    #[test]
    fn is_available_returns_true() {
        let c = SshKeysConfigurator;
        assert!(c.is_available());
    }

    #[test]
    fn diff_detects_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("id_ed25519");
        let desired = serde_yaml::from_str::<serde_yaml::Value>(r#"
- name: test
  type: ed25519
  path: KEY_PATH
"#.replace("KEY_PATH", &key_path.to_string_lossy())).unwrap();

        let c = SshKeysConfigurator;
        let drifts = c.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert!(drifts[0].key.contains("missing"));
    }
}
```

- [ ] **Step 2: Create ssh_keys.rs with SshKeysConfigurator struct**

```rust
use cfgd_core::errors::CfgdError;
use cfgd_core::output::Printer;
use cfgd_core::providers::{SystemConfigurator, SystemDrift};
use std::path::Path;

pub struct SshKeysConfigurator;

impl SystemConfigurator for SshKeysConfigurator {
    fn name(&self) -> &str { "sshKeys" }

    fn is_available(&self) -> bool { true }

    fn current_state(&self) -> Result<serde_yaml::Value, CfgdError> {
        // Read existing SSH keys from ~/.ssh/
        // Return list of { path, type, permissions }
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>, CfgdError> {
        // For each desired key entry:
        // - Check if key file exists at path
        // - If exists, check type via public key and permissions
        // - Return drifts for missing, wrong type, wrong permissions
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<(), CfgdError> {
        // For each desired key:
        // - If missing: run ssh-keygen -t <type> -f <path> -C <comment> [-N <passphrase>]
        // - If wrong permissions: chmod
        // - Create parent dir with 700 if needed
    }
}
```

- [ ] **Step 3: Add `pub mod ssh_keys;` to system/mod.rs and re-export**

- [ ] **Step 4: Register in cli/mod.rs `build_registry_with_config_and_packages()`**

```rust
registry.system_configurators.push(Box::new(crate::system::ssh_keys::SshKeysConfigurator));
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p cfgd ssh_keys -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd/src/system/ssh_keys.rs crates/cfgd/src/system/mod.rs crates/cfgd/src/cli/mod.rs
git commit -m "feat: add SshKeysConfigurator system configurator"
```

### Task 8: Implement GpgKeysConfigurator

**Files:**
- Create: `crates/cfgd/src/system/gpg_keys.rs`
- Modify: `crates/cfgd/src/system/mod.rs`
- Modify: `crates/cfgd/src/cli/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::providers::SystemConfigurator;

    #[test]
    fn name_is_gpg_keys() {
        let c = GpgKeysConfigurator;
        assert_eq!(c.name(), "gpgKeys");
    }

    #[test]
    fn is_available_checks_gpg_binary() {
        let c = GpgKeysConfigurator;
        // Available if gpg is on PATH
        let expected = cfgd_core::command_available("gpg");
        assert_eq!(c.is_available(), expected);
    }
}
```

- [ ] **Step 2: Implement GpgKeysConfigurator**

Follow same pattern as SshKeysConfigurator. Key generation uses `gpg --batch --gen-key` with a parameter file written to a tempfile:

```
%no-protection
Key-Type: eddsa
Key-Curve: ed25519
Key-Usage: sign
Name-Real: Jane Doe
Name-Email: jane@work.com
Expire-Date: 2y
%commit
```

Key matching: `gpg --list-keys --with-colons <email>`, parse for matching usage capabilities. Ignore revoked keys (flag `r` in colon-delimited output).

- [ ] **Step 3: Register and re-export**

Gate on `gpg` being available:
```rust
if cfgd_core::command_available("gpg") {
    registry.system_configurators.push(Box::new(crate::system::gpg_keys::GpgKeysConfigurator));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd gpg_keys -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd/src/system/gpg_keys.rs crates/cfgd/src/system/mod.rs crates/cfgd/src/cli/mod.rs
git commit -m "feat: add GpgKeysConfigurator system configurator"
```

### Task 9: Implement GitConfigurator

**Files:**
- Create: `crates/cfgd/src/system/git_config.rs`
- Modify: `crates/cfgd/src/system/mod.rs`
- Modify: `crates/cfgd/src/cli/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::providers::SystemConfigurator;

    #[test]
    fn name_is_git() {
        let c = GitConfigurator;
        assert_eq!(c.name(), "git");
    }

    #[test]
    fn diff_detects_wrong_value() {
        // Set a git config value via Command
        // Create desired with a different value
        // Assert diff returns a drift
    }

    #[test]
    fn diff_detects_missing_key() {
        // Use a key that definitely doesn't exist (e.g., cfgd.testkey.12345)
        // Assert diff returns a drift with actual = ""
    }
}
```

- [ ] **Step 2: Implement GitConfigurator**

```rust
pub struct GitConfigurator;

impl SystemConfigurator for GitConfigurator {
    fn name(&self) -> &str { "git" }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("git")
    }

    fn current_state(&self) -> Result<serde_yaml::Value, CfgdError> {
        // Run `git config --global --list` and parse into a Mapping
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>, CfgdError> {
        let mapping = desired.as_mapping().ok_or_else(|| {
            CfgdError::Config("git configurator expects a key-value mapping".into())
        })?;
        let mut drifts = Vec::new();
        for (key, value) in mapping {
            let key_str = key.as_str().unwrap_or_default();
            let desired_val = value_to_git_string(value);
            let actual_val = git_config_get(key_str).unwrap_or_default();
            if actual_val != desired_val {
                drifts.push(SystemDrift {
                    key: key_str.to_string(),
                    expected: desired_val,
                    actual: actual_val,
                });
            }
        }
        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<(), CfgdError> {
        let mapping = desired.as_mapping().ok_or_else(|| {
            CfgdError::Config("git configurator expects a key-value mapping".into())
        })?;
        for (key, value) in mapping {
            let key_str = key.as_str().unwrap_or_default();
            let val_str = value_to_git_string(value);
            printer.step(&format!("git config --global {} {}", key_str, val_str));
            std::process::Command::new("git")
                .args(["config", "--global", key_str, &val_str])
                .output()
                .map_err(|e| CfgdError::System(format!("git config failed: {}", e)))?;
        }
        Ok(())
    }
}

fn git_config_get(key: &str) -> Option<String> {
    std::process::Command::new("git")
        .args(["config", "--global", "--get", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn value_to_git_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        other => format!("{}", serde_yaml::to_string(other).unwrap_or_default().trim()),
    }
}
```

- [ ] **Step 3: Register (always available when git is on PATH)**

```rust
if cfgd_core::command_available("git") {
    registry.system_configurators.push(Box::new(crate::system::git_config::GitConfigurator));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cfgd git_config -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd/src/system/git_config.rs crates/cfgd/src/system/mod.rs crates/cfgd/src/cli/mod.rs
git commit -m "feat: add GitConfigurator system configurator"
```

---

## Subsystem 4: Compliance Snapshots

### Task 10: Add ComplianceConfig to ConfigSpec

**Files:**
- Modify: `crates/cfgd-core/src/config/mod.rs`
- Modify: `crates/cfgd-core/src/lib.rs`

- [ ] **Step 1: Extend parse_duration_str to support days**

In `crates/cfgd-core/src/lib.rs`, find `parse_duration_str` and add `d` support:

```rust
"d" => Ok(Duration::from_secs(num * 86400)),
```

Write test:
```rust
#[test]
fn parse_duration_days() {
    let d = parse_duration_str("30d").unwrap();
    assert_eq!(d, Duration::from_secs(30 * 86400));
}
```

- [ ] **Step 2: Add ComplianceConfig structs**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_compliance_interval")]
    pub interval: String,
    #[serde(default = "default_compliance_retention")]
    pub retention: String,
    #[serde(default)]
    pub scope: ComplianceScope,
    #[serde(default)]
    pub export: ComplianceExport,
}

fn default_compliance_interval() -> String { "1h".to_string() }
fn default_compliance_retention() -> String { "30d".to_string() }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceScope {
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_true")]
    pub packages: bool,
    #[serde(default = "default_true")]
    pub system: bool,
    #[serde(default = "default_true")]
    pub secrets: bool,
    #[serde(default)]
    pub watch_paths: Vec<String>,
    #[serde(default)]
    pub watch_package_managers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceExport {
    #[serde(default = "default_json")]
    pub format: String,
    #[serde(default = "default_compliance_path")]
    pub path: String,
}

fn default_json() -> String { "json".to_string() }
fn default_compliance_path() -> String { "~/.local/share/cfgd/compliance/".to_string() }

impl Default for ComplianceExport {
    fn default() -> Self {
        Self { format: default_json(), path: default_compliance_path() }
    }
}
```

- [ ] **Step 3: Add `compliance` field to ConfigSpec**

```rust
#[serde(default)]
pub compliance: Option<ComplianceConfig>,
```

- [ ] **Step 4: Write deserialization test**

```rust
#[test]
fn parse_compliance_config() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
    interval: 1h
    retention: 30d
    scope:
      watchPaths:
        - ~/.ssh
      watchPackageManagers:
        - brew
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let c = config.spec.compliance.unwrap();
    assert!(c.enabled);
    assert_eq!(c.scope.watch_paths, vec!["~/.ssh"]);
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p cfgd-core -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/cfgd-core/src/config/mod.rs crates/cfgd-core/src/lib.rs
git commit -m "feat: add ComplianceConfig to ConfigSpec, extend parse_duration_str with days"
```

### Task 11: Create compliance snapshot module

**Files:**
- Create: `crates/cfgd-core/src/compliance/mod.rs`
- Modify: `crates/cfgd-core/src/lib.rs` (add `pub mod compliance;`)
- Modify: `crates/cfgd-core/src/state/mod.rs` (add migration)

- [ ] **Step 1: Define snapshot types**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceSnapshot {
    pub timestamp: String,
    pub machine: MachineInfo,
    pub profile: String,
    pub sources: Vec<String>,
    pub checks: Vec<ComplianceCheck>,
    pub summary: ComplianceSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceCheck {
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub status: ComplianceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ComplianceStatus {
    Compliant,
    Warning,
    Violation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceSummary {
    pub compliant: usize,
    pub warning: usize,
    pub violation: usize,
}
```

- [ ] **Step 2: Implement snapshot collection**

```rust
use crate::config::{ComplianceScope, MergedProfile};
use crate::providers::ProviderRegistry;

pub fn collect_snapshot(
    profile: &MergedProfile,
    registry: &ProviderRegistry,
    scope: &ComplianceScope,
    sources: &[String],
) -> Result<ComplianceSnapshot, crate::errors::CfgdError> {
    let mut checks = Vec::new();

    if scope.files {
        checks.extend(collect_file_checks(profile)?);
    }
    if scope.packages {
        checks.extend(collect_package_checks(profile, registry)?);
    }
    if scope.system {
        checks.extend(collect_system_checks(profile, registry)?);
    }
    if scope.secrets {
        checks.extend(collect_secret_checks(profile)?);
    }
    for path in &scope.watch_paths {
        checks.extend(collect_watch_path_checks(path)?);
    }

    let summary = ComplianceSummary {
        compliant: checks.iter().filter(|c| c.status == ComplianceStatus::Compliant).count(),
        warning: checks.iter().filter(|c| c.status == ComplianceStatus::Warning).count(),
        violation: checks.iter().filter(|c| c.status == ComplianceStatus::Violation).count(),
    };

    Ok(ComplianceSnapshot {
        timestamp: crate::utc_now_iso8601(),
        machine: MachineInfo {
            hostname: hostname::get().unwrap_or_default().to_string_lossy().to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        profile: profile.name.clone(),
        sources: sources.to_vec(),
        checks,
        summary,
    })
}
```

Individual `collect_*_checks` functions query the reconciler's diff logic to determine compliant/warning/violation status for each managed resource.

- [ ] **Step 3: Add compliance_snapshots table to StateStore**

Add migration in `crates/cfgd-core/src/state/mod.rs`:

```sql
CREATE TABLE IF NOT EXISTS compliance_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    summary_compliant INTEGER NOT NULL,
    summary_warning INTEGER NOT NULL,
    summary_violation INTEGER NOT NULL
);
```

Add methods to StateStore:
```rust
pub fn store_compliance_snapshot(&self, snapshot: &ComplianceSnapshot, hash: &str) -> Result<()>
pub fn latest_compliance_hash(&self) -> Result<Option<String>>
pub fn compliance_history(&self, since: Option<&str>, limit: u32) -> Result<Vec<ComplianceSnapshotRecord>>
pub fn get_compliance_snapshot(&self, id: i64) -> Result<Option<ComplianceSnapshot>>
pub fn prune_compliance_snapshots(&self, retention: std::time::Duration) -> Result<usize>
```

- [ ] **Step 4: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serializes_to_json() {
        let snapshot = ComplianceSnapshot {
            timestamp: "2026-03-24T14:30:00Z".to_string(),
            machine: MachineInfo { hostname: "test".into(), os: "linux".into(), arch: "x86_64".into() },
            profile: "work".into(),
            sources: vec!["acme-corp".into()],
            checks: vec![ComplianceCheck {
                category: "file".into(),
                target: Some("~/.ssh/config".into()),
                status: ComplianceStatus::Compliant,
                detail: Some("permissions 600".into()),
                ..Default::default()
            }],
            summary: ComplianceSummary { compliant: 1, warning: 0, violation: 0 },
        };
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        assert!(json.contains("compliant"));
    }

    #[test]
    fn state_store_compliance_roundtrip() {
        let store = StateStore::open_in_memory().unwrap();
        // Store a snapshot, retrieve it, verify fields match
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/cfgd-core/src/compliance/ crates/cfgd-core/src/lib.rs crates/cfgd-core/src/state/mod.rs
git commit -m "feat: add compliance snapshot collection and storage"
```

### Task 12: Add compliance CLI commands

**Files:**
- Modify: `crates/cfgd/src/cli/mod.rs`

- [ ] **Step 1: Add Compliance command variant**

```rust
/// Compliance status and evidence export
Compliance {
    #[command(subcommand)]
    command: Option<ComplianceCommand>,
},
```

```rust
#[derive(Subcommand)]
pub enum ComplianceCommand {
    /// Export compliance snapshot to file or stdout
    Export,
    /// Show compliance snapshot history
    History {
        #[arg(long)]
        since: Option<String>,
    },
    /// Show diff between two snapshots
    Diff {
        id1: i64,
        id2: i64,
    },
}
```

- [ ] **Step 2: Implement handlers**

`cfgd compliance` (no subcommand) — run snapshot, print summary table.
`cfgd compliance export` — run snapshot, write to export path (or stdout with `-o json`).
`cfgd compliance history` — query StateStore, print table.
`cfgd compliance diff <id1> <id2>` — load two snapshots, diff checks arrays, print changes.

- [ ] **Step 3: Run tests**

Run: `cargo test -p cfgd -- --nocapture && cargo build -p cfgd`
Expected: PASS + successful build

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd/src/cli/mod.rs
git commit -m "feat: add cfgd compliance CLI commands"
```

### Task 13: Integrate compliance snapshots into daemon

**Files:**
- Modify: `crates/cfgd-core/src/daemon/mod.rs`

- [ ] **Step 1: Add compliance timer to daemon main loop**

In the daemon's `tokio::select!` loop, add a compliance interval timer alongside the existing reconcile and sync timers:

```rust
let compliance_interval = config.spec.compliance
    .as_ref()
    .filter(|c| c.enabled)
    .map(|c| parse_duration_str(&c.interval).unwrap_or(Duration::from_secs(3600)));

// In the select! loop:
_ = compliance_tick.tick(), if compliance_interval.is_some() => {
    let snapshot = collect_snapshot(&profile, &registry, &scope, &source_names)?;
    let hash = sha256_hex(serde_json::to_string(&snapshot)?.as_bytes());
    let prev_hash = state_store.latest_compliance_hash()?;
    if prev_hash.as_deref() != Some(&hash) {
        // State changed — write snapshot
        state_store.store_compliance_snapshot(&snapshot, &hash)?;
        if let Some(ref export) = compliance_config.export {
            write_snapshot_to_file(&snapshot, export)?;
        }
    }
    // Prune old snapshots
    let retention = parse_duration_str(&compliance_config.retention)?;
    state_store.prune_compliance_snapshots(retention)?;
}
```

- [ ] **Step 2: Write test for daemon compliance integration**

Test that the daemon loop calls snapshot collection when compliance is enabled and skips when disabled.

- [ ] **Step 3: Run full test suite**

Run: `cargo test --workspace -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/cfgd-core/src/daemon/mod.rs
git commit -m "feat: integrate compliance snapshots into daemon reconciliation loop"
```

---

## Final Verification

### Task 14: Full build and test verification

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

- [ ] **Step 2: Run fmt**

Run: `cargo fmt --check`
Expected: No formatting issues

- [ ] **Step 3: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass

- [ ] **Step 4: Build all binaries**

Run: `cargo build --workspace`
Expected: Clean build

- [ ] **Step 5: Run audit script**

Run: `.claude/scripts/audit.sh`
Expected: No violations from new code

- [ ] **Step 6: Update docs**

Update these documentation files to reflect new config fields:
- `docs/spec/profile.md` — add `encryption` and `permissions` fields to files.managed, add `envs` to secrets
- `docs/spec/module.md` — add `system` field
- `docs/spec/config.md` — add `compliance` section
- `docs/system-configurators.md` — add sshKeys, gpgKeys, git sections
- `docs/secrets.md` — document env injection
- `docs/configuration.md` — document compliance config

- [ ] **Step 7: Final commit**

```bash
git add docs/
git commit -m "docs: update specs and guides for compliance-as-code features"
```

---

## Review Corrections

The following corrections were identified during plan review. Implementing agents MUST read these before starting work.

### C1. File encryption validation lives in `crates/cfgd/src/files/mod.rs`, NOT the reconciler

Task 2 references `plan_files()` in the reconciler — this function does not exist there. File planning is done by `FileManager::plan()` in `crates/cfgd/src/files/mod.rs` (around line 124). The encryption check must be added there, not in `reconciler/mod.rs`. Read `files/mod.rs` first to understand the actual function signature and flow.

### C2. Error types must match actual crate error enums

The plan uses `CfgdError::Config(format!(...))`, `CfgdError::Composition(format!(...))`, and `CfgdError::System(format!(...))` throughout. These are WRONG — `CfgdError` uses `#[from]` variants with typed inner errors, not bare strings. `CfgdError::System` does not exist at all.

Before writing error returns, read `crates/cfgd-core/src/errors/mod.rs` to find the actual variants. You will likely need to add new variants (e.g., `ConfigError::EncryptionRequired { path, backend }`, `CompositionError::EncryptionConstraintViolation { target, source }`) or use existing generic variants. Follow the project's `thiserror` pattern.

### C3. `MergedProfile` has no `name` field

Task 11's `collect_snapshot()` references `profile.name.clone()`. `MergedProfile` has no `name` field. Pass the profile name as a separate `&str` parameter:

```rust
pub fn collect_snapshot(
    profile_name: &str,
    profile: &MergedProfile,
    registry: &ProviderRegistry,
    scope: &ComplianceScope,
    sources: &[String],
) -> Result<ComplianceSnapshot, CfgdError> {
```

### C4. `ComplianceScope` needs manual `Default` impl

The plan derives `Default` on `ComplianceScope` while also using `#[serde(default = "default_true")]` on bool fields. `#[derive(Default)]` sets bools to `false`, but the spec says they default to `true`. Remove `#[derive(Default)]` and write a manual impl:

```rust
impl Default for ComplianceScope {
    fn default() -> Self {
        Self {
            files: true,
            packages: true,
            system: true,
            secrets: true,
            watch_paths: Vec::new(),
            watch_package_managers: Vec::new(),
        }
    }
}
```

### C5. `ModuleFileEntry` also needs `encryption` field

The plan adds `encryption` only to `ManagedFileSpec` (profile files). Module files use `ModuleFileEntry` (config/mod.rs line ~756), a different struct. The spec says encryption constraints apply to "all files (from any origin — profile or module)." Add `encryption: Option<EncryptionSpec>` to `ModuleFileEntry` as well, and ensure the composition constraint check covers module files.

### C6. `validate_secret_specs` must be wired into config loading

The plan creates a standalone `validate_secret_specs()` function but does not specify where it's called. Wire it into the config/profile loading path — likely in `parse_profile()` or wherever `ProfileSpec` is deserialized. An unvalidated secret spec (no target, no envs) should fail at load time, not silently pass through to reconciliation.

### C7. `ComplianceCheck` needs `#[derive(Default)]`

Task 11's test uses `..Default::default()` on `ComplianceCheck` but the struct definition does not derive `Default`. Add it. Also, `ComplianceStatus` needs a default — use `Compliant`.

### C8. `ComplianceExport.format` should be an enum

The plan uses `pub format: String`. Use a proper enum instead:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComplianceFormat {
    #[default]
    Json,
    Yaml,
}
```

### C9. GitConfigurator tests must not pollute global git config

Task 9 tests that set/read git config values will pollute the CI runner's global config. Use the `GIT_CONFIG_GLOBAL` environment variable pointed at a temp file:

```rust
let dir = tempfile::tempdir().unwrap();
let git_config = dir.path().join(".gitconfig");
std::env::set_var("GIT_CONFIG_GLOBAL", &git_config);
// ... run test ...
std::env::remove_var("GIT_CONFIG_GLOBAL");
```

Or use `git config --file <tempfile>` instead of `--global` in the configurator, with the file path configurable for testing.

### C10. `is_file_encrypted` detection should be robust

The plan's SOPS detection (`content.contains("sops:")`) is fragile — a comment containing "sops:" would false-positive. For SOPS YAML/JSON files, parse the YAML and check for a top-level `sops` key with `mac` and `lastmodified` sub-keys. For age files, the `age-encryption.org` header check is correct.

### C11. Skeleton tests must be implemented, not left as comments

Tasks 2, 3, and 5 contain test skeletons with comment-only bodies. These will pass as empty tests and give false confidence. The implementing agent MUST write actual test assertions using `tempfile` for filesystem state and mock data for config structs. Read existing test patterns in `config/mod.rs` and `reconciler/mod.rs` before writing.

### C12. Task 5 secret env injection needs more detail

The plan hand-waves the plumbing between secret resolution and env file generation. The actual flow must be:
1. In reconciler `plan()`, identify secrets with `envs` field
2. During apply phase, resolve those secrets via `SecretProvider::resolve()`
3. Collect `(env_name, resolved_value)` pairs
4. Pass them to `plan_env()` (which gains a 4th parameter)
5. `generate_env_file_content()` includes them alongside regular env vars

Note: changing `plan_env()` from 3 to 4 parameters will break existing callers. Find ALL call sites via `cargo build` output and update them.

### C13. Task 11 `collect_*_checks` functions need implementation detail

Each collection function must:
- `collect_file_checks`: iterate `profile.files.managed`, check source encryption status and target permissions against spec
- `collect_package_checks`: iterate managed packages, query package managers for installed state/version
- `collect_system_checks`: iterate `profile.system`, call each configurator's `diff()`, map drifts to checks
- `collect_secret_checks`: iterate `profile.secrets`, verify targets exist with correct permissions (never read values)
- `collect_watch_path_checks`: stat each watch path, report permissions and whether the path is managed

Read the existing reconciler `plan_*` functions for each resource type — the compliance checks mirror their diff logic but return `ComplianceCheck` instead of `Action`.

### C14. Read daemon code before integrating compliance timer

Task 13 uses pseudo-code for the daemon loop. Before implementing, read `crates/cfgd-core/src/daemon/mod.rs` completely to understand the actual `tokio::select!` structure, timer patterns, and how existing intervals (reconcile, sync) are configured. Follow the same pattern exactly.
