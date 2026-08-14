use crate::database::QueryResult;
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentResponse {
    pub(crate) answer: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tables: Vec<TableBlock>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) charts: Vec<ChartBlock>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<WarningBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data_as_of: Option<String>,
}

impl AgentResponse {
    pub(crate) fn text(answer: impl Into<String>) -> Self {
        Self {
            answer: answer.into(),
            tables: Vec::new(),
            charts: Vec::new(),
            warnings: Vec::new(),
            data_as_of: None,
        }
    }

    pub(crate) fn from_query(
        answer: String,
        question: &str,
        intent: &str,
        expected_shape: &str,
        result: &QueryResult,
        chart_suggestion: Option<ChartType>,
        chart_title: Option<String>,
    ) -> Self {
        let table = build_table(question, expected_shape, result);
        let charts = table
            .as_ref()
            .and_then(|table| {
                build_chart(
                    question,
                    intent,
                    expected_shape,
                    table,
                    chart_suggestion,
                    chart_title,
                )
            })
            .into_iter()
            .collect();
        let warnings = result
            .truncated
            .then(|| WarningBlock {
                message: format!(
                    "Showing the first {} rows allowed by the analytics safety limit.",
                    result.row_count
                ),
            })
            .into_iter()
            .collect();

        Self {
            answer,
            tables: table.into_iter().collect(),
            charts,
            warnings,
            data_as_of: result.data_as_of.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TableBlock {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) columns: Vec<TableColumn>,
    pub(crate) rows: Vec<Map<String, Value>>,
    pub(crate) row_count: usize,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TableColumn {
    pub(crate) key: String,
    pub(crate) label: String,
    #[serde(rename = "type")]
    pub(crate) value_type: ColumnType,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ColumnType {
    String,
    Number,
    Currency,
    Date,
    Datetime,
    Percentage,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChartBlock {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) chart_type: ChartType,
    pub(crate) title: String,
    pub(crate) dataset_id: String,
    pub(crate) x_key: String,
    pub(crate) y_key: String,
    pub(crate) x_label: String,
    pub(crate) y_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChartType {
    Bar,
    HorizontalBar,
    Line,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WarningBlock {
    pub(crate) message: String,
}

fn build_table(question: &str, expected_shape: &str, result: &QueryResult) -> Option<TableBlock> {
    if expected_shape == "scalar" && result.row_count <= 1 && result.columns.len() <= 1 {
        return None;
    }

    let comparison = expected_shape == "comparison"
        && result.rows.len() == 1
        && result.rows[0]
            .values()
            .filter(|value| value.is_number())
            .count()
            >= 2;
    let (rows, ordered_keys) = if comparison {
        pivot_comparison(&result.rows[0])
    } else {
        present_rows(question, expected_shape, result)
    };
    let columns = ordered_keys
        .iter()
        .map(|key| TableColumn {
            key: key.clone(),
            label: humanize_key(key),
            value_type: infer_column_type(key, &rows),
        })
        .collect::<Vec<_>>();

    Some(TableBlock {
        id: "query-result".to_owned(),
        title: table_title(question, expected_shape),
        columns,
        row_count: rows.len(),
        rows,
        truncated: result.truncated,
    })
}

fn pivot_comparison(row: &Map<String, Value>) -> (Vec<Map<String, Value>>, Vec<String>) {
    let rows = row
        .iter()
        .filter(|(_, value)| value.is_number())
        .map(|(key, value)| {
            let mut row = Map::new();
            row.insert("period".to_owned(), Value::String(humanize_key(key)));
            row.insert("value".to_owned(), value.clone());
            row
        })
        .collect();
    (rows, vec!["period".to_owned(), "value".to_owned()])
}

fn present_rows(
    question: &str,
    expected_shape: &str,
    result: &QueryResult,
) -> (Vec<Map<String, Value>>, Vec<String>) {
    let ordinary_sku_request = is_sku_sales_question(question) && !is_raw_sku_question(question);
    let mut rows = result.rows.clone();
    if ordinary_sku_request {
        for row in &mut rows {
            if let Some(display_name) = product_display_name(row) {
                row.insert("variant".to_owned(), Value::String(display_name));
                for key in [
                    "product_name",
                    "product_title",
                    "variant_name",
                    "variant_title",
                    "variant_id",
                    "sku",
                ] {
                    row.remove(key);
                }
            }
        }
    }

    let mut keys = result.columns.clone();
    if let Some(first) = rows.first() {
        keys.extend(first.keys().cloned());
    }
    let customer_records =
        expected_shape == "record_list" && question.to_lowercase().contains("customer");
    let individual_orders = (expected_shape == "record_list" && !customer_records)
        || question.to_lowercase().contains("individual order")
        || question.to_lowercase().contains("order record");
    keys.sort_by_key(|key| {
        if customer_records {
            customer_record_priority(key)
        } else if individual_orders {
            individual_order_priority(key)
        } else {
            column_priority(key)
        }
    });
    keys.dedup();
    keys.retain(|key| rows.is_empty() || rows.iter().any(|row| row.contains_key(key)));
    (rows, keys)
}

fn product_display_name(row: &Map<String, Value>) -> Option<String> {
    let product = string_value(row, &["product_name", "product_title"])?;
    let variant = string_value(row, &["variant_name", "variant_title"]);
    let Some(variant) = variant else {
        return Some(product.to_owned());
    };
    let normalized_product = product.trim().to_lowercase();
    let normalized_variant = variant.trim().to_lowercase();
    if normalized_variant.starts_with(&normalized_product) {
        return Some(variant.to_owned());
    }
    let generic = normalized_variant.is_empty()
        || normalized_variant == "default"
        || normalized_variant == "default variant"
        || normalized_variant == normalized_product
        || normalized_product.ends_with(&normalized_variant);
    if generic {
        Some(product.to_owned())
    } else {
        Some(format!("{product} — {variant}"))
    }
}

fn string_value<'a>(row: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| row.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn build_chart(
    question: &str,
    intent: &str,
    expected_shape: &str,
    table: &TableBlock,
    chart_suggestion: Option<ChartType>,
    chart_title: Option<String>,
) -> Option<ChartBlock> {
    if table.rows.is_empty() {
        return None;
    }
    let normalized = format!("{question} {intent}").to_lowercase();
    if expected_shape == "record_list"
        || normalized.contains("individual order")
        || normalized.contains("order record")
        || normalized.contains("order id")
    {
        return None;
    }
    let x_column = table.columns.iter().find(|column| {
        !is_numeric_type(column.value_type)
            && !column.key.contains("classification")
            && !column.key.contains("confidence")
    })?;
    let preferred_y_keys: &[&str] = if expected_shape == "time_series" {
        &["projected_orders", "forecast_orders", "order_count"]
    } else if expected_shape == "ranking" {
        &[
            "due_customers",
            "change_percentage",
            "quantity_sold",
            "estimated_expected_units",
        ]
    } else {
        &["order_count"]
    };
    let y_column = preferred_y_keys
        .iter()
        .find_map(|key| table.columns.iter().find(|column| column.key == *key))
        .or_else(|| {
            table
                .columns
                .iter()
                .rev()
                .find(|column| is_numeric_type(column.value_type))
        })?;

    let inferred = if expected_shape == "time_series"
        || normalized.contains("by day")
        || normalized.contains("by hour")
        || normalized.contains("over time")
    {
        Some(ChartType::Line)
    } else if expected_shape == "ranking"
        || normalized.contains("status")
        || is_sku_sales_question(&normalized)
        || normalized.contains("top ")
        || normalized.contains("most selling")
        || normalized.contains("underperform")
        || normalized.contains("performing better")
        || normalized.contains("reorder")
    {
        Some(ChartType::HorizontalBar)
    } else if expected_shape == "comparison"
        || normalized.contains("compare")
        || normalized.contains("preceding")
        || normalized.contains(" versus ")
    {
        Some(ChartType::Bar)
    } else {
        None
    };
    let chart_type = inferred.or(chart_suggestion)?;

    Some(ChartBlock {
        id: "query-chart".to_owned(),
        chart_type,
        title: chart_title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| table.title.clone()),
        dataset_id: table.id.clone(),
        x_key: x_column.key.clone(),
        y_key: y_column.key.clone(),
        x_label: x_column.label.clone(),
        y_label: y_column.label.clone(),
    })
}

fn infer_column_type(key: &str, rows: &[Map<String, Value>]) -> ColumnType {
    let normalized = key.to_lowercase();
    if normalized.contains("percentage")
        || normalized.ends_with("_pct")
        || normalized.ends_with("_rate")
    {
        return ColumnType::Percentage;
    }
    if normalized.contains("_inr")
        || normalized.starts_with("inr_")
        || normalized == "order_value"
        || normalized == "average_order_value"
    {
        return ColumnType::Currency;
    }
    if normalized == "change_percentage" {
        return ColumnType::Percentage;
    }
    if normalized.ends_with("_at") || normalized.contains("timestamp") {
        return ColumnType::Datetime;
    }
    if normalized == "date"
        || normalized.ends_with("_date")
        || normalized == "day"
        || normalized == "period"
    {
        return ColumnType::Date;
    }
    if rows
        .iter()
        .filter_map(|row| row.get(key))
        .any(Value::is_number)
    {
        ColumnType::Number
    } else {
        ColumnType::String
    }
}

const fn is_numeric_type(value_type: ColumnType) -> bool {
    matches!(
        value_type,
        ColumnType::Number | ColumnType::Currency | ColumnType::Percentage
    )
}

fn column_priority(key: &str) -> (u8, String) {
    let normalized = key.to_lowercase();

    let priority = if matches!(
        normalized.as_str(),
        "name"
            | "product_name"
            | "product_title"
            | "variant"
            | "variant_name"
            | "variant_title"
            | "customer_name"
            | "full_name"
            | "first_name"
            | "store_name"
    ) {
        // Human-readable identity always wins.
        0
    } else if normalized.ends_with("_id") || normalized == "id" {
        // Technical identity directly follows the name.
        1
    } else if normalized.contains("status") || normalized == "period" {
        2
    } else if normalized == "day"
        || normalized == "week"
        || normalized == "month"
        || normalized.contains("date")
    {
        3
    } else if normalized.contains("count")
        || normalized.contains("quantity")
        || normalized.contains("total")
        || normalized.contains("value")
        || normalized.contains("percentage")
        || normalized.ends_with("_pct")
        || normalized.ends_with("_rate")
    {
        5
    } else {
        4
    };

    (priority, normalized)
}

fn individual_order_priority(key: &str) -> (u8, String) {
    let normalized = key.to_lowercase();
    let priority = match normalized.as_str() {
        "order_id" => 0,
        "display_id" => 1,
        "store_invoice_number" => 2,
        "zoho_order_status" | "operational_status" => 3,
        "created_at" => 4,
        "fulfillment_status" => 5,
        "payment_status" => 6,
        _ => 5,
    };
    (priority, normalized)
}

fn customer_record_priority(key: &str) -> (u8, String) {
    let normalized = key.to_lowercase();

    let priority = match normalized.as_str() {
        "customer_name" | "full_name" | "first_name" => 0,

        "customer_id" => 1,

        "phone" => 2,

        _ => 3,
    };

    (priority, normalized)
}

fn table_title(question: &str, expected_shape: &str) -> String {
    let normalized = question.to_lowercase();
    if is_sku_sales_question(question) && !is_raw_sku_question(question) {
        "Top-selling variants".to_owned()
    } else if is_raw_sku_question(question) {
        "Raw SKU values and sales".to_owned()
    } else if expected_shape == "record_list" && normalized.contains("customer") {
        if normalized.contains("haven't ordered")
            || normalized.contains("have not ordered")
            || normalized.contains("inactive")
        {
            "Lapsed customers".to_owned()
        } else {
            "Customer records".to_owned()
        }
    } else if expected_shape == "record_list" || normalized.contains("individual order") {
        "Orders and statuses".to_owned()
    } else if normalized.contains("status") {
        "Orders by operational status".to_owned()
    } else if expected_shape == "time_series" {
        "Order volume over time".to_owned()
    } else if expected_shape == "comparison" || normalized.contains("compare") {
        "Period comparison".to_owned()
    } else {
        "Query results".to_owned()
    }
}

fn humanize_key(key: &str) -> String {
    if key.eq_ignore_ascii_case("sku") {
        return "SKU".to_owned();
    }
    key.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                let word = format!("{}{}", first.to_uppercase(), characters.as_str());
                if word == "Id" {
                    "ID".to_owned()
                } else {
                    word
                }
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_sku_sales_question(question: &str) -> bool {
    let normalized = question.to_lowercase();
    ["sku", "variant", "item"]
        .iter()
        .any(|term| normalized.contains(term))
        && ["sell", "sold", "sales", "quantity", "top", "most"]
            .iter()
            .any(|term| normalized.contains(term))
}

fn is_raw_sku_question(question: &str) -> bool {
    let normalized = question.to_lowercase();
    normalized.contains("raw sku")
        || normalized.contains("raw product_variant.sku")
        || normalized.contains("product_variant.sku")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_deterministic_variant_display_name() {
        let mut row = Map::new();
        row.insert(
            "product_name".to_owned(),
            Value::String("MP Sharbati Atta".to_owned()),
        );
        row.insert("variant_name".to_owned(), Value::String("5 kg".to_owned()));
        assert_eq!(
            product_display_name(&row).as_deref(),
            Some("MP Sharbati Atta — 5 kg")
        );
    }

    #[test]
    fn converts_a_comparison_row_into_a_shared_chart_dataset() {
        let mut row = Map::new();
        row.insert(
            "last_7_days".to_owned(),
            Value::Number(serde_json::Number::from(23)),
        );
        row.insert(
            "previous_7_days".to_owned(),
            Value::Number(serde_json::Number::from(31)),
        );
        let result = QueryResult {
            columns: vec!["last_7_days".to_owned(), "previous_7_days".to_owned()],
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        let response = AgentResponse::from_query(
            "Comparison".to_owned(),
            "Compare periods",
            "compare",
            "comparison",
            &result,
            None,
            None,
        );
        assert_eq!(response.tables[0].rows.len(), 2);
        assert_eq!(response.charts[0].dataset_id, response.tables[0].id);
    }

    #[test]
    fn preserves_default_operational_record_column_order() {
        let mut row = Map::new();
        row.insert("created_at".to_owned(), Value::from("2026-06-10T00:00:00Z"));
        row.insert("zoho_order_status".to_owned(), Value::from("FULFILLED"));
        row.insert("store_invoice_number".to_owned(), Value::from(12));
        row.insert("display_id".to_owned(), Value::from(34));
        row.insert("order_id".to_owned(), Value::from("order_1"));
        let result = QueryResult {
            columns: row.keys().cloned().collect(),
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        let response = AgentResponse::from_query(
            "Found one order.".to_owned(),
            "List orders placed on June 10, 2026.",
            "list orders",
            "record_list",
            &result,
            None,
            None,
        );
        assert_eq!(
            response.tables[0]
                .columns
                .iter()
                .map(|column| column.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "order_id",
                "display_id",
                "store_invoice_number",
                "zoho_order_status",
                "created_at"
            ]
        );
    }
    #[test]
    fn variant_title_precedes_technical_variant_id() {
        let row = Map::from_iter([
            ("variant_id".to_owned(), Value::from("variant_123")),
            ("variant_title".to_owned(), Value::from("Regular - 5 kg")),
            ("quantity_sold".to_owned(), Value::from(120)),
            ("product_sales_value".to_owned(), Value::from(14_000.0)),
        ]);

        let result = QueryResult {
            columns: vec![
                "variant_id".to_owned(),
                "variant_title".to_owned(),
                "quantity_sold".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let response = AgentResponse::from_query(
            "Variant performance".to_owned(),
            "Show variant performance",
            "semantic product performance",
            "ranking",
            &result,
            None,
            None,
        );

        let keys = response.tables[0]
            .columns
            .iter()
            .map(|column| column.key.as_str())
            .collect::<Vec<_>>();

        assert_eq!(keys[0], "variant_title");

        assert_eq!(keys[1], "variant_id");
    }
    #[test]
    fn product_name_precedes_product_id() {
        assert!(column_priority("product_name") < column_priority("product_id"));
    }
}
