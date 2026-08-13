use super::{
    dates::{iso, latest_complete_business_date},
    forecast_store::{ForecastSnapshot, ForecastStore},
    operation::{
        ForecastHorizon, ForecastHorizonUnit, ForecastPlan, TrainingWindow, TrainingWindowKind,
    },
    query::{execute, QUALIFYING_SALE_FILTER},
    statistics::{select_and_forecast, DailyObservation, ForecastPoint},
};
use crate::{
    agent::{AgentError, AgentResponse, ChartType},
    database::{Database, QueryParameter, QueryResult},
};
use chrono::{Duration, FixedOffset, NaiveDate, Utc};
use serde_json::{Map, Number, Value};
use std::{collections::HashMap, time::Instant};

pub(crate) struct ForecastExecution {
    pub(crate) response: AgentResponse,
    pub(crate) db_duration_ms: u128,
    pub(crate) calculation_duration_ms: u128,
    pub(crate) summary_duration_ms: u128,
    pub(crate) selected_method: String,
    pub(crate) candidate_metrics: Vec<(String, f64, f64)>,
}

pub(crate) async fn execute_forecast_plan(
    database: &Database,
    store: &ForecastStore,
    question: &str,
    request_id: &str,
    data_as_of: &str,
    plan: &ForecastPlan,
) -> Result<ForecastExecution, AgentError> {
    let resolved = plan
        .resolve(current_ist_date())
        .map_err(AgentError::InvalidPlan)?;
    #[cfg(debug_assertions)]
    eprintln!(
        "[forecast plan]\nrequest_id={request_id}\nmetric=order_count\ntraining={}..{}\nforecast={}..{}\ngrain=day",
        resolved.training_period.start,
        resolved.training_period.end_exclusive,
        resolved.forecast_period.start,
        resolved.forecast_period.end_exclusive
    );
    let history_result = historical_query(
        database,
        resolved.training_period.start,
        resolved.training_period.end_exclusive,
    )
    .await?;
    let history = complete_history(
        &history_result,
        resolved.training_period.start,
        resolved.training_period.end_exclusive,
    );
    let calculation_started = Instant::now();
    let selection = select_and_forecast(
        &history,
        resolved.forecast_period.start,
        (resolved.forecast_period.end_exclusive - resolved.forecast_period.start).num_days(),
    )
    .map_err(AgentError::InvalidPlan)?;
    let calculation_duration_ms = calculation_started.elapsed().as_millis();
    #[cfg(debug_assertions)]
    eprintln!(
        "[forecast selection]\nrequest_id={request_id}\ncandidate_count={}\nselected={}\nmae={:.3}\nwape={:.3}",
        selection.candidates.len(), selection.selected.method.label(), selection.selected.mae, selection.selected.wape
    );
    let summary_started = Instant::now();
    let comparison_result = if let Some(period) = &resolved.comparison_period {
        Some(historical_query(database, period.start, period.end_exclusive).await?)
    } else {
        None
    };
    let comparison_total = resolved
        .comparison_period
        .as_ref()
        .zip(comparison_result.as_ref())
        .map(|(period, result)| {
            complete_history(result, period.start, period.end_exclusive)
                .iter()
                .map(|row| row.orders)
                .sum::<f64>()
        });
    let answer = forecast_summary(
        &selection.points,
        comparison_total,
        selection.selected.method.label(),
        selection.selected.mae,
        selection.selected.wape,
    );
    let summary_duration_ms = summary_started.elapsed().as_millis();
    let output = forecast_result(&selection.points, &history_result);
    let snapshot = ForecastSnapshot {
        forecast_id: format!(
            "forecast-{}-{}",
            Utc::now().timestamp_millis(),
            resolved.forecast_period.start
        ),
        created_at: Utc::now().to_rfc3339(),
        data_as_of: data_as_of.to_owned(),
        plan: resolved.clone(),
        selected_method: selection.selected.method.label().to_owned(),
        backtest: selection.selected.clone(),
        candidates: selection.candidates.clone(),
        projections: selection.points.clone(),
    };
    store.save(snapshot).map_err(|error| {
        AgentError::InvalidPlan(format!(
            "could not persist local forecast snapshot: {error}"
        ))
    })?;
    let response = AgentResponse::from_query(
        answer,
        question,
        "deterministic order-count forecast",
        "time_series",
        &output,
        Some(ChartType::Line),
        Some("Projected daily orders".to_owned()),
    );
    Ok(ForecastExecution {
        response,
        db_duration_ms: history_result.duration_ms
            + comparison_result
                .as_ref()
                .map_or(0, |result| result.duration_ms),
        calculation_duration_ms,
        summary_duration_ms,
        selected_method: selection.selected.method.label().to_owned(),
        candidate_metrics: selection
            .candidates
            .iter()
            .map(|metrics| (metrics.method.label().to_owned(), metrics.mae, metrics.wape))
            .collect(),
    })
}

pub(crate) async fn evaluate_latest_forecast(
    database: &Database,
    store: &ForecastStore,
    question: &str,
    data_as_of: &str,
) -> Result<AgentResponse, AgentError> {
    let snapshot = store
        .latest()
        .map_err(|error| {
            AgentError::InvalidPlan(format!("could not load local forecast snapshot: {error}"))
        })?
        .ok_or_else(|| {
            AgentError::InvalidPlan("no saved forecast is available to evaluate".to_owned())
        })?;
    let latest_complete = latest_complete_business_date(data_as_of).ok_or_else(|| {
        AgentError::InvalidPlan("data freshness timestamp was invalid".to_owned())
    })?;
    let actual_end =
        (latest_complete + Duration::days(1)).min(snapshot.plan.forecast_period.end_exclusive);
    let actual_result = if actual_end > snapshot.plan.forecast_period.start {
        historical_query(database, snapshot.plan.forecast_period.start, actual_end).await?
    } else {
        QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            truncated: false,
            duration_ms: 0,
            data_as_of: Some(data_as_of.to_owned()),
        }
    };
    let actuals = complete_history(
        &actual_result,
        snapshot.plan.forecast_period.start,
        actual_end,
    )
    .into_iter()
    .map(|row| (row.date, row.orders))
    .collect::<HashMap<_, _>>();
    let mut rows = Vec::new();
    let mut projected_total = 0.0;
    let mut actual_total = 0.0;
    let (mut met, mut exceeded, mut missed, mut pending) = (0, 0, 0, 0);
    for point in &snapshot.projections {
        let actual = actuals.get(&point.date).copied();
        let status = evaluation_status(point, actual);
        match status {
            "pending" => pending += 1,
            "exceeded" => exceeded += 1,
            "missed" => missed += 1,
            _ => met += 1,
        }
        if let Some(actual) = actual {
            actual_total += actual;
            projected_total += point.forecast;
        }
        let mut row = Map::new();
        row.insert("date".to_owned(), Value::String(iso(point.date)));
        row.insert("projected_orders".to_owned(), rounded(point.forecast));
        row.insert(
            "actual_orders".to_owned(),
            actual.map(rounded).unwrap_or(Value::Null),
        );
        row.insert(
            "variance".to_owned(),
            actual
                .map(|value| rounded(value - point.forecast))
                .unwrap_or(Value::Null),
        );
        row.insert("status".to_owned(), Value::String(status.to_owned()));
        rows.push(row);
    }
    let variance = actual_total - projected_total;
    let variance_pct = (projected_total != 0.0).then_some(variance / projected_total * 100.0);
    let answer = format!(
        "Projected: {:.0}\nActual: {:.0}\nVariance: {:+.0}{}\n\n{} days exceeded the forecast range, {} met it, {} missed it, and {} remain pending.",
        projected_total, actual_total, variance,
        variance_pct.map(|value| format!(" ({value:+.1}%)")).unwrap_or_else(|| " (percentage not meaningful)".to_owned()),
        exceeded, met, missed, pending
    );
    let result = QueryResult {
        columns: vec![
            "date".to_owned(),
            "projected_orders".to_owned(),
            "actual_orders".to_owned(),
            "variance".to_owned(),
            "status".to_owned(),
        ],
        row_count: rows.len(),
        rows,
        truncated: false,
        duration_ms: actual_result.duration_ms,
        data_as_of: actual_result.data_as_of,
    };
    Ok(AgentResponse::from_query(
        answer,
        question,
        "saved forecast versus actual",
        "time_series",
        &result,
        Some(ChartType::Line),
        Some("Projection versus actual".to_owned()),
    ))
}

async fn historical_query(
    database: &Database,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<QueryResult, AgentError> {
    execute(database,
        &format!(r#"SELECT timezone('Asia/Kolkata', o.created_at)::date AS order_date, COUNT(*) AS order_count FROM "order" AS o WHERE {QUALIFYING_SALE_FILTER} AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY order_date ORDER BY order_date"#),
        vec![QueryParameter::String(iso(start)), QueryParameter::String(iso(end))]).await
}

fn current_ist_date() -> NaiveDate {
    let ist = FixedOffset::east_opt(5 * 60 * 60 + 30 * 60).expect("the fixed IST offset is valid");
    Utc::now().with_timezone(&ist).date_naive()
}

pub(crate) fn complete_history(
    result: &QueryResult,
    start: NaiveDate,
    end: NaiveDate,
) -> Vec<DailyObservation> {
    let by_date = result
        .rows
        .iter()
        .filter_map(|row| {
            Some((
                NaiveDate::parse_from_str(row.get("order_date")?.as_str()?, "%Y-%m-%d").ok()?,
                row.get("order_count")?.as_f64()?,
            ))
        })
        .collect::<HashMap<_, _>>();
    let mut output = Vec::new();
    let mut date = start;
    while date < end {
        output.push(DailyObservation {
            date,
            orders: by_date.get(&date).copied().unwrap_or(0.0),
        });
        date += Duration::days(1);
    }
    output
}

fn forecast_result(points: &[ForecastPoint], history: &QueryResult) -> QueryResult {
    let rows = points
        .iter()
        .map(|point| {
            let mut row = Map::new();
            row.insert("date".to_owned(), Value::String(iso(point.date)));
            row.insert("projected_orders".to_owned(), rounded(point.forecast));
            row.insert("lower_bound".to_owned(), rounded(point.low));
            row.insert("upper_bound".to_owned(), rounded(point.high));
            row.insert(
                "classification".to_owned(),
                Value::String(point.classification.clone()),
            );
            row
        })
        .collect::<Vec<_>>();
    QueryResult {
        columns: vec![
            "date".to_owned(),
            "projected_orders".to_owned(),
            "lower_bound".to_owned(),
            "upper_bound".to_owned(),
            "classification".to_owned(),
        ],
        row_count: rows.len(),
        rows,
        truncated: false,
        duration_ms: history.duration_ms,
        data_as_of: history.data_as_of.clone(),
    }
}

fn forecast_summary(
    points: &[ForecastPoint],
    comparison_total: Option<f64>,
    method: &str,
    mae: f64,
    wape: f64,
) -> String {
    let total = points.iter().map(|point| point.forecast).sum::<f64>();
    let mut busiest = points.iter().collect::<Vec<_>>();
    busiest.sort_by(|left, right| right.forecast.total_cmp(&left.forecast));
    let dates = busiest
        .iter()
        .take(3)
        .map(|point| point.date.format("%b %-d").to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let comparison = comparison_total
        .map(|previous| {
            if previous == 0.0 {
                "The previous equivalent period was zero, so percentage change is not meaningful."
                    .to_owned()
            } else {
                format!(
                    "This is {:+.1}% versus the previous equivalent complete period.",
                    (total - previous) / previous * 100.0
                )
            }
        })
        .unwrap_or_default();
    format!("The forecast projects approximately {:.0} orders across {} days, averaging {:.1} per day. {}\n\nThe busiest projected dates are {}. The selected method was {}, with backtest MAE {:.1} and WAPE {:.1}%. The projection is driven by recurring weekday patterns in the complete historical training window; no causal business explanation is inferred.", total, points.len(), total / points.len().max(1) as f64, comparison, dates, method, mae, wape)
}

fn rounded(value: f64) -> Value {
    Number::from_f64(value.round())
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn evaluation_status(point: &ForecastPoint, actual: Option<f64>) -> &'static str {
    match actual {
        None => "pending",
        Some(value) if value > point.high => "exceeded",
        Some(value) if value < point.low => "missed",
        Some(_) => "met",
    }
}

// Preserve the existing specialized busy-day entry point on the shared engine.
pub(crate) async fn busy_day_forecast(
    database: &Database,
    question: &str,
    data_as_of: &str,
    horizon_days: i64,
    lookback_months: u32,
) -> Result<AgentResponse, AgentError> {
    let plan = ForecastPlan {
        metric: super::operation::ForecastMetric::OrderCount,
        training_window: TrainingWindow {
            kind: TrainingWindowKind::CalendarMonths,
            count: lookback_months,
        },
        forecast_horizon: ForecastHorizon {
            unit: ForecastHorizonUnit::Days,
            count: u32::try_from(horizon_days)
                .map_err(|_| AgentError::InvalidPlan("forecast horizon was invalid".to_owned()))?,
        },
        grain: super::operation::TimeBucket::Day,
        filters: super::plan::AnalyticsFilters::default(),
    };
    Ok(execute_forecast_plan(
        database,
        &ForecastStore::test_default(),
        question,
        "specialized-busy-day",
        data_as_of,
        &plan,
    )
    .await?
    .response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_evaluation_uses_saved_bounds() {
        let point = ForecastPoint {
            date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            forecast: 100.0,
            low: 90.0,
            high: 110.0,
            threshold: 120.0,
            classification: "normal".to_owned(),
        };
        assert_eq!(evaluation_status(&point, Some(120.0)), "exceeded");
        assert_eq!(evaluation_status(&point, Some(100.0)), "met");
        assert_eq!(evaluation_status(&point, Some(80.0)), "missed");
        assert_eq!(evaluation_status(&point, None), "pending");
    }
}
