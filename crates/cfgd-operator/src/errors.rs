use thiserror::Error;

#[derive(Debug, Error)]
pub enum OperatorError {
    #[error("Kubernetes API error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Reconciliation failed: {0}")]
    Reconciliation(String),

    #[error("Invalid spec: {0}")]
    InvalidSpec(String),

    #[error("Webhook error: {0}")]
    Webhook(String),

    #[error("Health server error: {0}")]
    Health(String),
}
