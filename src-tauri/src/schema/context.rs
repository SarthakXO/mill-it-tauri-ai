use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SchemaContext {
    pub(crate) tables: Vec<SchemaTable>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SchemaTable {
    pub(crate) name: String,
    pub(crate) columns: Vec<SchemaColumn>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SchemaColumn {
    pub(crate) name: String,
    pub(crate) postgres_type: String,
    pub(crate) nullable: bool,
    pub(crate) primary_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) references: Option<ForeignKey>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ForeignKey {
    pub(crate) table: String,
    pub(crate) column: String,
}

pub(crate) fn relevant_tables(question: &str, intent: &str) -> Vec<String> {
    let normalized = format!("{question} {intent}").to_lowercase();
    let question_normalized = question.to_lowercase();
    let mut tables = vec!["order"];

    if contains_any(
        &normalized,
        &["product", "variant", "item", "sold", "selling", "sales"],
    ) {
        tables.push("line_item");
        if !((question_normalized.contains("sku") || question_normalized.contains("variant"))
            && contains_any(&question_normalized, &["how many", "count"]))
        {
            tables.extend(["product_variant", "product"]);
        }
    }
    if contains_any(&normalized, &["fulfill", "delivery", "delivered", "rider"]) {
        tables.push("fulfillment");
    }
    if contains_any(&normalized, &["payment", "paid", "revenue", "refund"]) {
        tables.push("payment");
    }
    if contains_any(&normalized, &["customer", "buyer"])
        && !contains_any(&question_normalized, &["how many", "count"])
    {
        tables.push("customer");
    }
    if contains_any(&normalized, &["refund", "refunded"]) {
        tables.push("refund_request");
    }
    if contains_any(&normalized, &["cancel", "cancellation"]) {
        tables.extend([
            "partial_order_cancellation",
            "partial_order_cancellation_item",
        ]);
    }
    if contains_any(&normalized, &["lifecycle", "pending"]) {
        tables.extend(["order_lifecycle_log", "fulfillment", "payment"]);
    }
    if contains_any(&normalized, &["store", "location"]) {
        tables.push("store");
    }

    tables.sort_unstable();
    tables.dedup();
    tables.into_iter().map(str::to_owned).collect()
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_product_sales_schema_without_every_domain() {
        let tables = relevant_tables("Which products sold most last week?", "rank products");
        assert!(tables.contains(&"line_item".to_owned()));
        assert!(tables.contains(&"product_variant".to_owned()));
        assert!(!tables.contains(&"customer".to_owned()));
    }

    #[test]
    fn scalar_counts_load_only_tables_needed_for_the_aggregate() {
        assert_eq!(
            relevant_tables("How many different SKUs sold in June?", "count variants"),
            vec!["line_item".to_owned(), "order".to_owned()]
        );
        assert_eq!(
            relevant_tables("How many customers ordered in June?", "count customers"),
            vec!["order".to_owned()]
        );
    }
}
