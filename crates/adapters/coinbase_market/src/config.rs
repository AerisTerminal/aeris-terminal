//! Bounded adapter configuration.

use crate::errors::CoinbaseError;

/// Maximum products per subscription.
pub const MAXIMUM_PRODUCTS: usize = 64;
/// Maximum accepted WebSocket message.
pub const MAXIMUM_MESSAGE_BYTES: usize = 262_144;
/// Validated bounded configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoinbaseConfig {
    pub products: Vec<String>,
    pub maximum_message_bytes: usize,
}

impl CoinbaseConfig {
    /// Creates a bounded subscription configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, duplicated, or malformed products.
    pub fn try_new(products: Vec<String>) -> Result<Self, CoinbaseError> {
        if products.is_empty() || products.len() > MAXIMUM_PRODUCTS {
            return Err(CoinbaseError::InvalidConfiguration);
        }
        let mut seen = std::collections::BTreeSet::new();
        for product in &products {
            if product.is_empty()
                || product.len() > 32
                || !product
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                || !seen.insert(product)
            {
                return Err(CoinbaseError::InvalidConfiguration);
            }
        }
        Ok(Self {
            products,
            maximum_message_bytes: MAXIMUM_MESSAGE_BYTES,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CoinbaseConfig, MAXIMUM_PRODUCTS};

    #[test]
    fn configuration_bounds() {
        assert!(CoinbaseConfig::try_new(vec!["BTC-USD".to_string()]).is_ok());
        assert!(CoinbaseConfig::try_new(Vec::new()).is_err());
        assert!(
            CoinbaseConfig::try_new(vec!["BTC-USD".to_string(), "BTC-USD".to_string()]).is_err()
        );
        assert!(CoinbaseConfig::try_new(vec!["BTC USD".to_string()]).is_err());
        assert!(CoinbaseConfig::try_new(vec!["X".to_string(); MAXIMUM_PRODUCTS + 1]).is_err());
    }
}
