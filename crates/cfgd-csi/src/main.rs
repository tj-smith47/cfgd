pub mod errors;

pub mod csi {
    #[allow(
        clippy::doc_overindented_list_items,
        clippy::doc_lazy_continuation,
        clippy::derive_partial_eq_without_eq
    )]
    pub mod v1 {
        tonic::include_proto!("csi.v1");
    }
}

fn main() {}
