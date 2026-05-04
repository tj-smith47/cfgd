use super::{ResourceSchema, SchemaField};

pub(super) static POLICY_ITEMS_FIELDS: [SchemaField; 6] = [
    SchemaField {
        name: "packages",
        type_desc: "object",
        required: false,
        description: "Package declarations (same schema as profile packages)",
        children: &[],
    },
    SchemaField {
        name: "files",
        type_desc: "[]object",
        required: false,
        description: "Managed file declarations",
        children: &[],
    },
    SchemaField {
        name: "env",
        type_desc: "list[{name, value}]",
        required: false,
        description: "Environment variable declarations",
        children: &[],
    },
    SchemaField {
        name: "system",
        type_desc: "map[string]any",
        required: false,
        description: "System configurator settings",
        children: &[],
    },
    SchemaField {
        name: "profiles",
        type_desc: "[]string",
        required: false,
        description: "Profiles in this tier",
        children: &[],
    },
    SchemaField {
        name: "modules",
        type_desc: "[]string",
        required: false,
        description: "Modules in this tier",
        children: &[],
    },
];

pub(super) static SCHEMA_MACHINECONFIG: ResourceSchema = ResourceSchema {
    name: "MachineConfig",
    api_version: cfgd_core::API_VERSION,
    kind: "MachineConfig",
    location: "Kubernetes CRD (cfgd-operator)",
    description: "Kubernetes Custom Resource representing a managed machine's desired and observed configuration state.",
    fields: &[
        SchemaField {
            name: "hostname",
            type_desc: "string",
            required: true,
            description: "Machine hostname",
            children: &[],
        },
        SchemaField {
            name: "profile",
            type_desc: "string",
            required: true,
            description: "Active profile name",
            children: &[],
        },
        SchemaField {
            name: "moduleRefs",
            type_desc: "[]object",
            required: false,
            description: "Modules that should be installed",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Module name",
                    children: &[],
                },
                SchemaField {
                    name: "required",
                    type_desc: "bool",
                    required: false,
                    description: "Whether the module is required (default: false)",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "packages",
            type_desc: "[]string",
            required: false,
            description: "Required packages",
            children: &[],
        },
        SchemaField {
            name: "packageVersions",
            type_desc: "map[string]string",
            required: false,
            description: "Reported installed versions by package name",
            children: &[],
        },
        SchemaField {
            name: "files",
            type_desc: "[]object",
            required: false,
            description: "Managed files",
            children: &[
                SchemaField {
                    name: "path",
                    type_desc: "string",
                    required: true,
                    description: "File path on the machine",
                    children: &[],
                },
                SchemaField {
                    name: "content",
                    type_desc: "string",
                    required: false,
                    description: "Inline file content",
                    children: &[],
                },
                SchemaField {
                    name: "source",
                    type_desc: "string",
                    required: false,
                    description: "Source reference",
                    children: &[],
                },
                SchemaField {
                    name: "mode",
                    type_desc: "string",
                    required: false,
                    description: "File mode in octal (default: 0644)",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "systemSettings",
            type_desc: "map[string]string",
            required: false,
            description: "System configurator settings",
            children: &[],
        },
    ],
};
