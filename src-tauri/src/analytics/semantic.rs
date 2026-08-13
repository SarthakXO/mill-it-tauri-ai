use super::{
    compiler::{compile, CompiledAnalyticsQuery},
    insights::summarize_metric_query,
    plan::{AnalyticsPlan, Dimension, Metric, VisualizationIntent},
};
use crate::{
    agent::{AgentError, AgentResponse, ChartType, WarningBlock},
    database::{validate_sql, Database, QueryParameter, QueryResult},
};
use chrono::{Datelike, Duration, Weekday};
use serde_json::Value;
use std::time::Instant;

pub(crate) struct SemanticExecution {
    pub(crate) response: AgentResponse,
    pub(crate) rows: usize,
    pub(crate) db_duration_ms: u128,
    pub(crate) calculation_duration_ms: u128,
    pub(crate) summary_duration_ms: u128,
}

pub(crate) async fn execute_semantic_plan(
    database: &Database,
    question: &str,
    request_id: &str,
    plan: &AnalyticsPlan,
) -> Result<SemanticExecution, AgentError> {
    plan.validate()
        .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;
    let compiled = compile(plan).map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

    #[cfg(debug_assertions)]
    {
        eprintln!(
            "[analytics plan]\nrequest_id={}\ndomain={:?}\nmetrics={:?}\ndimensions={}\nperiods={}",
            request_id,
            plan.domain,
            plan.metrics,
            serde_json::to_string(&plan.dimensions).unwrap_or_else(|_| "[]".to_owned()),
            serde_json::to_string(
                &plan
                    .periods
                    .iter()
                    .map(|period| format!("{}..{}", period.start, period.end_exclusive))
                    .collect::<Vec<_>>()
            )
            .unwrap_or_else(|_| "[]".to_owned())
        );
        eprintln!(
            "[analytics query]\nrequest_id={}\nsql={}\nparams={}",
            request_id,
            compiled.sql,
            safe_parameters_for_log(&compiled.parameters)
        );
    }

    // Defense in depth: compiler output is logged, then parsed and approved by the
    // same AST policy used for generated SQL before it can reach PostgreSQL.
    let validated = validate_sql(&compiled.sql)?;
    if validated.parameter_count() != compiled.parameters.len() {
        return Err(AgentError::InvalidPlan(
            "compiled query placeholder count did not match its parameters".to_owned(),
        ));
    }
    let mut result = database
        .execute_deterministic(&validated, &compiled.parameters)
        .await?;
    let calculation_started = Instant::now();
    apply_period_labels(&mut result, &compiled);
    let calculation_duration_ms = calculation_started.elapsed().as_millis();
    let summary_started = Instant::now();
    let answer = summarize(plan, &result);
    let summary_duration_ms = summary_started.elapsed().as_millis();
    let chart_type = chart_type(plan);
    let mut response = AgentResponse::from_query(
        answer,
        question,
        semantic_intent(plan),
        compiled.expected_shape,
        &result,
        chart_type,
        Some(chart_title(plan).to_owned()),
    );
    if partial_week_edges(plan) {
        response.warnings.push(WarningBlock {
            message: "The first and last calendar-week buckets are partial because the selected range starts or ends mid-week.".to_owned(),
        });
    }
    #[cfg(debug_assertions)]
    eprintln!(
        "[analytics summary]\nrequest_id={}\nmode=deterministic\nmetrics={:?}\nrows={}",
        request_id, plan.metrics, result.row_count
    );
    Ok(SemanticExecution {
        rows: result.row_count,
        db_duration_ms: result.duration_ms,
        calculation_duration_ms,
        summary_duration_ms,
        response,
    })
}

fn apply_period_labels(result: &mut QueryResult, compiled: &CompiledAnalyticsQuery) {
    if !result.columns.iter().any(|column| column == "period_index") {
        return;
    }
    for row in &mut result.rows {
        let index = row
            .remove("period_index")
            .and_then(|value| value.as_u64().or_else(|| value.as_f64().map(|v| v as u64)))
            .unwrap_or(0);
        let label = index
            .checked_sub(1)
            .and_then(|index| compiled.period_labels.get(index as usize))
            .cloned()
            .unwrap_or_else(|| "Unknown period".to_owned());
        row.insert("period".to_owned(), Value::String(label));
    }
    result.columns.retain(|column| column != "period_index");
    result.columns.insert(0, "period".to_owned());
}

fn summarize(plan: &AnalyticsPlan, result: &QueryResult) -> String {
    summarize_metric_query(plan, result)
}

fn partial_week_edges(plan: &AnalyticsPlan) -> bool {
    if !plan.dimensions.contains(&Dimension::Week) || plan.periods.len() != 1 {
        return false;
    }
    let period = &plan.periods[0];
    period.start.weekday() != Weekday::Mon
        || (period.end_exclusive - Duration::days(1)).weekday() != Weekday::Sun
}

fn chart_type(plan: &AnalyticsPlan) -> Option<ChartType> {
    match plan.visualization {
        Some(VisualizationIntent::Bar) => Some(ChartType::Bar),
        Some(VisualizationIntent::HorizontalBar) => Some(ChartType::HorizontalBar),
        Some(VisualizationIntent::Line) => Some(ChartType::Line),
        Some(VisualizationIntent::Table | VisualizationIntent::None) | None => None,
    }
}

fn semantic_intent(plan: &AnalyticsPlan) -> &'static str {
    match plan.metrics[0] {
        Metric::OrderCount => "semantic order count",
        Metric::UniqueCustomers => "semantic unique customer count",
        Metric::QuantitySold => "semantic quantity sold",
        Metric::UniqueVariants => "semantic unique variant count",
        Metric::OrderValue => "semantic qualifying order value with computed fallback",
        Metric::AverageOrderValue => "semantic qualifying AOV with computed fallback",
        Metric::AverageItemQuantityPerOrder => "semantic average item quantity per order",
        Metric::CancelledOrderCount => "semantic cancelled order count",
        Metric::CancellationRate => "semantic cancellation rate",
    }
}

fn chart_title(plan: &AnalyticsPlan) -> &'static str {
    match plan.metrics[0] {
        Metric::OrderCount => "Order volume",
        Metric::UniqueCustomers => "Unique customers",
        Metric::QuantitySold => "Quantity sold",
        Metric::UniqueVariants => "Unique variants",
        Metric::OrderValue => "Qualifying order value",
        Metric::AverageOrderValue => "Qualifying AOV",
        Metric::AverageItemQuantityPerOrder => "Average item quantity per order",
        Metric::CancelledOrderCount => "Cancelled orders",
        Metric::CancellationRate => "Cancellation rate",
    }
}

fn safe_parameters_for_log(parameters: &[QueryParameter]) -> String {
    let values = parameters
        .iter()
        .map(|parameter| match parameter {
            QueryParameter::String(value) if safe_log_string(value) => Value::String(value.clone()),
            QueryParameter::Integer(value) => Value::from(*value),
            QueryParameter::Float(value) => Value::from(*value),
            QueryParameter::Boolean(value) => Value::from(*value),
            QueryParameter::Null => Value::Null,
            QueryParameter::String(_) => Value::String("<redacted>".to_owned()),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).unwrap_or_else(|_| "[\"<redacted>\"]".to_owned())
}

fn safe_log_string(value: &str) -> bool {
    let date = value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| index == 4 || index == 7 || character.is_ascii_digit());
    let status = value.len() <= 40
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || " _-".contains(character));
    date || status
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::plan::{AnalyticsDomain, AnalyticsFilters, SortSpec, TimePeriod};
    use chrono::NaiveDate;
    use serde_json::{Map, Value};

    #[test]
    fn period_indexes_are_replaced_only_from_the_compiled_label_map() {
        let mut row = Map::new();
        row.insert("period_index".to_owned(), Value::from(2));
        row.insert("order_count".to_owned(), Value::from(100));
        let mut result = QueryResult {
            columns: vec!["period_index".to_owned(), "order_count".to_owned()],
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        let compiled = CompiledAnalyticsQuery {
            sql: String::new(),
            parameters: Vec::new(),
            expected_shape: "comparison",
            period_labels: vec!["April 2026".to_owned(), "May 2026".to_owned()],
        };
        apply_period_labels(&mut result, &compiled);
        assert_eq!(result.rows[0].get("period"), Some(&Value::from("May 2026")));
        assert!(!result.rows[0].contains_key("period_index"));
    }

    #[test]
    fn weekly_partial_edges_are_detected() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Week],
            periods: vec![TimePeriod {
                label: "May through June".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            }],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Line),
        };
        assert!(partial_week_edges(&plan));
    }
}
