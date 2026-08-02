//! Re-exports the shared log-masking primitives so existing call sites keep
//! `crate::masker::...`. The single source of truth lives in `shared::log_masking`.

pub use shared::log_masking::{mask_secrets, SecretCatalog};
