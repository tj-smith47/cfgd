use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

/// Test seam env var for redirecting `gpg` invocations to a shim binary.
/// Production code never sets this; tests point it at a `/bin/sh` shim
/// installed via `cfgd_core::test_helpers::ToolShim`.
const GPG_BIN_ENV: &str = "CFGD_GPG_BIN";

/// Build a `Command` for `gpg`, honoring [`GPG_BIN_ENV`] for tests. Mirrors
/// the `cosign_cmd` / `tool_cmd` pattern used elsewhere in the codebase.
fn gpg_cmd() -> Command {
    cfgd_core::tool_cmd(GPG_BIN_ENV, "gpg")
}

// ---------------------------------------------------------------------------
// GpgKeysConfigurator
// ---------------------------------------------------------------------------

/// Manages GPG key provisioning.
///
/// Config format:
/// ```yaml
/// system:
///   gpgKeys:
///     - name: work-signing
///       type: ed25519          # ed25519 | rsa4096
///       realName: "Jane Doe"
///       email: jane@work.com
///       expiry: 2y             # gpg expiry notation (e.g. 1y, 6m, 0 = no expiry)
///       usage: sign            # sign | encrypt | auth | sign,encrypt
/// ```
///
/// Key matching is on **primary UID email** and **usage capabilities**.
/// Revoked keys are ignored.
///
/// - No matching key → generate via `gpg --batch --gen-key`.
/// - Key exists but expired → drift.
/// - Key exists and valid → compliant.
pub struct GpgKeysConfigurator;

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// A single GPG key declaration from config.
#[derive(Debug)]
pub(crate) struct GpgKeySpec {
    name: String,
    key_type: GpgKeyType,
    real_name: String,
    email: String,
    expiry: String,
    usage: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpgKeyType {
    Ed25519,
    Rsa4096,
}

impl GpgKeyType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "ed25519" => Some(Self::Ed25519),
            "rsa4096" => Some(Self::Rsa4096),
            _ => None,
        }
    }
}

/// Parsed information about a key found in the keyring.
#[derive(Debug)]
struct KeyringEntry {
    fingerprint: String,
    email: String,
    /// Validity character from gpg --list-keys --with-colons (field 2 of pub line)
    validity: char,
    /// Expiry timestamp (Unix seconds) or 0 if no expiry
    expiry_ts: u64,
    /// Capability string from the pub record (field 12), e.g. "SC", "E", "A"
    capabilities: String,
}

impl KeyringEntry {
    fn is_revoked(&self) -> bool {
        self.validity == 'r'
    }

    fn is_expired(&self) -> bool {
        // gpg marks expired keys with validity 'e', but also check timestamp
        if self.validity == 'e' {
            return true;
        }
        if self.expiry_ts > 0 {
            return self.expiry_ts < cfgd_core::unix_secs_now();
        }
        false
    }
}

// ---------------------------------------------------------------------------
// GPG colon-record parsing
// ---------------------------------------------------------------------------

/// Parse `gpg --list-keys --with-colons --with-fingerprint <email>` output
/// into a list of `KeyringEntry` values.
///
/// The format is documented at https://git.gnupg.org/cgi-bin/gitweb.cgi?p=gnupg.git;a=blob;f=doc/DETAILS
/// Relevant record types used here:
///   - `pub` : public key record
///   - `uid` : user ID record (email extracted here)
///   - `fpr` : fingerprint record
fn parse_gpg_colon_output(output: &str) -> Vec<KeyringEntry> {
    let mut entries: Vec<KeyringEntry> = Vec::new();

    // We build entries per `pub` block. State carried across lines:
    let mut current_validity: char = '-';
    let mut current_expiry_ts: u64 = 0;
    let mut current_capabilities: String = String::new();
    let mut current_fingerprint: String = String::new();
    let mut current_email: String = String::new();
    let mut in_pub = false;

    for line in output.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.is_empty() {
            continue;
        }

        match fields[0] {
            "pub" => {
                // Flush previous entry
                if in_pub && !current_fingerprint.is_empty() {
                    entries.push(KeyringEntry {
                        fingerprint: current_fingerprint.clone(),
                        email: current_email.clone(),
                        validity: current_validity,
                        expiry_ts: current_expiry_ts,
                        capabilities: current_capabilities.clone(),
                    });
                }
                // Start new pub block
                in_pub = true;
                current_validity = fields.get(1).and_then(|s| s.chars().next()).unwrap_or('-');
                current_expiry_ts = fields
                    .get(6)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                // Field index 11 (0-based) holds key capabilities
                current_capabilities = fields.get(11).map(|s| s.to_uppercase()).unwrap_or_default();
                current_fingerprint = String::new();
                current_email = String::new();
            }
            "fpr" if in_pub && current_fingerprint.is_empty() => {
                current_fingerprint = fields.get(9).map(|s| s.to_string()).unwrap_or_default();
            }
            "uid" => {
                // uid validity (field 1), user-id string (field 9)
                // Extract email from "Real Name (comment) <email@example.com>"
                if in_pub
                    && current_email.is_empty()
                    && let Some(uid_str) = fields.get(9)
                    && let Some(email) = extract_email_from_uid(uid_str)
                {
                    current_email = email;
                }
            }
            _ => {}
        }
    }

    // Flush final entry
    if in_pub && !current_fingerprint.is_empty() {
        entries.push(KeyringEntry {
            fingerprint: current_fingerprint,
            email: current_email,
            validity: current_validity,
            expiry_ts: current_expiry_ts,
            capabilities: current_capabilities,
        });
    }

    entries
}

/// Extract `email@example.com` from a GPG UID string such as
/// `"Real Name (comment) <email@example.com>"`.
fn extract_email_from_uid(uid: &str) -> Option<String> {
    let start = uid.rfind('<')?;
    let end = uid.rfind('>')?;
    if start < end {
        let email = uid[start + 1..end].trim();
        if email.is_empty() {
            return None;
        }
        Some(email.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Key lookup helpers
// ---------------------------------------------------------------------------

/// Convert a usage string like `"sign,encrypt"` into a set of required
/// capability characters that must appear in the GPG capabilities field.
///
/// GPG capability characters: S = sign, E = encrypt, A = authenticate
fn required_capabilities(usage: &str) -> Vec<char> {
    let mut caps = Vec::new();
    for part in usage.split(',') {
        match part.trim() {
            "sign" => caps.push('S'),
            "encrypt" => caps.push('E'),
            "auth" => caps.push('A'),
            _ => {}
        }
    }
    caps
}

/// Query the keyring for keys matching `email`. Returns all non-revoked entries.
///
/// Exit code 2 from gpg means no keys matched — treated as an empty result.
/// Any other non-zero exit code is an error.
fn query_keys_for_email(email: &str) -> Result<Vec<KeyringEntry>> {
    let output = gpg_cmd()
        .args(["--list-keys", "--with-colons", "--with-fingerprint", email])
        .output()
        .map_err(CfgdError::Io)?;

    match output.status.code() {
        Some(0) => {} // success — continue to parse
        Some(2) | None => {
            // exit 2 = no keys found (normal); None = terminated by signal (treat as empty)
            return Ok(Vec::new());
        }
        Some(code) => {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "gpg --list-keys failed (exit {}): {}",
                code,
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let all = parse_gpg_colon_output(&stdout);
    // Filter to only entries where email matches (gpg may return additional keys)
    let filtered = all
        .into_iter()
        .filter(|e| e.email.eq_ignore_ascii_case(email) && !e.is_revoked())
        .collect();
    Ok(filtered)
}

/// Build a GPG batch parameter file for key generation.
pub(crate) fn build_param_file(spec: &GpgKeySpec) -> String {
    let key_type_line = match spec.key_type {
        GpgKeyType::Ed25519 => "Key-Type: eddsa\nKey-Curve: ed25519",
        GpgKeyType::Rsa4096 => "Key-Type: rsa\nKey-Length: 4096",
    };

    // Normalise usage into GPG usage string
    let gpg_usage = spec
        .usage
        .split(',')
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "%no-protection\n{}\nKey-Usage: {}\nName-Real: {}\nName-Email: {}\nExpire-Date: {}\n%commit\n",
        key_type_line, gpg_usage, spec.real_name, spec.email, spec.expiry,
    )
}

// ---------------------------------------------------------------------------
// Config parsing helpers
// ---------------------------------------------------------------------------

fn parse_key_spec(entry: &serde_yaml::Value) -> Option<GpgKeySpec> {
    let name = entry.get("name")?.as_str()?.to_string();
    let type_str = entry
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("ed25519");
    let key_type = GpgKeyType::from_str(type_str)?;
    let real_name = entry.get("realName")?.as_str()?.to_string();
    let email = entry.get("email")?.as_str()?.to_string();
    let expiry = entry
        .get("expiry")
        .and_then(|v| v.as_str())
        .unwrap_or("2y")
        .to_string();
    let usage = entry
        .get("usage")
        .and_then(|v| v.as_str())
        .unwrap_or("sign")
        .to_string();

    Some(GpgKeySpec {
        name,
        key_type,
        real_name,
        email,
        expiry,
        usage,
    })
}

// ---------------------------------------------------------------------------
// Shared key resolution helper
// ---------------------------------------------------------------------------

/// Parse a key spec from config, query the keyring for matching keys, and
/// filter by required capabilities. Returns `None` if the entry cannot be parsed.
fn resolve_matching_keys(
    entry: &serde_yaml::Value,
) -> Result<Option<(GpgKeySpec, Vec<KeyringEntry>)>> {
    let spec = match parse_key_spec(entry) {
        Some(s) => s,
        None => return Ok(None),
    };

    let keys = query_keys_for_email(&spec.email)?;
    let req_caps = required_capabilities(&spec.usage);

    let matching: Vec<KeyringEntry> = keys
        .into_iter()
        .filter(|k| req_caps.iter().all(|c| k.capabilities.contains(*c)))
        .collect();

    Ok(Some((spec, matching)))
}

// ---------------------------------------------------------------------------
// SystemConfigurator implementation
// ---------------------------------------------------------------------------

impl SystemConfigurator for GpgKeysConfigurator {
    fn name(&self) -> &str {
        "gpgKeys"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available_with_seam(GPG_BIN_ENV, "gpg")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        // Return an empty sequence; actual state is interrogated on demand in diff().
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let entries = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };

        let mut drifts = Vec::new();

        for entry in entries {
            let (spec, matching) = match resolve_matching_keys(entry)? {
                Some(pair) => pair,
                None => continue,
            };

            if matching.is_empty() {
                drifts.push(SystemDrift {
                    key: format!("gpgKeys.{}.presence", spec.name),
                    expected: format!(
                        "key for {} <{}> with usage={} present",
                        spec.real_name, spec.email, spec.usage
                    ),
                    actual: "not found in keyring".to_string(),
                });
                continue;
            }

            // Check that at least one matching key is not expired
            let all_expired = matching.iter().all(|k| k.is_expired());
            if all_expired {
                let fingerprints: Vec<&str> =
                    matching.iter().map(|k| k.fingerprint.as_str()).collect();
                drifts.push(SystemDrift {
                    key: format!("gpgKeys.{}.expiry", spec.name),
                    expected: "key not expired".to_string(),
                    actual: format!("key(s) expired: {}", fingerprints.join(", ")),
                });
            }
            // If at least one valid key exists, no drift for this entry.
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let entries = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(()),
        };

        for entry in entries {
            let (spec, matching) = match resolve_matching_keys(entry)? {
                Some(pair) => pair,
                None => continue,
            };

            let has_valid = matching.iter().any(|k| !k.is_expired());

            if has_valid {
                // Key exists and is valid — nothing to do
                let fingerprint = matching
                    .iter()
                    .find(|k| !k.is_expired())
                    .map(|k| k.fingerprint.as_str())
                    .unwrap_or("unknown");
                printer.status_simple(
                    Role::Info,
                    format!(
                        "gpgKeys: {} <{}> already present ({}), skipping",
                        spec.real_name, spec.email, fingerprint
                    ),
                );
                continue;
            }

            if !matching.is_empty() {
                // All matching keys are expired — warn but still generate
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "gpgKeys: existing key(s) for {} <{}> are expired; generating new key",
                        spec.real_name, spec.email
                    ),
                );
            }

            // Generate key via gpg --batch --gen-key with a parameter file written to a temp file
            printer.status_simple(
                Role::Info,
                format!(
                    "gpgKeys: generating {:?} key for {} <{}>",
                    spec.key_type, spec.real_name, spec.email
                ),
            );

            let param = build_param_file(&spec);

            // Write param file atomically to a temp location, pass to gpg
            let tmp_dir = std::env::temp_dir();
            let param_path = tmp_dir.join(format!(
                "cfgd-gpg-{}.params",
                cfgd_core::sha256_hex(spec.email.as_bytes())
            ));
            cfgd_core::atomic_write_str(&param_path, &param)?;

            let mut cmd = gpg_cmd();
            cmd.args(["--batch", "--gen-key", param_path.to_str().unwrap_or("")]);

            let output = printer
                .run(
                    &mut cmd,
                    format!("Generating GPG key for {} <{}>", spec.real_name, spec.email),
                )
                .map_err(CfgdError::Io)?;

            // Clean up param file (best-effort, no %no-protection but still tidy up)
            let _ = std::fs::remove_file(&param_path);

            if !output.status.success() {
                return Err(CfgdError::Io(std::io::Error::other(format!(
                    "gpg --batch --gen-key failed for {} <{}>: {}",
                    spec.real_name, spec.email, output.stderr,
                ))));
            }

            // Read back the fingerprint for confirmation
            let keys_after = query_keys_for_email(&spec.email)?;
            let post_caps = required_capabilities(&spec.usage);
            let new_key = keys_after
                .iter()
                .filter(|k| post_caps.iter().all(|c| k.capabilities.contains(*c)))
                .find(|k| !k.is_expired());

            if let Some(k) = new_key {
                printer.status_simple(
                    Role::Ok,
                    format!(
                        "gpgKeys: generated key for {} <{}> — fingerprint {}",
                        spec.real_name, spec.email, k.fingerprint
                    ),
                );
            } else {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "gpgKeys: key generated for {} <{}> but failed to confirm fingerprint",
                        spec.real_name, spec.email
                    ),
                );
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
