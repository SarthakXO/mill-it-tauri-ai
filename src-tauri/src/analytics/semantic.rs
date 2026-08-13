use super::{
    compiler::{compile, CompiledAnalyticsQuery},
    facts::build_time_series_facts,
    insights::summarize_metric_query_with_facts,
    plan::{AnalyticsPlan, Dimension, Metric, VisualizationIntent},
};
use crate::{
    agent::{AgentError, AgentResponse, ChartType, WarningBlock},
    database::{validate_sql, Database, QueryParameter, QueryResult},
};
use chrono::{Datelike, Weekday};
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

    let (partial_start_bucket, partial_end_bucket) = partial_bucket_edges(plan);

    let analytical_facts =
        build_time_series_facts(plan, &result, partial_start_bucket, partial_end_bucket);

    let calculation_duration_ms = calculation_started.elapsed().as_millis();

    #[cfg(debug_assertions)]
    if let Some(facts) = analytical_facts.as_ref() {
        eprintln!(
        "[analytics facts]\nrequest_id={}\npartial_start_bucket={}\npartial_end_bucket={}\ncomplete_bucket_count={}",
        request_id,
        facts.partial_start_bucket,
        facts.partial_end_bucket,
        facts.complete_bucket_count
    );
    }

    let summary_started = Instant::now();

    let answer = summarize_metric_query_with_facts(plan, &result, analytical_facts.as_ref());

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
    if let Some(message) = partial_bucket_warning(plan, partial_start_bucket, partial_end_bucket) {
        response.warnings.push(WarningBlock { message });
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

fn partial_bucket_edges(plan: &AnalyticsPlan) -> (bool, bool) {
    if plan.periods.len() != 1 {
        return (false, false);
    }

    let period = &plan.periods[0];

    if plan.dimensions.contains(&Dimension::Week) {
        return (
            period.start.weekday() != Weekday::Mon,
            period.end_exclusive.weekday() != Weekday::Mon,
        );
    }

    if plan.dimensions.contains(&Dimension::Month) {
        return (period.start.day() != 1, period.end_exclusive.day() != 1);
    }

    // Day-grain historical periods already use whole-day
    // start-inclusive/end-exclusive boundaries.
    (false, false)
}

fn partial_bucket_warning(
    plan: &AnalyticsPlan,
    partial_start_bucket: bool,
    partial_end_bucket: bool,
) -> Option<String> {
    if !partial_start_bucket && !partial_end_bucket {
        return None;
    }

    let (bucket, boundary) = if plan.dimensions.contains(&Dimension::Week) {
        ("calendar-week", "week")
    } else if plan.dimensions.contains(&Dimension::Month) {
        ("calendar-month", "month")
    } else {
        return None;
    };

    let message = match (
        partial_start_bucket,
        partial_end_bucket,
    ) {
        (true, true) => format!(
            "The first and last {bucket} buckets are partial because the selected range starts or ends mid-{boundary}."
        ),
        (true, false) => format!(
            "The first {bucket} bucket is partial because the selected range starts mid-{boundary}."
        ),
        (false, true) => format!(
            "The last {bucket} bucket is partial because the selected range ends mid-{boundary}."
        ),
        (false, false) => unreachable!(),
    };

    Some(message)
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
        Metric::ProductOrderCount => "semantic product order count",

        Metric::ProductUniqueCustomers => "semantic product unique buyer count",

        Metric::ProductSalesValue => "semantic product merchandise sales value",
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
        Metric::ProductOrderCount => "Product orders",
        Metric::ProductUniqueCustomers => "Product buyers",
        Metric::ProductSalesValue => "Product sales value",
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
        assert_eq!(partial_bucket_edges(&plan), (true, true));
    }
    #[test]
    fn monthly_partial_edges_are_detected() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Month],
            periods: vec![TimePeriod {
                label: "Rolling three months".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 5, 13).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
            }],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Line),
        };

        assert_eq!(partial_bucket_edges(&plan), (true, true));
    }
}
