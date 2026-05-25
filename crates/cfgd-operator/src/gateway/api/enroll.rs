//! Enrollment endpoints + signature verification.
//!
//! Token-based enrollment (`enroll`), discovery (`enroll_info`), and the
//! key-based challenge/response flow (`request_challenge`, `verify_enrollment`)
//! along with the `ssh-keygen` / `gpg --verify` driver helpers.

use super::*;

pub(super) async fn enroll(
    State(state): State<SharedState>,
    Json(req): Json<EnrollRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if state.enrollment_method != EnrollmentMethod::Token {
        return Err(GatewayError::InvalidRequest(
            "bootstrap token enrollment is not enabled — this server uses key-based enrollment"
                .to_string(),
        ));
    }
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;
    if req.token.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "token must not be empty".to_string(),
        ));
    }

    // Generate the per-device API key outside the transaction — pure computation.
    let device_api_key = generate_token("cfgd_dev");
    let api_key_hash = hash_token(&device_api_key);
    let token_hash = hash_token(&req.token);

    let db = state.db.clone();
    let device_id = req.device_id.clone();
    let hostname = req.hostname.clone();
    let os = req.os.clone();
    let arch = req.arch.clone();
    let api_key_hash_c = api_key_hash.clone();
    let token_hash_c = token_hash.clone();

    let (bootstrap, desired_config) = db
        .with_write_tx(move |tx| {
            let bootstrap = crate::gateway::db::validate_and_consume_bootstrap_token_tx(
                tx,
                &token_hash_c,
                &device_id,
            )?;
            crate::gateway::db::register_device_tx(
                tx,
                &device_id,
                &hostname,
                &os,
                &arch,
                "pending-enrollment",
                None,
            )?;
            crate::gateway::db::create_device_credential_tx(
                tx,
                &device_id,
                &api_key_hash_c,
                &bootstrap.username,
                bootstrap.team.as_deref(),
            )?;
            let desired = crate::gateway::db::get_device_tx(tx, &device_id)
                .ok()
                .and_then(|d| d.desired_config);
            Ok((bootstrap, desired))
        })
        .await?;

    let _ = state.event_tx.send(FleetEvent {
        timestamp: cfgd_core::utc_now_iso8601(),
        device_id: req.device_id.clone(),
        event_type: "enrollment".to_string(),
        summary: format!("user={} hostname={}", bootstrap.username, req.hostname),
    });

    tracing::info!(
        device_id = %req.device_id,
        hostname = %req.hostname,
        username = %bootstrap.username,
        "device enrolled"
    );

    if let Some(ref m) = state.metrics {
        m.devices_enrolled_total.inc();
    }

    Ok((
        StatusCode::CREATED,
        Json(EnrollResponse {
            status: "enrolled".to_string(),
            device_id: req.device_id,
            api_key: device_api_key,
            username: bootstrap.username,
            team: bootstrap.team,
            desired_config,
        }),
    ))
}
pub(super) async fn enroll_info(
    State(state): State<SharedState>,
) -> Result<impl IntoResponse, GatewayError> {
    Ok(Json(EnrollInfoResponse {
        method: state.enrollment_method.clone(),
    }))
}
pub(super) async fn request_challenge(
    State(state): State<SharedState>,
    Json(req): Json<ChallengeRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if state.enrollment_method != EnrollmentMethod::Key {
        return Err(GatewayError::InvalidRequest(
            "key-based enrollment is not enabled — this server uses bootstrap token enrollment"
                .to_string(),
        ));
    }
    validate_username(&req.username)?;
    validate_device_id(&req.device_id)?;
    validate_hostname(&req.hostname)?;

    // Verify user has at least one public key registered
    let keys = state.db.list_user_public_keys(&req.username).await?;
    if keys.is_empty() {
        return Err(GatewayError::InvalidRequest(format!(
            "no public keys registered for user '{}' — ask your admin to add your SSH or GPG key",
            req.username
        )));
    }

    let nonce = generate_token("cfgd_ch");

    let challenge = state
        .db
        .create_enrollment_challenge(
            &req.username,
            &req.device_id,
            &req.hostname,
            (&req.os, &req.arch),
            &nonce,
            CHALLENGE_TTL_SECS,
        )
        .await?;

    tracing::info!(
        username = %req.username,
        device_id = %req.device_id,
        challenge_id = %challenge.id,
        "enrollment challenge issued"
    );

    Ok((
        StatusCode::CREATED,
        Json(ChallengeResponse {
            challenge_id: challenge.id,
            nonce,
            expires_at: challenge.expires_at,
        }),
    ))
}

/// Verify a signed enrollment challenge and enroll the device (key mode only).
pub(super) async fn verify_enrollment(
    State(state): State<SharedState>,
    Json(req): Json<VerifyRequest>,
) -> Result<impl IntoResponse, GatewayError> {
    if state.enrollment_method != EnrollmentMethod::Key {
        return Err(GatewayError::InvalidRequest(
            "key-based enrollment is not enabled".to_string(),
        ));
    }
    if req.challenge_id.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "challenge_id must not be empty".to_string(),
        ));
    }
    if req.signature.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "signature must not be empty".to_string(),
        ));
    }
    if !matches!(req.key_type.as_str(), "ssh" | "gpg") {
        return Err(GatewayError::InvalidRequest(
            "key_type must be 'ssh' or 'gpg'".to_string(),
        ));
    }

    // Atomically consume the challenge
    let challenge = state
        .db
        .consume_enrollment_challenge(&req.challenge_id)
        .await?;

    // Get user's public keys of the matching type
    let keys = state.db.list_user_public_keys(&challenge.username).await?;
    let matching_keys: Vec<_> = keys.iter().filter(|k| k.key_type == req.key_type).collect();

    if matching_keys.is_empty() {
        return Err(GatewayError::InvalidRequest(format!(
            "no {} keys registered for user '{}'",
            req.key_type, challenge.username
        )));
    }

    // Verify signature against each matching key until one succeeds.
    // Verification involves blocking I/O (temp files + subprocess) — run off the async runtime.
    let nonce = challenge.nonce.clone();
    let signature = req.signature.clone();
    let key_type = req.key_type.clone();
    let owned_keys: Vec<crate::gateway::db::UserPublicKey> =
        matching_keys.iter().map(|k| (*k).clone()).collect();
    let verified = match tokio::task::spawn_blocking(move || {
        let key_refs: Vec<_> = owned_keys.iter().collect();
        match key_type.as_str() {
            "ssh" => verify_ssh_signature(&nonce, &signature, &key_refs),
            "gpg" => verify_gpg_signature(&nonce, &signature, &key_refs),
            _ => false,
        }
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                is_panic = e.is_panic(),
                is_cancelled = e.is_cancelled(),
                "signature verification blocking task failed to join",
            );
            return Err(GatewayError::Internal(
                "signature verification task failed".into(),
            ));
        }
    };

    if !verified {
        return Err(GatewayError::Unauthorized);
    }

    let device_api_key = generate_token("cfgd_dev");
    let api_key_hash = hash_token(&device_api_key);

    // Look up team from TeamConfig/user keys metadata (use first key's data)
    let team: Option<String> = None; // Key-based enrollment doesn't carry team info in the key itself

    // Signature verified — enroll the device (same flow as bootstrap token enrollment)
    let db_clone = state.db.clone();
    let device_id_c = challenge.device_id.clone();
    let hostname_c = challenge.hostname.clone();
    let os_c = challenge.os.clone();
    let arch_c = challenge.arch.clone();
    let api_key_hash_c = api_key_hash.clone();
    let username_c = challenge.username.clone();
    let team_c = team.clone();
    let desired_config = db_clone
        .with_write_tx(move |tx| {
            crate::gateway::db::register_device_tx(
                tx,
                &device_id_c,
                &hostname_c,
                &os_c,
                &arch_c,
                "pending-enrollment",
                None,
            )?;
            crate::gateway::db::create_device_credential_tx(
                tx,
                &device_id_c,
                &api_key_hash_c,
                &username_c,
                team_c.as_deref(),
            )?;
            let desired = crate::gateway::db::get_device_tx(tx, &device_id_c)
                .ok()
                .and_then(|d| d.desired_config);
            Ok(desired)
        })
        .await?;

    let _ = state.event_tx.send(FleetEvent {
        timestamp: cfgd_core::utc_now_iso8601(),
        device_id: challenge.device_id.clone(),
        event_type: "enrollment".to_string(),
        summary: format!(
            "user={} hostname={} method=key",
            challenge.username, challenge.hostname
        ),
    });

    tracing::info!(
        device_id = %challenge.device_id,
        hostname = %challenge.hostname,
        username = %challenge.username,
        "device enrolled via key verification"
    );

    if let Some(ref m) = state.metrics {
        m.devices_enrolled_total.inc();
    }

    Ok((
        StatusCode::CREATED,
        Json(EnrollResponse {
            status: "enrolled".to_string(),
            device_id: challenge.device_id,
            api_key: device_api_key,
            username: challenge.username,
            team,
            desired_config,
        }),
    ))
}

/// Verify an SSH signature using `ssh-keygen -Y verify`.
pub(super) fn verify_ssh_signature(
    nonce: &str,
    signature_pem: &str,
    keys: &[&crate::gateway::db::UserPublicKey],
) -> bool {
    let tmp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "failed to create temp dir for SSH verification");
            return false;
        }
    };

    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.sig");

    if let Err(e) = std::fs::write(&data_path, nonce) {
        tracing::error!(error = %e, "failed to write challenge data for SSH verification");
        return false;
    }
    if let Err(e) = std::fs::write(&sig_path, signature_pem) {
        tracing::error!(error = %e, "failed to write signature file for SSH verification");
        return false;
    }

    // Try each key until one verifies. Per-iteration `allowed_signers_{idx}`
    // file mirrors the GPG sibling's per-key homedir pattern: prevents any
    // cross-key contamination if this code is ever exercised concurrently
    // (today the loop is sequential, but the asymmetry was an audit finding).
    for (idx, key) in keys.iter().enumerate() {
        let signers_path = tmp_dir.path().join(format!("allowed_signers_{idx}"));
        // Write allowed_signers file: "username key_type key_data"
        // The public_key field is the full OpenSSH public key line (e.g. "ssh-ed25519 AAAA... comment")
        let signer_line = format!("{} {}", key.username, key.public_key);
        if let Err(e) = std::fs::write(&signers_path, &signer_line) {
            tracing::warn!(error = %e, key_user = %key.username, "ssh verify: failed to write allowed_signers");
            continue;
        }

        let data_file = match std::fs::File::open(&data_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "failed to open challenge data file");
                return false;
            }
        };

        let result = std::process::Command::new("ssh-keygen")
            .args([
                "-Y",
                "verify",
                "-f",
                &signers_path.to_string_lossy(),
                "-I",
                &key.username,
                "-n",
                "cfgd-enroll",
                "-s",
                &sig_path.to_string_lossy(),
            ])
            .stdin(data_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => {
                tracing::info!(
                    username = %key.username,
                    fingerprint = %key.fingerprint,
                    "SSH signature verified"
                );
                return true;
            }
            Ok(_) => {
                tracing::debug!(
                    fingerprint = %key.fingerprint,
                    "SSH signature did not match this key"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "ssh-keygen failed — is OpenSSH installed?");
                continue;
            }
        }
    }

    false
}

/// Verify a GPG signature using `gpg --verify`.
pub(super) fn verify_gpg_signature(
    nonce: &str,
    signature_armored: &str,
    keys: &[&crate::gateway::db::UserPublicKey],
) -> bool {
    let tmp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "failed to create temp dir for GPG verification");
            return false;
        }
    };

    let data_path = tmp_dir.path().join("challenge.txt");
    let sig_path = tmp_dir.path().join("challenge.txt.asc");
    let key_path = tmp_dir.path().join("pubkey.asc");

    if let Err(e) = std::fs::write(&data_path, nonce) {
        tracing::error!(error = %e, "gpg verify: failed to write challenge data");
        return false;
    }
    if let Err(e) = std::fs::write(&sig_path, signature_armored) {
        tracing::error!(error = %e, "gpg verify: failed to write signature");
        return false;
    }

    for (idx, key) in keys.iter().enumerate() {
        // Per-iteration homedir — sharing one keyring across iterations
        // lets `gpg --verify` accept a signature from ANY previously-imported
        // key, breaking the "this key signed the nonce" attribution in logs.
        let gpg_home = tmp_dir.path().join(format!("gnupg-{idx}"));
        if let Err(e) = std::fs::create_dir_all(&gpg_home) {
            tracing::warn!(error = %e, key_user = %key.username, "gpg verify: failed to create per-key homedir");
            continue;
        }
        let _ = cfgd_core::set_file_permissions(&gpg_home, 0o700);

        if let Err(e) = std::fs::write(&key_path, &key.public_key) {
            tracing::warn!(error = %e, key_user = %key.username, "gpg verify: failed to write public key");
            continue;
        }

        // Import the public key. Capture stderr so the debug log is actionable
        // when an import fails (missing key material, bad permissions, unknown
        // packet version, etc.) instead of discarding the underlying gpg error.
        let import = std::process::Command::new("gpg")
            .args([
                "--homedir",
                &gpg_home.to_string_lossy(),
                "--batch",
                "--yes",
                "--import",
                &key_path.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::null())
            .output();

        match import {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                tracing::debug!(
                    fingerprint = %key.fingerprint,
                    status = ?o.status.code(),
                    stderr = %cfgd_core::stderr_lossy_trimmed(&o),
                    "failed to import GPG key",
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    fingerprint = %key.fingerprint,
                    "gpg --import invocation failed",
                );
                continue;
            }
        }

        // Verify the signature against THIS iteration's single-key keyring.
        // Success here means the signature verifies under exactly `key`,
        // so the `fingerprint = %key.fingerprint` we log below is truthful.
        let verify = std::process::Command::new("gpg")
            .args([
                "--homedir",
                &gpg_home.to_string_lossy(),
                "--batch",
                "--verify",
                &sig_path.to_string_lossy(),
                &data_path.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match verify {
            Ok(status) if status.success() => {
                tracing::info!(
                    username = %key.username,
                    fingerprint = %key.fingerprint,
                    "GPG signature verified"
                );
                return true;
            }
            Ok(_) => {
                tracing::debug!(
                    fingerprint = %key.fingerprint,
                    "GPG signature did not match this key"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "gpg failed — is GPG installed?");
                return false;
            }
        }
    }

    false
}
