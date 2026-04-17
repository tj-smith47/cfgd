use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FleetStatus {
    pub total_devices: usize,
    pub healthy: usize,
    pub drifted: usize,
    pub offline: usize,
}

#[cfg(test)]
mod tests {
    use super::super::db::ServerDb;

    fn test_db() -> (ServerDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("test.db");
        let db = ServerDb::open(path.to_str().expect("utf8")).expect("open");
        (db, tmp)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_status_empty() {
        let (db, _tmp) = test_db();
        let status = db.get_fleet_status().await.expect("get status");
        assert_eq!(status.total_devices, 0);
        assert_eq!(status.healthy, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_status_counts() {
        let (db, _tmp) = test_db();
        db.register_device("d1", "host1", "linux", "x86_64", "h1", None)
            .await
            .expect("register");
        db.register_device("d2", "host2", "linux", "x86_64", "h2", None)
            .await
            .expect("register");
        db.record_drift_event("d2", "something drifted")
            .await
            .expect("drift");

        let status = db.get_fleet_status().await.expect("get status");
        assert_eq!(status.total_devices, 2);
        assert_eq!(status.healthy, 1);
        assert_eq!(status.drifted, 1);
        assert_eq!(status.offline, 0);
    }
}
