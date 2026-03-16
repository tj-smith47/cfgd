use serde::Serialize;

use super::db::{DeviceStatus, ServerDb};
use super::errors::GatewayError;

#[derive(Debug, Clone, Serialize)]
pub struct FleetStatus {
    pub total_devices: usize,
    pub healthy: usize,
    pub drifted: usize,
    pub offline: usize,
}

pub fn get_fleet_status(db: &ServerDb) -> Result<FleetStatus, GatewayError> {
    let devices = db.list_devices()?;
    let total_devices = devices.len();
    let mut healthy = 0usize;
    let mut drifted = 0usize;
    let mut offline = 0usize;

    for device in &devices {
        match device.status {
            DeviceStatus::Healthy => healthy += 1,
            DeviceStatus::Drifted => drifted += 1,
            DeviceStatus::Offline | DeviceStatus::PendingReconcile => offline += 1,
        }
    }

    Ok(FleetStatus {
        total_devices,
        healthy,
        drifted,
        offline,
    })
}

#[cfg(test)]
mod tests {
    use super::super::db::ServerDb;
    use super::*;

    #[test]
    fn fleet_status_empty() {
        let db = ServerDb::open(":memory:").expect("open db");
        let status = get_fleet_status(&db).expect("get status");
        assert_eq!(status.total_devices, 0);
        assert_eq!(status.healthy, 0);
    }

    #[test]
    fn fleet_status_counts() {
        let db = ServerDb::open(":memory:").expect("open db");
        db.register_device("d1", "host1", "linux", "x86_64", "h1")
            .expect("register");
        db.register_device("d2", "host2", "linux", "x86_64", "h2")
            .expect("register");
        db.record_drift_event("d2", "something drifted")
            .expect("drift");

        let status = get_fleet_status(&db).expect("get status");
        assert_eq!(status.total_devices, 2);
        assert_eq!(status.healthy, 1);
        assert_eq!(status.drifted, 1);
        assert_eq!(status.offline, 0);
    }
}
