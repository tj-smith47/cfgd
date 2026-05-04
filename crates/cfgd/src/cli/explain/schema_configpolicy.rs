use super::{ResourceSchema, SchemaField};

pub(super) static SCHEMA_CONFIGPOLICY: ResourceSchema = ResourceSchema {
    name: "ConfigPolicy",
    api_version: cfgd_core::API_VERSION,
    kind: "ConfigPolicy",
    location: "Kubernetes CRD (cfgd-operator)",
    description: "Kubernetes Custom Resource defining fleet-wide configuration baselines. Machines are checked for compliance against policies.",
    fields: &[
        SchemaField {
            name: "name",
            type_desc: "string",
            required: true,
            description: "Policy name",
            children: &[],
        },
        SchemaField {
            name: "requiredModules",
            type_desc: "[]string",
            required: false,
            description: "Modules that must be installed",
            children: &[],
        },
        SchemaField {
            name: "packages",
            type_desc: "[]PackageRef",
            required: false,
            description: "Required packages (each entry has name and optional version constraint)",
            children: &[
                SchemaField {
                    name: "name",
                    type_desc: "string",
                    required: true,
                    description: "Package name",
                    children: &[],
                },
                SchemaField {
                    name: "version",
                    type_desc: "string",
                    required: false,
                    description: "Semver version requirement (e.g. \">=1.28\", \"~2.40\")",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "settings",
            type_desc: "map[string]string",
            required: false,
            description: "Required system settings",
            children: &[],
        },
        SchemaField {
            name: "targetSelector",
            type_desc: "map[string]string",
            required: false,
            description: "Label selector to match target MachineConfigs",
            children: &[],
        },
    ],
};
