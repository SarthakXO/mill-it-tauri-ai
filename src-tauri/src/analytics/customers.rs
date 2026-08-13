use super::{
    dates::{iso, latest_complete_business_date, subtract_months},
    orders::format_number,
    query::{
        execute, execute_customer_records, NET_LINE_QUANTITY, PARTIAL_CANCEL_QUANTITY_JOIN,
        QUALIFYING_SALE_FILTER,
    },
    statistics::{median, median_absolute_deviation},
};
use crate::{
    agent::{AgentError, AgentResponse},
    database::{Database, QueryParameter, QueryResult},
};
use chrono::{Duration, NaiveDate};
use serde_json::{Map, Number, Value};
use std::collections::HashMap;

const MINIMUM_REORDER_PURCHASES: usize = 3;

fn lapsed_customer_sql(joins: &str, product_predicate: &str, return_records: bool) -> String {
    if return_records {
        format!(
            "WITH eligible AS (SELECT o.customer_id, COUNT(DISTINCT o.id) AS orders_in_lookback FROM \"order\" AS o{joins} WHERE {QUALIFYING_SALE_FILTER} AND o.customer_id IS NOT NULL AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date){product_predicate} GROUP BY o.customer_id HAVING COUNT(DISTINCT o.id) >= $3), latest AS (SELECT o.customer_id, MAX(timezone('Asia/Kolkata', o.created_at)::date) AS last_order_date, MAX(o.created_at) AS last_order_at FROM \"order\" AS o WHERE {QUALIFYING_SALE_FILTER} AND o.customer_id IS NOT NULL AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.customer_id) SELECT c.id AS customer_id, c.first_name, c.phone, eligible.orders_in_lookback, latest.last_order_at::text AS last_order_at, (($2::text)::date - latest.last_order_date) AS days_since_last_order FROM eligible JOIN latest ON latest.customer_id = eligible.customer_id JOIN customer AS c ON c.id = eligible.customer_id WHERE latest.last_order_date < (($2::text)::date - ($4::bigint * INTERVAL '1 day')) ORDER BY days_since_last_order DESC LIMIT 500"
        )
    } else {
        format!(
            "WITH eligible AS (SELECT o.customer_id FROM \"order\" AS o{joins} WHERE {QUALIFYING_SALE_FILTER} AND o.customer_id IS NOT NULL AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date){product_predicate} GROUP BY o.customer_id HAVING COUNT(DISTINCT o.id) >= $3), latest AS (SELECT o.customer_id, MAX(timezone('Asia/Kolkata', o.created_at)::date) AS last_order_date FROM \"order\" AS o WHERE {QUALIFYING_SALE_FILTER} AND o.customer_id IS NOT NULL AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.customer_id) SELECT COUNT(*) AS customer_count FROM eligible JOIN latest ON latest.customer_id = eligible.customer_id WHERE latest.last_order_date < (($2::text)::date - ($4::bigint * INTERVAL '1 day'))"
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn lapsed_customers(
    database: &Database,
    question: &str,
    data_as_of: &str,
    minimum_orders: usize,
    lookback_months: u32,
    inactive_days: i64,
    product_filter: Option<&str>,
    return_records: bool,
) -> Result<AgentResponse, AgentError> {
    let latest_complete = latest_complete_business_date(data_as_of).ok_or_else(|| {
        AgentError::InvalidPlan("data freshness timestamp was invalid".to_owned())
    })?;
    let end_exclusive = latest_complete + Duration::days(1);
    let lookback_start = subtract_months(end_exclusive, lookback_months).ok_or_else(|| {
        AgentError::InvalidPlan("customer lookback period was invalid".to_owned())
    })?;
    let (joins, product_predicate) = if product_filter.is_some() {
        (
            " JOIN line_item AS li ON li.order_id = o.id ",
            " AND lower(li.title) LIKE '%' || lower(($5::text)) || '%' ",
        )
    } else {
        ("", "")
    };
    let sql = lapsed_customer_sql(joins, product_predicate, return_records);
    let mut parameters = vec![
        QueryParameter::String(iso(lookback_start)),
        QueryParameter::String(iso(end_exclusive)),
        QueryParameter::Integer(minimum_orders as i64),
        QueryParameter::Integer(inactive_days),
    ];
    if let Some(product) = product_filter {
        parameters.push(QueryParameter::String(product.to_owned()));
    }
    let result = if return_records {
        execute_customer_records(database, &sql, parameters).await?
    } else {
        execute(database, &sql, parameters).await?
    };
    if !return_records {
        let count = result
            .rows
            .first()
            .and_then(|row| row.get("customer_count"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let product_note =
            product_filter.map_or(String::new(), |product| format!(" for {product}"));
        let answer = format!(
            "{} customers placed at least {} qualifying orders{} during the {}-month lookback and have not ordered in the last {} days.",
            format_number(count),
            minimum_orders,
            product_note,
            lookback_months,
            inactive_days
        );
        return Ok(AgentResponse::from_query(
            answer,
            question,
            "deterministic lapsed customer count",
            "scalar",
            &result,
            None,
            None,
        ));
    }
    let mut inactivity = result
        .rows
        .iter()
        .filter_map(|row| row.get("days_since_last_order").and_then(Value::as_f64))
        .collect::<Vec<_>>();
    inactivity.sort_by(f64::total_cmp);
    let median_days = median(&inactivity).unwrap_or(0.0);
    let product_note = product_filter.map_or(String::new(), |product| format!(" for {product}"));
    let answer = format!(
        "{} customers placed at least {} qualifying orders{} during the lookback period but have not ordered in the last {} days.\n\nThe median inactivity period in this group is {:.0} days.",
        result.row_count,
        minimum_orders,
        product_note,
        inactive_days,
        median_days
    );
    Ok(AgentResponse::from_query(
        answer,
        question,
        "deterministic lapsed customer analysis",
        "record_list",
        &result,
        None,
        None,
    ))
}

pub(crate) async fn reorder_demand(
    database: &Database,
    question: &str,
    data_as_of: &str,
    horizon_days: i64,
    limit: usize,
) -> Result<AgentResponse, AgentError> {
    let latest_complete = latest_complete_business_date(data_as_of).ok_or_else(|| {
        AgentError::InvalidPlan("data freshness timestamp was invalid".to_owned())
    })?;
    let end_exclusive = latest_complete + Duration::days(1);
    let start = subtract_months(end_exclusive, 12)
        .ok_or_else(|| AgentError::InvalidPlan("reorder history period was invalid".to_owned()))?;
    let sql = format!(
        "SELECT o.customer_id, li.variant_id, li.title AS historical_product_label, timezone('Asia/Kolkata', o.created_at)::date AS purchase_date, SUM({NET_LINE_QUANTITY}) AS quantity FROM line_item AS li JOIN \"order\" AS o ON o.id = li.order_id{PARTIAL_CANCEL_QUANTITY_JOIN} WHERE {QUALIFYING_SALE_FILTER} AND o.customer_id IS NOT NULL AND li.variant_id IS NOT NULL AND li.is_return = false AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.customer_id, li.variant_id, li.title, purchase_date ORDER BY o.customer_id, li.variant_id, purchase_date"
    );
    let history = execute(
        database,
        &sql,
        vec![
            QueryParameter::String(iso(start)),
            QueryParameter::String(iso(end_exclusive)),
        ],
    )
    .await?;
    let mut customer_variants = HashMap::<(String, String), PurchaseHistory>::new();
    for row in &history.rows {
        let Some(customer_id) = row.get("customer_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(variant_id) = row.get("variant_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(date) = row
            .get("purchase_date")
            .and_then(Value::as_str)
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        else {
            continue;
        };
        let quantity = row.get("quantity").and_then(Value::as_f64).unwrap_or(0.0);
        let entry = customer_variants
            .entry((customer_id.to_owned(), variant_id.to_owned()))
            .or_insert_with(|| PurchaseHistory {
                historical_product_label: row
                    .get("historical_product_label")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown product")
                    .to_owned(),
                purchases: Vec::new(),
            });
        entry.purchases.push((date, quantity));
    }
    let horizon_end = latest_complete + Duration::days(horizon_days);
    let mut variants = HashMap::<String, DemandAggregate>::new();
    let mut signal_rows = Vec::new();
    for ((customer_id, variant_id), mut purchase_history) in customer_variants {
        purchase_history.purchases.sort_by_key(|(date, _)| *date);
        let Some(signal) = reorder_signal(&purchase_history.purchases) else {
            continue;
        };
        if signal.expected_next > horizon_end {
            continue;
        }
        let aggregate = variants
            .entry(variant_id.clone())
            .or_insert_with(|| DemandAggregate {
                variant_id: variant_id.clone(),
                historical_product_label: purchase_history.historical_product_label.clone(),
                due_customers: 0,
                overdue_customers: 0,
                estimated_units: 0.0,
                high_confidence: 0,
                medium_confidence: 0,
            });
        aggregate.due_customers += 1;
        if signal.expected_next <= latest_complete {
            aggregate.overdue_customers += 1;
        }
        aggregate.estimated_units += signal.typical_quantity;
        match signal.confidence {
            "High" => aggregate.high_confidence += 1,
            "Medium" => aggregate.medium_confidence += 1,
            _ => {}
        }
        let last_purchase = purchase_history
            .purchases
            .last()
            .map(|(date, _)| *date)
            .expect("reorder history has purchases");
        let overdue_days = (latest_complete - signal.expected_next).num_days().max(0);
        signal_rows.push(Map::from_iter([
            ("customer_id".to_owned(), Value::String(customer_id)),
            ("variant_id".to_owned(), Value::String(variant_id)),
            (
                "historical_product_label".to_owned(),
                Value::String(purchase_history.historical_product_label),
            ),
            (
                "last_purchase_date".to_owned(),
                Value::String(iso(last_purchase)),
            ),
            (
                "typical_reorder_interval_days".to_owned(),
                number(signal.typical_interval_days),
            ),
            (
                "expected_reorder_date".to_owned(),
                Value::String(iso(signal.expected_next)),
            ),
            (
                "historical_median_quantity".to_owned(),
                number(signal.typical_quantity),
            ),
            (
                "confidence".to_owned(),
                Value::String(signal.confidence.to_owned()),
            ),
            (
                "overdue".to_owned(),
                Value::Bool(signal.expected_next <= latest_complete),
            ),
            ("overdue_days".to_owned(), Value::from(overdue_days)),
        ]));
    }
    signal_rows.sort_by(|left, right| {
        right
            .get("overdue_days")
            .and_then(Value::as_i64)
            .cmp(&left.get("overdue_days").and_then(Value::as_i64))
    });
    signal_rows.truncate(limit);
    let mut aggregates = variants.into_values().collect::<Vec<_>>();
    aggregates.sort_by(|left, right| {
        right
            .due_customers
            .cmp(&left.due_customers)
            .then_with(|| right.estimated_units.total_cmp(&left.estimated_units))
    });
    aggregates.truncate(limit);
    let rows = aggregates
        .iter()
        .map(|aggregate| {
            let mut row = Map::new();
            row.insert(
                "variant_id".to_owned(),
                Value::String(aggregate.variant_id.clone()),
            );
            row.insert(
                "historical_product_label".to_owned(),
                Value::String(aggregate.historical_product_label.clone()),
            );
            row.insert(
                "due_customers".to_owned(),
                Value::from(aggregate.due_customers),
            );
            row.insert(
                "overdue_customers".to_owned(),
                Value::from(aggregate.overdue_customers),
            );
            row.insert(
                "estimated_expected_units".to_owned(),
                number(aggregate.estimated_units),
            );
            row.insert(
                "confidence".to_owned(),
                Value::String(aggregate.confidence().to_owned()),
            );
            row
        })
        .collect::<Vec<_>>();
    let aggregate_output = QueryResult {
        columns: vec![
            "variant_id".to_owned(),
            "historical_product_label".to_owned(),
            "due_customers".to_owned(),
            "overdue_customers".to_owned(),
            "estimated_expected_units".to_owned(),
            "confidence".to_owned(),
        ],
        row_count: rows.len(),
        rows,
        truncated: history.truncated,
        duration_ms: history.duration_ms,
        data_as_of: history.data_as_of.clone(),
    };
    let detail_output = QueryResult {
        columns: vec![
            "customer_id".to_owned(),
            "variant_id".to_owned(),
            "historical_product_label".to_owned(),
            "last_purchase_date".to_owned(),
            "typical_reorder_interval_days".to_owned(),
            "expected_reorder_date".to_owned(),
            "historical_median_quantity".to_owned(),
            "confidence".to_owned(),
            "overdue".to_owned(),
            "overdue_days".to_owned(),
        ],
        row_count: signal_rows.len(),
        rows: signal_rows,
        truncated: history.truncated,
        duration_ms: history.duration_ms,
        data_as_of: history.data_as_of.clone(),
    };
    let answer = aggregate_output.rows.first().map_or_else(
        || "No variants had enough repeat-purchase history to produce a near-term reorder signal.".to_owned(),
        |row| format!(
            "{} has the strongest repeat-order signal for the next {} days, with {} customers approaching or exceeding their historical reorder window.\n\nEstimated expected units are based on each customer's typical historical reorder quantity, not guaranteed sales.",
            row.get("historical_product_label").and_then(Value::as_str).unwrap_or("The leading variant"),
            horizon_days,
            format_number(row.get("due_customers").and_then(Value::as_f64).unwrap_or(0.0))
        ),
    );
    let mut response = AgentResponse::from_query(
        answer,
        question,
        "deterministic customer-variant reorder demand",
        "ranking",
        &aggregate_output,
        None,
        Some("Near-term repeat-order signals".to_owned()),
    );
    let mut details = AgentResponse::from_query(
        "Customer-level reorder signals".to_owned(),
        question,
        "deterministic customer reorder signals",
        "record_list",
        &detail_output,
        None,
        None,
    );
    if let Some(table) = details.tables.first_mut() {
        table.id = "reorder-customer-signals".to_owned();
        table.title = "Customer reorder signals".to_owned();
    }
    response.tables.extend(details.tables);
    Ok(response)
}

#[derive(Debug)]
struct PurchaseHistory {
    historical_product_label: String,
    purchases: Vec<(NaiveDate, f64)>,
}

#[derive(Debug)]
struct ReorderSignal {
    expected_next: NaiveDate,
    typical_interval_days: f64,
    typical_quantity: f64,
    confidence: &'static str,
}

fn reorder_signal(purchases: &[(NaiveDate, f64)]) -> Option<ReorderSignal> {
    if purchases.len() < MINIMUM_REORDER_PURCHASES {
        return None;
    }
    let intervals = purchases
        .windows(2)
        .map(|window| (window[1].0 - window[0].0).num_days() as f64)
        .filter(|days| *days > 0.0)
        .collect::<Vec<_>>();
    if intervals.len() < 2 {
        return None;
    }
    let typical_interval = median(&intervals)?;
    let variability = median_absolute_deviation(&intervals).unwrap_or(typical_interval);
    let ratio = if typical_interval > 0.0 {
        variability / typical_interval
    } else {
        1.0
    };
    let confidence = if purchases.len() >= 4 && ratio <= 0.2 {
        "High"
    } else if ratio <= 0.5 {
        "Medium"
    } else {
        "Low"
    };
    let quantities = purchases
        .iter()
        .map(|(_, quantity)| *quantity)
        .collect::<Vec<_>>();
    Some(ReorderSignal {
        expected_next: purchases.last()?.0 + Duration::days(typical_interval.round() as i64),
        typical_interval_days: typical_interval,
        typical_quantity: median(&quantities).unwrap_or(0.0),
        confidence,
    })
}

#[derive(Debug)]
struct DemandAggregate {
    variant_id: String,
    historical_product_label: String,
    due_customers: usize,
    overdue_customers: usize,
    estimated_units: f64,
    high_confidence: usize,
    medium_confidence: usize,
}

impl DemandAggregate {
    fn confidence(&self) -> &'static str {
        if self.high_confidence * 2 >= self.due_customers {
            "High"
        } else if (self.high_confidence + self.medium_confidence) * 2 >= self.due_customers {
            "Medium"
        } else {
            "Low"
        }
    }
}

fn number(value: f64) -> Value {
    Number::from_f64((value * 10.0).round() / 10.0)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_signal_uses_median_interval() {
        let dates = [(2026, 1, 1), (2026, 1, 29), (2026, 2, 27), (2026, 3, 28)]
            .map(|(year, month, day)| (NaiveDate::from_ymd_opt(year, month, day).unwrap(), 2.0));
        let signal = reorder_signal(&dates).unwrap();
        assert_eq!(
            signal.expected_next,
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap()
        );
        assert_eq!(signal.confidence, "High");
    }

    #[test]
    fn irregular_reorder_history_has_lower_confidence() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let purchases = [0, 10, 40, 100].map(|days| (start + Duration::days(days), 1.0));
        assert_ne!(reorder_signal(&purchases).unwrap().confidence, "High");
    }

    #[test]
    fn lapsed_cohort_separates_eligibility_from_latest_qualifying_order() {
        let sql = lapsed_customer_sql("", "", true);
        let (_, latest_and_final) = sql.split_once("), latest AS (").unwrap();
        let (latest, _) = latest_and_final.split_once(") SELECT").unwrap();

        assert_eq!(sql.matches(QUALIFYING_SALE_FILTER).count(), 2);
        assert!(sql.contains("created_at) >= (($1::text)::date"));
        assert!(!latest.contains("created_at) >= (($1::text)::date"));
        assert!(latest.contains("created_at) < (($2::text)::date"));
        assert!(sql.contains("(($2::text)::date - latest.last_order_date)"));
    }
}
