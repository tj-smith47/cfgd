use serde::{Deserialize, Serialize};

/// Device status as a proper enum with well-defined states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceStatus {
    Healthy,
    Drifted,
    Offline,
    PendingReconcile,
}

impl DeviceStatus {
    pub fn as_str(&self) -> &str {
        match self {
            DeviceStatus::Healthy => "healthy",
            DeviceStatus::Drifted => "drifted",
            DeviceStatus::Offline => "offline",
            DeviceStatus::PendingReconcile => "pending-reconcile",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "healthy" => DeviceStatus::Healthy,
            "drifted" => DeviceStatus::Drifted,
            "offline" => DeviceStatus::Offline,
            "pending-reconcile" => DeviceStatus::PendingReconcile,
            _ => DeviceStatus::Offline,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub last_checkin: String,
    pub config_hash: String,
    pub status: DeviceStatus,
    pub desired_config: Option<serde_json::Value>,
    pub compliance_summary: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftEvent {
    pub id: String,
    pub device_id: String,
    pub timestamp: String,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinEvent {
    pub id: String,
    pub device_id: String,
    pub timestamp: String,
    pub config_hash: String,
    pub config_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetEvent {
    pub timestamp: String,
    pub device_id: String,
    pub event_type: String,
    pub summary: String,
}

/// A bootstrap token record — admin-created, one-time use for device enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapToken {
    pub id: String,
    pub username: String,
    pub team: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub used_at: Option<String>,
    pub used_by_device: Option<String>,
}

/// A device credential record — permanent API key for an enrolled device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredential {
    pub device_id: String,
    pub username: String,
    pub team: Option<String>,
    pub created_at: String,
    pub last_used: Option<String>,
    pub revoked: bool,
}

/// A user's public key for key-based enrollment verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPublicKey {
    pub id: String,
    pub username: String,
    pub key_type: String, // "ssh" or "gpg"
    pub public_key: String,
    pub fingerprint: String,
    pub label: Option<String>,
    pub created_at: String,
}

/// A short-lived enrollment challenge for key-based enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentChallenge {
    pub id: String,
    pub username: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub nonce: String,
    pub created_at: String,
    pub expires_at: String,
}
