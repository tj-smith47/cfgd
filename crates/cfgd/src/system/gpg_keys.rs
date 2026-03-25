use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

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
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return self.expiry_ts < now;
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
            "fpr" => {
                if in_pub && current_fingerprint.is_empty() {
                    current_fingerprint = fields.get(9).map(|s| s.to_string()).unwrap_or_default();
                }
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
    let output = Command::new("gpg")
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
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "gpg --list-keys failed (exit {}): {}",
                code,
                stderr.trim_end()
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
// SystemConfigurator implementation
// ---------------------------------------------------------------------------

impl SystemConfigurator for GpgKeysConfigurator {
    fn name(&self) -> &str {
        "gpgKeys"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("gpg")
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
            let spec = match parse_key_spec(entry) {
                Some(s) => s,
                None => continue,
            };

            let keys = query_keys_for_email(&spec.email)?;
            let req_caps = required_capabilities(&spec.usage);

            // Find a key that matches the required capabilities
            let matching: Vec<&KeyringEntry> = keys
                .iter()
                .filter(|k| req_caps.iter().all(|c| k.capabilities.contains(*c)))
                .collect();

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
            let spec = match parse_key_spec(entry) {
                Some(s) => s,
                None => continue,
            };

            let keys = query_keys_for_email(&spec.email)?;
            let req_caps = required_capabilities(&spec.usage);

            let matching: Vec<&KeyringEntry> = keys
                .iter()
                .filter(|k| req_caps.iter().all(|c| k.capabilities.contains(*c)))
                .collect();

            let has_valid = matching.iter().any(|k| !k.is_expired());

            if has_valid {
                // Key exists and is valid — nothing to do
                let fingerprint = matching
                    .iter()
                    .find(|k| !k.is_expired())
                    .map(|k| k.fingerprint.as_str())
                    .unwrap_or("unknown");
                printer.info(&format!(
                    "gpgKeys: {} <{}> already present ({}), skipping",
                    spec.real_name, spec.email, fingerprint
                ));
                continue;
            }

            if !matching.is_empty() {
                // All matching keys are expired — warn but still generate
                printer.warning(&format!(
                    "gpgKeys: existing key(s) for {} <{}> are expired; generating new key",
                    spec.real_name, spec.email
                ));
            }

            // Generate key via gpg --batch --gen-key with a parameter file written to a temp file
            printer.info(&format!(
                "gpgKeys: generating {:?} key for {} <{}>",
                spec.key_type, spec.real_name, spec.email
            ));

            let param = build_param_file(&spec);

            // Write param file atomically to a temp location, pass to gpg
            let tmp_dir = std::env::temp_dir();
            let param_path = tmp_dir.join(format!(
                "cfgd-gpg-{}.params",
                cfgd_core::sha256_hex(spec.email.as_bytes())
            ));
            cfgd_core::atomic_write_str(&param_path, &param)?;

            let output = Command::new("gpg")
                .args(["--batch", "--gen-key", param_path.to_str().unwrap_or("")])
                .output()
                .map_err(CfgdError::Io)?;

            // Clean up param file (best-effort, no %no-protection but still tidy up)
            let _ = std::fs::remove_file(&param_path);

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(CfgdError::Io(std::io::Error::other(format!(
                    "gpg --batch --gen-key failed for {} <{}>: {}",
                    spec.real_name,
                    spec.email,
                    stderr.trim_end()
                ))));
            }

            // Read back the fingerprint for confirmation
            let keys_after = query_keys_for_email(&spec.email)?;
            let new_key = keys_after
                .iter()
                .filter(|k| req_caps.iter().all(|c| k.capabilities.contains(*c)))
                .find(|k| !k.is_expired());

            if let Some(k) = new_key {
                printer.success(&format!(
                    "gpgKeys: generated key for {} <{}> — fingerprint {}",
                    spec.real_name, spec.email, k.fingerprint
                ));
            } else {
                printer.warning(&format!(
                    "gpgKeys: key generated for {} <{}> but fingerprint could not be confirmed",
                    spec.real_name, spec.email
                ));
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- name() ---

    #[test]
    fn name_returns_gpg_keys() {
        assert_eq!(GpgKeysConfigurator.name(), "gpgKeys");
    }

    // --- is_available() ---

    #[test]
    fn is_available_reflects_command_available() {
        // This tests the function call itself. In CI without gpg this returns false,
        // in environments with gpg it returns true. Either way is correct.
        let avail = GpgKeysConfigurator.is_available();
        assert_eq!(avail, cfgd_core::command_available("gpg"));
    }

    // --- extract_email_from_uid ---

    #[test]
    fn extract_email_standard_uid() {
        assert_eq!(
            extract_email_from_uid("Jane Doe (work) <jane@work.com>"),
            Some("jane@work.com".to_string())
        );
    }

    #[test]
    fn extract_email_no_angle_brackets() {
        assert_eq!(extract_email_from_uid("Jane Doe"), None);
    }

    #[test]
    fn extract_email_empty_brackets() {
        assert_eq!(extract_email_from_uid("Jane <>"), None);
    }

    #[test]
    fn extract_email_no_comment() {
        assert_eq!(
            extract_email_from_uid("Jane Doe <jane@example.com>"),
            Some("jane@example.com".to_string())
        );
    }

    // --- required_capabilities ---

    #[test]
    fn required_caps_sign() {
        assert_eq!(required_capabilities("sign"), vec!['S']);
    }

    #[test]
    fn required_caps_encrypt() {
        assert_eq!(required_capabilities("encrypt"), vec!['E']);
    }

    #[test]
    fn required_caps_sign_encrypt() {
        assert_eq!(required_capabilities("sign,encrypt"), vec!['S', 'E']);
    }

    #[test]
    fn required_caps_auth() {
        assert_eq!(required_capabilities("auth"), vec!['A']);
    }

    #[test]
    fn required_caps_unknown_ignored() {
        assert_eq!(required_capabilities("certify"), Vec::<char>::new());
    }

    // --- GpgKeyType::from_str ---

    #[test]
    fn key_type_ed25519() {
        assert_eq!(GpgKeyType::from_str("ed25519"), Some(GpgKeyType::Ed25519));
    }

    #[test]
    fn key_type_rsa4096() {
        assert_eq!(GpgKeyType::from_str("rsa4096"), Some(GpgKeyType::Rsa4096));
    }

    #[test]
    fn key_type_unknown() {
        assert_eq!(GpgKeyType::from_str("dsa"), None);
    }

    // --- build_param_file ---

    #[test]
    fn param_file_ed25519() {
        let spec = GpgKeySpec {
            name: "work".to_string(),
            key_type: GpgKeyType::Ed25519,
            real_name: "Jane Doe".to_string(),
            email: "jane@work.com".to_string(),
            expiry: "2y".to_string(),
            usage: "sign".to_string(),
        };
        let param = build_param_file(&spec);
        assert!(param.contains("%no-protection"), "missing %no-protection");
        assert!(param.contains("Key-Type: eddsa"), "missing Key-Type: eddsa");
        assert!(param.contains("Key-Curve: ed25519"), "missing Key-Curve");
        assert!(param.contains("Key-Usage: sign"), "missing Key-Usage");
        assert!(param.contains("Name-Real: Jane Doe"), "missing Name-Real");
        assert!(
            param.contains("Name-Email: jane@work.com"),
            "missing Name-Email"
        );
        assert!(param.contains("Expire-Date: 2y"), "missing Expire-Date");
        assert!(param.contains("%commit"), "missing %commit");
    }

    #[test]
    fn param_file_rsa4096() {
        let spec = GpgKeySpec {
            name: "enc".to_string(),
            key_type: GpgKeyType::Rsa4096,
            real_name: "Bob Smith".to_string(),
            email: "bob@example.com".to_string(),
            expiry: "1y".to_string(),
            usage: "encrypt".to_string(),
        };
        let param = build_param_file(&spec);
        assert!(param.contains("Key-Type: rsa"), "missing Key-Type: rsa");
        assert!(
            param.contains("Key-Length: 4096"),
            "missing Key-Length: 4096"
        );
        assert!(!param.contains("ed25519"), "should not contain ed25519");
        assert!(param.contains("Key-Usage: encrypt"), "missing Key-Usage");
    }

    #[test]
    fn param_file_multi_usage() {
        let spec = GpgKeySpec {
            name: "combo".to_string(),
            key_type: GpgKeyType::Ed25519,
            real_name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            expiry: "0".to_string(),
            usage: "sign,encrypt".to_string(),
        };
        let param = build_param_file(&spec);
        assert!(
            param.contains("Key-Usage: sign encrypt"),
            "multi-usage should be space-separated"
        );
    }

    // --- parse_gpg_colon_output ---

    const SAMPLE_OUTPUT: &str = "\
pub:-:255:22:AABBCCDD11223344:1700000000:1800000000::u:::scESC:::23::\n\
fpr:::::::::AABBCCDD11223344AABBCCDD11223344AABBCCDD:\n\
uid:u::::1700000000::ABC123::Jane Doe <jane@work.com>::::::::::0:\n\
sub:-:255:18:DDCCBBAA44332211:1700000000:1800000000::u:::e:::\n\
";

    #[test]
    fn parse_colon_extracts_entry() {
        let entries = parse_gpg_colon_output(SAMPLE_OUTPUT);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.email, "jane@work.com");
        assert_eq!(e.fingerprint, "AABBCCDD11223344AABBCCDD11223344AABBCCDD");
        assert!(!e.is_revoked());
    }

    #[test]
    fn parse_colon_revoked_key() {
        let output = "\
pub:r:255:22:AABBCCDD11223344:1700000000:0::r:::scESC:::23::\n\
fpr:::::::::REVOKEDFPR:\n\
uid:r::::1700000000::ABC123::Revoked User <rev@example.com>::::::::::0:\n\
";
        let entries = parse_gpg_colon_output(output);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_revoked());
    }

    #[test]
    fn parse_colon_expired_key() {
        // expiry_ts of 1 is well in the past
        let output = "\
pub:e:255:22:AABBCCDD11223344:1000000000:1::-:::scESC:::23::\n\
fpr:::::::::EXPIREDFPR:\n\
uid:e::::1000000000::ABC123::Expired User <exp@example.com>::::::::::0:\n\
";
        let entries = parse_gpg_colon_output(output);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_expired());
    }

    #[test]
    fn parse_colon_no_expiry() {
        let output = "\
pub:u:255:22:AABBCCDD11223344:1700000000:0::u:::scESC:::23::\n\
fpr:::::::::NOEEXPIRYFPR:\n\
uid:u::::1700000000::ABC123::No Expiry <noexp@example.com>::::::::::0:\n\
";
        let entries = parse_gpg_colon_output(output);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_expired());
    }

    #[test]
    fn parse_colon_empty_output() {
        let entries = parse_gpg_colon_output("");
        assert!(entries.is_empty());
    }

    // --- diff() with empty/non-sequence desired ---

    #[test]
    fn diff_empty_sequence_returns_no_drift() {
        let configurator = GpgKeysConfigurator;
        let desired = serde_yaml::Value::Sequence(Vec::new());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn diff_non_sequence_returns_no_drift() {
        let configurator = GpgKeysConfigurator;
        let desired = serde_yaml::Value::String("not a sequence".into());
        let drifts = configurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- current_state() ---

    #[test]
    fn current_state_is_empty_sequence() {
        let state = GpgKeysConfigurator.current_state().unwrap();
        assert!(state.is_sequence());
        assert!(state.as_sequence().unwrap().is_empty());
    }

    // --- parse_key_spec ---

    #[test]
    fn parse_key_spec_all_fields() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
name: work-signing
type: ed25519
realName: "Jane Doe"
email: jane@work.com
expiry: 2y
usage: sign
"#,
        )
        .unwrap();
        let spec = parse_key_spec(&yaml).unwrap();
        assert_eq!(spec.name, "work-signing");
        assert_eq!(spec.key_type, GpgKeyType::Ed25519);
        assert_eq!(spec.real_name, "Jane Doe");
        assert_eq!(spec.email, "jane@work.com");
        assert_eq!(spec.expiry, "2y");
        assert_eq!(spec.usage, "sign");
    }

    #[test]
    fn parse_key_spec_defaults() {
        // type, expiry, usage have defaults
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
name: minimal
realName: "Alice"
email: alice@example.com
"#,
        )
        .unwrap();
        let spec = parse_key_spec(&yaml).unwrap();
        assert_eq!(spec.key_type, GpgKeyType::Ed25519);
        assert_eq!(spec.expiry, "2y");
        assert_eq!(spec.usage, "sign");
    }

    #[test]
    fn parse_key_spec_missing_email_returns_none() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
name: bad
realName: "No Email"
"#,
        )
        .unwrap();
        assert!(parse_key_spec(&yaml).is_none());
    }

    #[test]
    fn parse_key_spec_bad_type_returns_none() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"
name: bad
type: dsa512
realName: "Bad Type"
email: bad@example.com
"#,
        )
        .unwrap();
        assert!(parse_key_spec(&yaml).is_none());
    }

    // --- Integration test with isolated GNUPGHOME ---
    // Only runs when gpg binary is available.

    #[test]
    fn gpg_integration_generate_and_detect() {
        if !cfgd_core::command_available("gpg") {
            return; // skip if gpg not installed
        }

        let tmp = tempfile::tempdir().unwrap();
        let gnupghome = tmp.path().to_str().unwrap();

        // Generate a key in the isolated keyring
        let param = "%no-protection\nKey-Type: eddsa\nKey-Curve: ed25519\nKey-Usage: sign\nName-Real: Test User\nName-Email: test@cfgd.local\nExpire-Date: 1y\n%commit\n";
        let param_path = tmp.path().join("gen.params");
        std::fs::write(&param_path, param).unwrap();

        let gen_output = Command::new("gpg")
            .env("GNUPGHOME", gnupghome)
            .args(["--batch", "--gen-key", param_path.to_str().unwrap()])
            .output()
            .unwrap();

        if !gen_output.status.success() {
            // gpg key gen might fail in sandboxed envs; skip gracefully
            return;
        }

        // Now query the isolated keyring
        let list = Command::new("gpg")
            .env("GNUPGHOME", gnupghome)
            .args([
                "--list-keys",
                "--with-colons",
                "--with-fingerprint",
                "test@cfgd.local",
            ])
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&list.stdout);
        let entries = parse_gpg_colon_output(&stdout);
        let matching: Vec<&KeyringEntry> = entries
            .iter()
            .filter(|e| e.email.eq_ignore_ascii_case("test@cfgd.local") && !e.is_revoked())
            .collect();

        assert!(!matching.is_empty(), "key should be found after generation");
        let valid = matching.iter().any(|k| !k.is_expired());
        assert!(valid, "newly generated key should not be expired");
    }
}
