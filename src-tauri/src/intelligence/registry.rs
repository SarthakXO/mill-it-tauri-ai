use super::types::SchemaRegistry;
use serde_json::Value;

const SCHEMA: &str = include_str!("../../resources/intelligence/core/schema.json");
const RELATIONSHIPS: &str = include_str!("../../resources/intelligence/core/relationships.json");
const TERMINOLOGY: &str = include_str!("../../resources/intelligence/core/terminology.json");
const PII_POLICY: &str = include_str!("../../resources/intelligence/core/pii-policy.json");
const OPERATIONS: &str = include_str!("../../resources/intelligence/domains/operations.json");
const CUSTOMERS: &str = include_str!("../../resources/intelligence/domains/customers.json");
const PRODUCTS: &str = include_str!("../../resources/intelligence/domains/products.json");
const ORDER_METRICS: &str = include_str!("../../resources/intelligence/metrics/orders.json");
const CUSTOMER_METRICS: &str = include_str!("../../resources/intelligence/metrics/customers.json");
const PRODUCT_METRICS: &str = include_str!("../../resources/intelligence/metrics/products.json");

pub(crate) struct IntelligenceRegistry {
    pub(crate) schema: SchemaRegistry,
    pub(crate) relationships: Value,
    pub(crate) terminology: Value,
    pub(crate) pii_policy: Value,
    pub(crate) operations: Value,
    pub(crate) customers: Value,
    pub(crate) products: Value,
    pub(crate) order_metrics: Value,
    pub(crate) customer_metrics: Value,
    pub(crate) product_metrics: Value,
}

impl IntelligenceRegistry {
    pub(crate) fn load() -> Result<Self, serde_json::Error> {
        Ok(Self {
            schema: serde_json::from_str(SCHEMA)?,
            relationships: serde_json::from_str(RELATIONSHIPS)?,
            terminology: serde_json::from_str(TERMINOLOGY)?,
            pii_policy: serde_json::from_str(PII_POLICY)?,
            operations: serde_json::from_str(OPERATIONS)?,
            customers: serde_json::from_str(CUSTOMERS)?,
            products: serde_json::from_str(PRODUCTS)?,
            order_metrics: serde_json::from_str(ORDER_METRICS)?,
            customer_metrics: serde_json::from_str(CUSTOMER_METRICS)?,
            product_metrics: serde_json::from_str(PRODUCT_METRICS)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_is_valid_json() {
        let registry = IntelligenceRegistry::load().expect("registry should load");
        assert!(registry
            .schema
            .tables
            .iter()
            .any(|table| table.name == "order"));
        assert!(registry
            .schema
            .tables
            .iter()
            .any(|table| table.name == "product_variant"));
    }
}
