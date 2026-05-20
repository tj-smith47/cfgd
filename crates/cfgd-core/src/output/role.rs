use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Ok,
    Warn,
    Fail,
    Pending,
    Running,
    Skipped,
    Info,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        let json = serde_json::to_string(&Role::Ok).unwrap();
        assert_eq!(json, "\"ok\"");
        let json = serde_json::to_string(&Role::Fail).unwrap();
        assert_eq!(json, "\"fail\"");
    }

    #[test]
    fn role_round_trips() {
        for r in [
            Role::Ok,
            Role::Warn,
            Role::Fail,
            Role::Pending,
            Role::Running,
            Role::Skipped,
            Role::Info,
        ] {
            let s = serde_json::to_string(&r).unwrap();
            let back: Role = serde_json::from_str(&s).unwrap();
            assert_eq!(r, back);
        }
    }
}
