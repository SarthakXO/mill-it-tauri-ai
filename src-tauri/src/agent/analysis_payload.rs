use super::{sql_plan::SqlQueryPlan, summary::SummaryAnalysis};
use crate::{database::validator::is_sensitive_column, database::QueryResult};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const DEFAULT_MAX_ANALYSIS_ROWS: usize = 100;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AnalysisPayload {
    pub(crate) question: String,
    pub(crate) analysis_type: String,
    pub(crate) metrics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Vec<Map<String, Value>>>,
    pub(crate) derived_facts: Value,
    pub(crate) approved_facts: Vec<ApprovedFact>,
    pub(crate) guaranteed_summary: String,
    pub(crate) data_as_of: Option<String>,
    pub(crate) dataset_policy: DatasetPolicy,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ApprovedFact {
    pub(crate) id: String,
    pub(crate) text: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DatasetPolicy {
    pub(crate) source_row_count: usize,
    pub(crate) rows_sent_to_model: usize,
    pub(crate) pii_rows_omitted: bool,
    pub(crate) reduced_for_size: bool,
}

impl AnalysisPayload {
    pub(crate) fn from_query(
        question: &str,
        plan: &SqlQueryPlan,
        result: &QueryResult,
        summary: &SummaryAnalysis,
        customer_record_list: bool,
    ) -> Self {
        let threshold = analysis_row_threshold();
        let reduced_for_size = result.rows.len() > threshold;
        let omit_rows = customer_record_list || reduced_for_size;
        let data = (!omit_rows).then(|| result.rows.iter().map(sanitize_row).collect::<Vec<_>>());
        let rows_sent_to_model = data.as_ref().map_or(0, Vec::len);
        let derived_facts = json!({
            "deterministic": summary.facts,
            "column_statistics": numeric_column_statistics(result),
            "series_statistics": series_statistics(plan, result),
            "category_statistics": (!customer_record_list).then(|| category_statistics(plan, result)),
            "customer_cohort_statistics": customer_record_list.then(|| customer_cohort_statistics(result)),
            "result": {
                "row_count": result.row_count,
                "truncated": result.truncated
            }
        });
        Self {
            question: question.to_owned(),
            analysis_type: plan.result_shape.as_str().to_owned(),
            metrics: plan.metrics.clone(),
            data,
            derived_facts,
            approved_facts: vec![ApprovedFact {
                id: "F1".to_owned(),
                text: summary.answer.clone(),
            }],
            guaranteed_summary: summary.answer.clone(),
            data_as_of: result.data_as_of.clone(),
            dataset_policy: DatasetPolicy {
                source_row_count: result.rows.len(),
                rows_sent_to_model,
                pii_rows_omitted: customer_record_list,
                reduced_for_size,
            },
        }
    }
}

fn analysis_row_threshold() -> usize {
    std::env::var("INSIGHT_MAX_ROWS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=100).contains(value))
        .unwrap_or(DEFAULT_MAX_ANALYSIS_ROWS)
}

fn sanitize_row(row: &Map<String, Value>) -> Map<String, Value> {
    row.iter()
        .filter(|(key, value)| !is_sensitive_column(key) && is_analysis_scalar(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_analysis_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn numeric_column_statistics(result: &QueryResult) -> Value {
    let mut values = BTreeMap::<String, Vec<f64>>::new();
    for row in &result.rows {
        for (key, value) in row {
            if !is_sensitive_column(key) {
                if let Some(number) = value.as_f64() {
                    values.entry(key.clone()).or_default().push(number);
                }
            }
        }
    }
    Value::Object(
        values
            .into_iter()
            .map(|(key, mut values)| {
                values.sort_by(f64::total_cmp);
                let total = values.iter().sum::<f64>();
                let mean = total / values.len() as f64;
                let median = if values.len() % 2 == 0 {
                    let upper = values.len() / 2;
                    (values[upper - 1] + values[upper]) / 2.0
                } else {
                    values[values.len() / 2]
                };
                (
                    key,
                    json!({
                        "count": values.len(),
                        "total": total,
                        "mean": mean,
                        "median": median,
                        "minimum": values.first(),
                        "maximum": values.last()
                    }),
                )
            })
            .collect(),
    )
}

fn category_statistics(plan: &SqlQueryPlan, result: &QueryResult) -> Value {
    let Some(first_row) = result.rows.first() else {
        return Value::Null;
    };
    let dimension = first_row
        .iter()
        .find(|(_, value)| value.is_string() || value.is_boolean())
        .map(|(key, _)| key.clone());
    let metric = first_row
        .iter()
        .find(|(key, value)| {
            let key = key.to_lowercase();
            value.is_number()
                && !key.ends_with("_id")
                && !key.ends_with("_rank")
                && !key.starts_with("rank")
                && !key.contains("display_id")
        })
        .map(|(key, _)| key.clone());
    let (Some(dimension), Some(metric)) = (dimension, metric) else {
        return Value::Null;
    };
    let mut points = result
        .rows
        .iter()
        .filter_map(|row| {
            Some(json!({
                "label": row.get(&dimension)?.as_str()?,
                "value": row.get(&metric)?.as_f64()?
            }))
        })
        .collect::<Vec<_>>();
    points.sort_by(|left, right| {
        right["value"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&left["value"].as_f64().unwrap_or(0.0))
    });
    let total = points
        .iter()
        .filter_map(|point| point["value"].as_f64())
        .sum::<f64>();
    let full_universe =
        matches!(
            plan.result_shape,
            super::sql_plan::SqlResultShape::CategoryBreakdown
        ) || (matches!(plan.result_shape, super::sql_plan::SqlResultShape::Ranking)
            && !plan.sql.to_lowercase().contains("limit"));
    let share = |count: usize| {
        let subtotal = points
            .iter()
            .take(count)
            .filter_map(|point| point["value"].as_f64())
            .sum::<f64>();
        (full_universe && total.abs() > f64::EPSILON).then_some(subtotal / total * 100.0)
    };
    let top_1_share = share(1);
    let top_3_share = share(3);
    let top_5_share = share(5);
    let top_10_share = share(10);
    let top = points
        .into_iter()
        .take(10)
        .map(|mut point| {
            point["share_percent"] = if full_universe && total.abs() > f64::EPSILON {
                Value::from(point["value"].as_f64().unwrap_or(0.0) / total * 100.0)
            } else {
                Value::Null
            };
            point
        })
        .collect::<Vec<_>>();
    json!({
        "dimension": dimension,
        "metric": metric,
        "total": total,
        "concentration_supported": full_universe,
        "top_1_share_percent": top_1_share,
        "top_3_share_percent": top_3_share,
        "top_5_share_percent": top_5_share,
        "top_10_share_percent": top_10_share,
        "top": top
    })
}

fn series_statistics(plan: &SqlQueryPlan, result: &QueryResult) -> Value {
    let Some(dimension) = plan.x_column.as_ref() else {
        return Value::Null;
    };
    let mut metrics = Map::new();
    for metric in &plan.metrics {
        let points = result
            .rows
            .iter()
            .filter_map(|row| {
                Some((
                    scalar_label(row.get(dimension)?)?,
                    row.get(metric)?.as_f64()?,
                ))
            })
            .collect::<Vec<_>>();
        if points.len() < 2 {
            continue;
        }
        let changes = points
            .windows(2)
            .map(|pair| {
                let difference = pair[1].1 - pair[0].1;
                json!({
                    "from": pair[0].0,
                    "to": pair[1].0,
                    "from_value": pair[0].1,
                    "to_value": pair[1].1,
                    "absolute_change": difference,
                    "percentage_change": (pair[0].1.abs() > f64::EPSILON)
                        .then_some(difference / pair[0].1 * 100.0),
                    "direction": if difference > 0.0 { "up" } else if difference < 0.0 { "down" } else { "flat" }
                })
            })
            .collect::<Vec<_>>();
        let largest_rise = changes
            .iter()
            .filter(|change| {
                change["absolute_change"]
                    .as_f64()
                    .is_some_and(|value| value > 0.0)
            })
            .max_by(|left, right| {
                left["absolute_change"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&right["absolute_change"].as_f64().unwrap_or(0.0))
            });
        let largest_decline = changes
            .iter()
            .filter(|change| {
                change["absolute_change"]
                    .as_f64()
                    .is_some_and(|value| value < 0.0)
            })
            .min_by(|left, right| {
                left["absolute_change"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&right["absolute_change"].as_f64().unwrap_or(0.0))
            });
        let directions = changes
            .iter()
            .filter_map(|change| change["direction"].as_str())
            .collect::<Vec<_>>();
        let reversals = directions
            .windows(2)
            .filter(|pair| matches!(pair, ["up", "down"] | ["down", "up"]))
            .count();
        let overall = points.last().expect("two points exist").1 - points[0].1;
        metrics.insert(
            metric.clone(),
            json!({
                "first": {"label": points[0].0, "value": points[0].1},
                "last": {"label": points.last().expect("two points exist").0, "value": points.last().expect("two points exist").1},
                "overall_absolute_change": overall,
                "overall_percentage_change": (points[0].1.abs() > f64::EPSILON)
                    .then_some(overall / points[0].1 * 100.0),
                "sequential_changes": changes,
                "up_intervals": directions.iter().filter(|direction| **direction == "up").count(),
                "down_intervals": directions.iter().filter(|direction| **direction == "down").count(),
                "flat_intervals": directions.iter().filter(|direction| **direction == "flat").count(),
                "reversal_count": reversals,
                "largest_rise": largest_rise,
                "largest_decline": largest_decline
            }),
        );
    }
    Value::Object(metrics)
}

fn scalar_label(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn customer_cohort_statistics(result: &QueryResult) -> Value {
    let mut orders = numeric_values(result, "order_count");
    let mut inactivity = numeric_values(result, "days_since_last_order");
    json!({
        "customer_count": result.row_count,
        "average_order_count": (!orders.is_empty()).then(|| orders.iter().sum::<f64>() / orders.len() as f64),
        "median_order_count": median(&mut orders),
        "median_inactivity_days": median(&mut inactivity),
        "inactive_over_120_days": inactivity.iter().filter(|value| **value > 120.0).count()
    })
}

fn numeric_values(result: &QueryResult, key: &str) -> Vec<f64> {
    result
        .rows
        .iter()
        .filter_map(|row| row.get(key).and_then(Value::as_f64))
        .collect()
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sql_plan::{SqlResultShape, SqlVisualization};

    #[test]
    fn customer_rows_are_omitted_from_the_model_payload() {
        let mut row = Map::new();
        row.insert("customer_id".to_owned(), Value::from("cus_opaque"));
        row.insert("first_name".to_owned(), Value::from("Sensitive"));
        row.insert("phone".to_owned(), Value::from("9999999999"));
        row.insert("order_count".to_owned(), Value::from(5));
        row.insert("days_since_last_order".to_owned(), Value::from(90));
        let result = QueryResult {
            columns: row.keys().cloned().collect(),
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        let plan = SqlQueryPlan {
            sql: "SELECT 1".to_owned(),
            parameters: Vec::new(),
            result_shape: SqlResultShape::RecordList,
            title: "Customers".to_owned(),
            x_column: None,
            metrics: Vec::new(),
            visualization: SqlVisualization::None,
        };
        let summary = SummaryAnalysis {
            answer: "One customer matched.".to_owned(),
            facts: json!({"row_count": 1}),
        };
        let payload =
            AnalysisPayload::from_query("Which customers?", &plan, &result, &summary, true);
        assert!(payload.data.is_none());
        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(!serialized.contains("Sensitive"));
        assert!(!serialized.contains("9999999999"));
        assert!(serialized.contains("median_inactivity_days"));
    }
}
