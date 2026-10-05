pub mod app;
pub mod cache;
pub mod errors;
pub mod identity;
pub mod metrics;
pub mod node;

pub mod csi {
    #[allow(
        clippy::doc_overindented_list_items,
        clippy::doc_lazy_continuation,
        clippy::derive_partial_eq_without_eq,
        clippy::double_must_use
    )]
    pub mod v1 {
        tonic::include_proto!("csi.v1");
    }
}
