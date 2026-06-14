//! CRD spec types for the cfgd operator.
//!
//! The spec types, their `schemars` schemas, and the cross-field `validate()`
//! impls live in the `cfgd-crd` crate so that `cfgd-core` and the CLI can share
//! one schema/validation source with the operator. They are re-exported here so
//! every existing `crate::crds::*` consumer (controllers, webhook, gateway,
//! `gen_crds`) keeps its import paths unchanged.

pub use cfgd_crd::*;
