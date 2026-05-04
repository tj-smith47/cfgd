use super::{ResourceSchema, SchemaField};

pub(super) static SCHEMA_DRIFTALERT: ResourceSchema = ResourceSchema {
    name: "DriftAlert",
    api_version: cfgd_core::API_VERSION,
    kind: "DriftAlert",
    location: "Kubernetes CRD (cfgd-operator)",
    description: "Kubernetes Custom Resource created when a machine drifts from its desired state. Tracks drift details and resolution status.",
    fields: &[
        SchemaField {
            name: "deviceId",
            type_desc: "string",
            required: true,
            description: "Device identifier",
            children: &[],
        },
        SchemaField {
            name: "machineConfigRef",
            type_desc: "string",
            required: true,
            description: "Reference to the MachineConfig resource",
            children: &[],
        },
        SchemaField {
            name: "driftDetails",
            type_desc: "[]object",
            required: false,
            description: "Individual drift items",
            children: &[
                SchemaField {
                    name: "field",
                    type_desc: "string",
                    required: true,
                    description: "Field that drifted",
                    children: &[],
                },
                SchemaField {
                    name: "expected",
                    type_desc: "string",
                    required: true,
                    description: "Expected value",
                    children: &[],
                },
                SchemaField {
                    name: "actual",
                    type_desc: "string",
                    required: true,
                    description: "Actual observed value",
                    children: &[],
                },
            ],
        },
        SchemaField {
            name: "severity",
            type_desc: "string",
            required: true,
            description: "Drift severity: low | medium | high | critical",
            children: &[],
        },
    ],
};
