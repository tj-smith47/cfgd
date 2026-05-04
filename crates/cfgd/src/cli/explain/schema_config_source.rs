use super::schema_machineconfig::POLICY_ITEMS_FIELDS;
use super::{ResourceSchema, SchemaField};

pub(super) static SCHEMA_CONFIG_SOURCE: ResourceSchema = ResourceSchema {
    name: "ConfigSource",
    api_version: cfgd_core::API_VERSION,
    kind: "ConfigSource",
    location: "cfgd-source.yaml (in source repo root)",
    description: "Team config source manifest. Published by teams in their config repos to define profiles, modules, and policy tiers available for subscription.",
    fields: &[
        SchemaField {
            name: "provides",
            type_desc: "object",
            required: false,
            description: "What this source provides",
            children: &[
                SchemaField {
                    name: "profiles",
                    type_desc: "[]string",
                    required: false,
                    description: "Profile names available from this source",
                    children: &[],
                },
                SchemaField {
                    name: "profileDetails",
                    type_desc: "[]object",
                    required: false,
                    description: "Detailed profile entries with descriptions",
                    children: &[
                        SchemaField {
                            name: "name",
                            type_desc: "string",
                            required: true,
                            description: "Profile name",
                            children: &[],
                        },
                        SchemaField {
                            name: "description",
                            type_desc: "string",
                            required: false,
                            description: "Profile description",
                            children: &[],
                        },
                        SchemaField {
                            name: "path",
                            type_desc: "string",
                            required: false,
                            description: "Path to profile YAML",
                            children: &[],
                        },
                        SchemaField {
                            name: "inherits",
                            type_desc: "[]string",
                            required: false,
                            description: "Profiles this inherits from",
                            children: &[],
                        },
                    ],
                },
                SchemaField {
                    name: "platformProfiles",
                    type_desc: "map[string]string",
                    required: false,
                    description: "OS/distro to profile mapping for auto-detection",
                    children: &[],
                },
                SchemaField {
                    name: "modules",
                    type_desc: "[]string",
                    required: false,
                    description: "Module names available from this source",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "policy",
            type_desc: "object",
            required: false,
            description: "Policy tiers controlling how items are applied",
            children: &[
                SchemaField {
                    name: "required",
                    type_desc: "object",
                    required: false,
                    description: "Items that must be applied (enforced)",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "recommended",
                    type_desc: "object",
                    required: false,
                    description: "Items that are recommended (prompted)",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "optional",
                    type_desc: "object",
                    required: false,
                    description: "Items that are opt-in",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "locked",
                    type_desc: "object",
                    required: false,
                    description: "Items that cannot be overridden by subscribers",
                    children: &POLICY_ITEMS_FIELDS,
                },
                SchemaField {
                    name: "constraints",
                    type_desc: "object",
                    required: false,
                    description: "Security constraints on source capabilities",
                    children: &[
                        SchemaField {
                            name: "noScripts",
                            type_desc: "bool",
                            required: false,
                            description: "Disallow scripts from this source (default: true)",
                            children: &[],
                        },
                        SchemaField {
                            name: "noSecretsRead",
                            type_desc: "bool",
                            required: false,
                            description: "Disallow secret reading (default: true)",
                            children: &[],
                        },
                        SchemaField {
                            name: "allowedTargetPaths",
                            type_desc: "[]string",
                            required: false,
                            description: "Restrict file targets to these path prefixes",
                            children: &[],
                        },
                        SchemaField {
                            name: "allowSystemChanges",
                            type_desc: "bool",
                            required: false,
                            description: "Allow system configurator changes (default: false)",
                            children: &[],
                        },
                    ],
                },
            ],
        },
    ],
};
