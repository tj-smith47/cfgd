use std::collections::HashMap;

use crate::config::{PolicyItems, ProfileSpec};

pub(super) fn has_content(items: &PolicyItems) -> bool {
    items.packages.is_some()
        || !items.files.is_empty()
        || !items.env.is_empty()
        || !items.aliases.is_empty()
        || !items.system.is_empty()
        || !items.profiles.is_empty()
        || !items.modules.is_empty()
        || !items.secrets.is_empty()
}

pub(super) fn policy_items_to_spec(items: &PolicyItems) -> ProfileSpec {
    ProfileSpec {
        packages: items.packages.clone(),
        files: if items.files.is_empty() {
            None
        } else {
            Some(crate::config::FilesSpec {
                managed: items.files.clone(),
                permissions: HashMap::new(),
            })
        },
        env: items.env.clone(),
        aliases: items.aliases.clone(),
        system: items.system.clone(),
        modules: items.modules.clone(),
        secrets: items.secrets.clone(),
        ..Default::default()
    }
}
