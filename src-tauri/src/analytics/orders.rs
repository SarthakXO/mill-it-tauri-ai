use super::{
    dates::iso,
    query::execute,
    types::{DateRange, TimeBucket},
};
use crate::{
    agent::{AgentError, AgentResponse},
    database::{Database, QueryParameter, QueryResult},
};
use chrono::Duration;
use serde_json::Value;

pub(crate) async fn order_count(
    database: &Database,
    question: &str,
    range: &DateRange,
) -> Result<AgentResponse, AgentError> {
    let result = execute(
        database,
        r#"SELECT COUNT(*) AS order_count
           FROM "order" AS o
           WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
             AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)"#,
        date_parameters(range),
    )
    .await?;
    let count = numeric_cell(&result, "order_count").unwrap_or(0.0);
    let answer = format!(
        "{} orders were placed from {} through {}.",
        format_number(count),
        range.start.format("%b %-d, %Y"),
        (range.end_exclusive - Duration::days(1)).format("%b %-d, %Y")
    );
    Ok(AgentResponse::from_query(
        answer,
        question,
        "deterministic order count",
        "scalar",
        &result,
        None,
        None,
    ))
}

pub(crate) async fn order_timeseries(
    database: &Database,
    question: &str,
    range: &DateRange,
    bucket: TimeBucket,
) -> Result<AgentResponse, AgentError> {
    let bucket_expression = match bucket {
        TimeBucket::Day => "timezone('Asia/Kolkata', o.created_at)::date",
        TimeBucket::Week => "date_trunc('week', timezone('Asia/Kolkata', o.created_at))::date",
        TimeBucket::Month => "date_trunc('month', timezone('Asia/Kolkata', o.created_at))::date",
    };
    let sql = format!(
        "SELECT {bucket_expression} AS period, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY period ORDER BY period"
    );
    let result = execute(database, &sql, date_parameters(range)).await?;
    let points = result
        .rows
        .iter()
        .filter_map(|row| {
            Some((
                row.get("period")?.as_str()?.to_owned(),
                row.get("order_count")?.as_f64()?,
            ))
        })
        .collect::<Vec<_>>();
    let total = points.iter().map(|(_, value)| value).sum::<f64>();
    let highest = points
        .iter()
        .max_by(|left, right| left.1.total_cmp(&right.1));
    let lowest = points
        .iter()
        .min_by(|left, right| left.1.total_cmp(&right.1));
    let answer = match (highest, lowest) {
        (Some(highest), Some(lowest)) => format!(
            "The period recorded {} orders across {} {} buckets.\n\nThe highest volume was {} in {}, while the lowest was {} in {}.",
            format_number(total),
            points.len(),
            bucket.sql_unit(),
            format_number(highest.1),
            highest.0,
            format_number(lowest.1),
            lowest.0
        ),
        _ => "No matching order volume was found for that period.".to_owned(),
    };
    Ok(AgentResponse::from_query(
        answer,
        question,
        "deterministic order volume time series",
        "time_series",
        &result,
        None,
        Some("Order volume".to_owned()),
    ))
}

pub(crate) async fn status_breakdown(
    database: &Database,
    question: &str,
    range: &DateRange,
) -> Result<AgentResponse, AgentError> {
    let result = execute(
        database,
        r#"SELECT COALESCE(o.zoho_order_status::text, 'UNSPECIFIED') AS operational_status,
                  COUNT(*) AS order_count
           FROM "order" AS o
           WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
             AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
           GROUP BY o.zoho_order_status::text
           ORDER BY order_count DESC"#,
        date_parameters(range),
    )
    .await?;
    let leader = result.rows.first();
    let answer = leader.map_or_else(
        || "No matching status data was found.".to_owned(),
        |row| {
            format!(
                "{} was the largest operational-status group with {} orders during the period.",
                row.get("operational_status")
                    .and_then(Value::as_str)
                    .unwrap_or("UNSPECIFIED"),
                format_number(
                    row.get("order_count")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                )
            )
        },
    );
    Ok(AgentResponse::from_query(
        answer,
        question,
        "deterministic operational status breakdown",
        "category_breakdown",
        &result,
        None,
        Some("Orders by operational status".to_owned()),
    ))
}

pub(crate) fn date_parameters(range: &DateRange) -> Vec<QueryParameter> {
    vec![
        QueryParameter::String(iso(range.start)),
        QueryParameter::String(iso(range.end_exclusive)),
    ]
}

pub(crate) fn numeric_cell(result: &QueryResult, key: &str) -> Option<f64> {
    result.rows.first()?.get(key)?.as_f64()
}

pub(crate) fn format_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        let raw = format!("{:.0}", value.abs());
        let grouped = raw
            .chars()
            .rev()
            .enumerate()
            .fold(String::new(), |mut output, (index, character)| {
                if index > 0 && index % 3 == 0 {
                    output.push(',');
                }
                output.push(character);
                output
            })
            .chars()
            .rev()
            .collect::<String>();
        if value < 0.0 {
            format!("-{grouped}")
        } else {
            grouped
        }
    } else {
        format!("{value:.1}")
    }
}
