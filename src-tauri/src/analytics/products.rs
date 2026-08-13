use super::{
    orders::{date_parameters, format_number},
    query::{execute, NET_LINE_QUANTITY, PARTIAL_CANCEL_QUANTITY_JOIN, QUALIFYING_SALE_FILTER},
    types::{DateRange, PerformanceDirection},
};
use crate::{
    agent::{AgentError, AgentResponse},
    database::{Database, QueryParameter, QueryResult},
};
use serde_json::{Map, Number, Value};

const MINIMUM_BASELINE_QUANTITY: f64 = 10.0;
const UNDERPERFORMANCE_THRESHOLD: f64 = -20.0;
const OVERPERFORMANCE_THRESHOLD: f64 = 20.0;

pub(crate) async fn top_variants(
    database: &Database,
    question: &str,
    range: &DateRange,
    limit: usize,
) -> Result<AgentResponse, AgentError> {
    let sql = format!(
        "SELECT li.variant_id, li.title AS historical_product_label, SUM({NET_LINE_QUANTITY}) AS quantity_sold FROM line_item AS li JOIN \"order\" AS o ON o.id = li.order_id{PARTIAL_CANCEL_QUANTITY_JOIN} WHERE {QUALIFYING_SALE_FILTER} AND li.variant_id IS NOT NULL AND li.is_return = false AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY li.variant_id, li.title ORDER BY quantity_sold DESC LIMIT $3"
    );
    let mut parameters = date_parameters(range);
    parameters.push(QueryParameter::Integer(limit as i64));
    let result = execute(database, &sql, parameters).await?;
    let answer = result.rows.first().map_or_else(
        || "No qualifying variant sales were found for that period.".to_owned(),
        |row| {
            let variant = display_variant(row);
            let units = row
                .get("quantity_sold")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let top_total = result
                .rows
                .iter()
                .take(5)
                .filter_map(|row| row.get("quantity_sold").and_then(Value::as_f64))
                .sum::<f64>();
            format!(
                "{} was the best-selling variant with {} units.\n\nThe top {} variants accounted for {} units combined.",
                variant,
                format_number(units),
                result.rows.len().min(5),
                format_number(top_total)
            )
        },
    );
    let presented = variant_display_result(result);
    Ok(AgentResponse::from_query(
        answer,
        question,
        "deterministic top variants",
        "ranking",
        &presented,
        None,
        Some("Best-selling variants".to_owned()),
    ))
}

fn variant_display_result(result: QueryResult) -> QueryResult {
    let rows = result
        .rows
        .iter()
        .map(|row| {
            let mut output = Map::new();
            output.insert("variant".to_owned(), Value::String(display_variant(row)));
            output.insert(
                "quantity_sold".to_owned(),
                row.get("quantity_sold").cloned().unwrap_or(Value::Null),
            );
            output
        })
        .collect::<Vec<_>>();
    QueryResult {
        columns: vec!["variant".to_owned(), "quantity_sold".to_owned()],
        row_count: rows.len(),
        rows,
        truncated: result.truncated,
        duration_ms: result.duration_ms,
        data_as_of: result.data_as_of,
    }
}

pub(crate) async fn product_performance(
    database: &Database,
    question: &str,
    current: &DateRange,
    baseline: &DateRange,
    direction: PerformanceDirection,
    limit: usize,
) -> Result<AgentResponse, AgentError> {
    let sql = format!(
        "SELECT li.variant_id, li.title AS historical_product_label, SUM(CASE WHEN timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) THEN {NET_LINE_QUANTITY} ELSE 0 END) AS current_quantity, SUM(CASE WHEN timezone('Asia/Kolkata', o.created_at) >= (($3::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($4::text)::date) THEN {NET_LINE_QUANTITY} ELSE 0 END) AS previous_quantity FROM line_item AS li JOIN \"order\" AS o ON o.id = li.order_id{PARTIAL_CANCEL_QUANTITY_JOIN} WHERE {QUALIFYING_SALE_FILTER} AND li.variant_id IS NOT NULL AND li.is_return = false AND ((timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)) OR (timezone('Asia/Kolkata', o.created_at) >= (($3::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($4::text)::date))) GROUP BY li.variant_id, li.title"
    );
    let result = execute(
        database,
        &sql,
        vec![
            QueryParameter::String(current.start.to_string()),
            QueryParameter::String(current.end_exclusive.to_string()),
            QueryParameter::String(baseline.start.to_string()),
            QueryParameter::String(baseline.end_exclusive.to_string()),
        ],
    )
    .await?;
    let mut rows = result
        .rows
        .iter()
        .filter_map(|row| performance_row(row, current.days(), baseline.days(), direction))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let left_change = left
            .get("change_percentage")
            .and_then(Value::as_f64)
            .unwrap_or(match direction {
                PerformanceDirection::Underperforming => f64::INFINITY,
                PerformanceDirection::Overperforming => f64::NEG_INFINITY,
            });
        let right_change = right
            .get("change_percentage")
            .and_then(Value::as_f64)
            .unwrap_or(match direction {
                PerformanceDirection::Underperforming => f64::INFINITY,
                PerformanceDirection::Overperforming => f64::NEG_INFINITY,
            });
        match direction {
            PerformanceDirection::Underperforming => left_change.total_cmp(&right_change),
            PerformanceDirection::Overperforming => right_change.total_cmp(&left_change),
        }
    });
    rows.truncate(limit);
    let output = QueryResult {
        columns: vec![
            "variant".to_owned(),
            "previous_quantity".to_owned(),
            "current_quantity".to_owned(),
            "previous_daily_rate".to_owned(),
            "current_daily_rate".to_owned(),
            "change_percentage".to_owned(),
            "comparison_status".to_owned(),
        ],
        row_count: rows.len(),
        rows,
        truncated: false,
        duration_ms: result.duration_ms,
        data_as_of: result.data_as_of,
    };
    let answer = performance_summary(&output, direction, current, baseline);
    let title = match direction {
        PerformanceDirection::Underperforming => "Underperforming variants",
        PerformanceDirection::Overperforming => "Overperforming variants",
    };
    Ok(AgentResponse::from_query(
        answer,
        question,
        "deterministic variant daily-rate comparison",
        "ranking",
        &output,
        None,
        Some(title.to_owned()),
    ))
}

fn performance_row(
    row: &Map<String, Value>,
    current_days: i64,
    baseline_days: i64,
    direction: PerformanceDirection,
) -> Option<Map<String, Value>> {
    let current = row.get("current_quantity")?.as_f64()?;
    let baseline = row.get("previous_quantity")?.as_f64()?;
    let calculation = calculate_performance(current, baseline, current_days, baseline_days);
    let include = match calculation.status {
        PerformanceStatus::Comparable => match direction {
            PerformanceDirection::Underperforming => {
                calculation.change_percentage? <= UNDERPERFORMANCE_THRESHOLD
            }
            PerformanceDirection::Overperforming => {
                calculation.change_percentage? >= OVERPERFORMANCE_THRESHOLD
            }
        },
        PerformanceStatus::NoBaseline => {
            direction == PerformanceDirection::Overperforming && current > 0.0
        }
        PerformanceStatus::TinyBaseline => false,
    };
    if !include {
        return None;
    }
    let mut output = Map::new();
    output.insert("variant".to_owned(), Value::String(display_variant(row)));
    output.insert("previous_quantity".to_owned(), number(baseline));
    output.insert("current_quantity".to_owned(), number(current));
    output.insert(
        "previous_daily_rate".to_owned(),
        number(calculation.baseline_daily_rate),
    );
    output.insert(
        "current_daily_rate".to_owned(),
        number(calculation.current_daily_rate),
    );
    output.insert(
        "change_percentage".to_owned(),
        calculation.change_percentage.map_or(Value::Null, number),
    );
    output.insert(
        "comparison_status".to_owned(),
        Value::String(match calculation.status {
            PerformanceStatus::Comparable => "Comparable".to_owned(),
            PerformanceStatus::NoBaseline => "New/no comparable baseline".to_owned(),
            PerformanceStatus::TinyBaseline => "Insufficient baseline volume".to_owned(),
        }),
    );
    Some(output)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PerformanceStatus {
    Comparable,
    NoBaseline,
    TinyBaseline,
}

#[derive(Clone, Copy, Debug)]
struct PerformanceCalculation {
    current_daily_rate: f64,
    baseline_daily_rate: f64,
    change_percentage: Option<f64>,
    status: PerformanceStatus,
}

fn calculate_performance(
    current: f64,
    baseline: f64,
    current_days: i64,
    baseline_days: i64,
) -> PerformanceCalculation {
    let current_daily_rate = current / current_days.max(1) as f64;
    let baseline_daily_rate = baseline / baseline_days.max(1) as f64;
    if baseline <= 0.0 {
        return PerformanceCalculation {
            current_daily_rate,
            baseline_daily_rate,
            change_percentage: None,
            status: PerformanceStatus::NoBaseline,
        };
    }
    if baseline < MINIMUM_BASELINE_QUANTITY {
        return PerformanceCalculation {
            current_daily_rate,
            baseline_daily_rate,
            change_percentage: Some((current_daily_rate / baseline_daily_rate - 1.0) * 100.0),
            status: PerformanceStatus::TinyBaseline,
        };
    }
    PerformanceCalculation {
        current_daily_rate,
        baseline_daily_rate,
        change_percentage: Some((current_daily_rate / baseline_daily_rate - 1.0) * 100.0),
        status: PerformanceStatus::Comparable,
    }
}

fn performance_summary(
    result: &QueryResult,
    direction: PerformanceDirection,
    current: &DateRange,
    baseline: &DateRange,
) -> String {
    let threshold = match direction {
        PerformanceDirection::Underperforming => "at least 20% below",
        PerformanceDirection::Overperforming => "at least 20% above",
    };
    let period_note = format!(
        "Current period {} to {}; baseline {} to {}.",
        current.start,
        current.end_exclusive - chrono::Duration::days(1),
        baseline.start,
        baseline.end_exclusive - chrono::Duration::days(1)
    );
    result.rows.first().map_or_else(
        || format!("No variants with a meaningful baseline were selling {threshold} their baseline daily rate.\n\n{period_note}"),
        |row| {
            let variant = row.get("variant").and_then(Value::as_str).unwrap_or("The leading variant");
            let change = row.get("change_percentage").and_then(Value::as_f64);
            let lead = change.map_or_else(
                || format!("{variant} is new in the current period and has no comparable baseline."),
                |change| format!("{} variants were selling {threshold} their baseline daily rate. {variant} had the largest movement at {change:.1}%.", result.row_count),
            );
            format!("{lead}\n\n{period_note}")
        },
    )
}

fn display_variant(row: &Map<String, Value>) -> String {
    if let Some(label) = row
        .get("historical_product_label")
        .and_then(Value::as_str)
        .filter(|label| !label.trim().is_empty())
    {
        return label.to_owned();
    }
    let product = row
        .get("product_name")
        .and_then(Value::as_str)
        .unwrap_or("Unknown product");
    let variant = row
        .get("variant_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if variant.is_empty() || variant.eq_ignore_ascii_case(product) {
        product.to_owned()
    } else {
        format!("{product} — {variant}")
    }
}

fn number(value: f64) -> Value {
    Number::from_f64((value * 100.0).round() / 100.0)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_handles_zero_tiny_and_zero_current_baselines() {
        let no_baseline = calculate_performance(20.0, 0.0, 30, 30);
        assert_eq!(no_baseline.status, PerformanceStatus::NoBaseline);
        assert!(no_baseline.change_percentage.is_none());

        let tiny = calculate_performance(8.0, 5.0, 30, 30);
        assert_eq!(tiny.status, PerformanceStatus::TinyBaseline);

        let stopped = calculate_performance(0.0, 50.0, 30, 30);
        assert_eq!(stopped.change_percentage, Some(-100.0));
    }

    #[test]
    fn historical_line_title_wins_over_current_catalog_names() {
        let row = Map::from_iter([
            (
                "historical_product_label".to_owned(),
                Value::from("Historical atta 5kg"),
            ),
            ("product_name".to_owned(), Value::from("Renamed atta")),
            ("variant_name".to_owned(), Value::from("Current 5kg")),
        ]);
        assert_eq!(display_variant(&row), "Historical atta 5kg");
    }
}
