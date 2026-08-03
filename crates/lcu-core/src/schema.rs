//! Versioned JSON schema metadata for external and internal contracts.

use serde::{Deserialize, Serialize};

use crate::protocol::PROTOCOL_SCHEMA_VERSION;

/// Machine-readable schema document shipped with the product.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaDocument {
    /// Contract schema version (semver-like string).
    pub schema_version: String,
    /// Product name for consumers.
    pub product: String,
    /// Internal private IPC protocol version.
    pub internal_protocol_version: u32,
}

impl SchemaDocument {
    pub fn current() -> Self {
        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION.to_string(),
            product: "local-computer-use".to_string(),
            internal_protocol_version: crate::protocol::InternalProtocolVersion::CURRENT.0,
        }
    }
}

