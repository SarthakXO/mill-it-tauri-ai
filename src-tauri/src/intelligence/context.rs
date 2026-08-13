use super::{registry::IntelligenceRegistry, types::Sensitivity};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SemanticContext {
    pub(crate) schema_index: Vec<SchemaIndexEntry>,
    pub(crate) detailed_schema: Vec<Value>,
    pub(crate) terminology: Value,
    pub(crate) relationships: Value,
    pub(crate) pii_policy: Value,
    pub(crate) domains: Vec<Value>,
    pub(crate) metrics: Vec<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SchemaIndexEntry {
    pub(crate) table: String,
    pub(crate) meaning: String,
}

pub(crate) fn context_for_question(question: &str) -> Result<SemanticContext, serde_json::Error> {
    let registry = IntelligenceRegistry::load()?;
    let normalized = question.to_lowercase();
    let product = contains_any(&normalized, &["product", "variant", "sku", "item", "sold"]);
    let customer = contains_any(
        &normalized,
        &[
            "customer", "buyer", "reorder", "repeat", "lapsed", "inactive",
        ],
    );
    let operations = contains_any(
        &normalized,
        &["order", "status", "fulfill", "delivery", "store", "cancel"],
    ) || (!product && !customer);

    let relevant_tables = selected_tables(&normalized, operations, customer, product);
    let detailed_schema = registry
        .schema
        .tables
        .iter()
        .filter(|table| relevant_tables.contains(&table.name.as_str()))
        .map(|table| {
            let columns = table
                .columns
                .iter()
                .filter(|column| {
                    matches!(
                        column.sensitivity,
                        Sensitivity::Safe | Sensitivity::Internal
                    )
                })
                .collect::<Vec<_>>();
            json!({
                "table": table.name,
                "description": table.description,
                "columns": columns,
            })
        })
        .collect();
    let relationships = registry
        .relationships
        .get("relationships")
        .and_then(Value::as_array)
        .map(|relationships| {
            relationships
                .iter()
                .filter(|relationship| {
                    ["from", "to"].iter().all(|key| {
                        relationship
                            .get(*key)
                            .and_then(Value::as_str)
                            .and_then(|endpoint| endpoint.split('.').next())
                            .is_some_and(|table| relevant_tables.contains(&table))
                    })
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut domains = Vec::new();
    let mut metrics = Vec::new();
    if operations {
        domains.push(registry.operations);
        metrics.push(registry.order_metrics);
    }
    if customer {
        domains.push(registry.customers);
        metrics.push(registry.customer_metrics);
    }
    if product {
        domains.push(registry.products);
        metrics.push(registry.product_metrics);
    }
    Ok(SemanticContext {
        schema_index: registry
            .schema
            .tables
            .iter()
            .filter(|table| relevant_tables.contains(&table.name.as_str()))
            .map(|table| SchemaIndexEntry {
                table: table.name.clone(),
                meaning: table.description.clone(),
            })
            .collect(),
        detailed_schema,
        terminology: registry.terminology,
        relationships: json!({"relationships": relationships}),
        pii_policy: json!({
            "model_context_rules": [
                "PII fields and values are excluded from model context.",
                "Internal analytical identifiers may be used for joins, grouping, and deterministic calculations.",
                "Secret and denied fields are never projected or sent to the model."
            ]
        }),
        domains,
        metrics,
    })
}

fn selected_tables(
    question: &str,
    operations: bool,
    customer: bool,
    product: bool,
) -> Vec<&'static str> {
    let mut tables = Vec::new();
    if operations || customer || product {
        tables.push("order");
    }
    if customer {
        tables.push("customer");
    }
    if product {
        tables.extend([
            "line_item",
            "product_variant",
            "product",
            "partial_order_cancellation_item",
        ]);
    }
    if question.contains("fulfill") || question.contains("delivery") {
        tables.push("fulfillment");
    }
    if question.contains("refund") {
        tables.push("refund_request");
    }
    if question.contains("store") {
        tables.push("store");
    }
    if question.contains("cancel") {
        tables.extend([
            "partial_order_cancellation",
            "partial_order_cancellation_item",
        ]);
    }
    tables.sort_unstable();
    tables.dedup();
    tables
}

fn contains_any(value: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| value.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_context_is_small_and_excludes_pii_columns() {
        let context = context_for_question("Compare weekly orders in May and June")
            .expect("context should load");
        let serialized = serde_json::to_string(&context).unwrap();
        assert!(serialized.contains("created_at"));
        assert!(!serialized.contains("first_name"));
        assert!(!serialized.contains("phone"));
        assert!(!serialized.contains("refund workflow"));
    }

    #[test]
    fn reorder_context_selects_customer_and_product_domains_without_pii() {
        let context = context_for_question("Which customers reorder Khapli variants?")
            .expect("context should load");
        let serialized = serde_json::to_string(&context).unwrap();

        assert!(serialized.contains("customer-product reorder history"));
        assert!(serialized.contains("line_item.variant_id"));
        assert!(serialized.contains("customer"));
        assert!(serialized.contains("product_variant"));

        assert!(!serialized.contains("first_name"));
        assert!(!serialized.contains("phone"));
    }

    #[test]
    fn product_sales_context_includes_net_quantity_dependencies() {
        let context = context_for_question("Tell me the best-selling products in June")
            .expect("context should load");

        let serialized = serde_json::to_string(&context).unwrap();

        assert!(serialized.contains("line_item"));
        assert!(serialized.contains("product_variant"));
        assert!(serialized.contains("product_id"));
        assert!(serialized.contains("partial_order_cancellation_item"));
        assert!(serialized.contains("cancelled_quantity"));

        assert!(!serialized.contains("first_name"));
        assert!(!serialized.contains("phone"));
    }
}
