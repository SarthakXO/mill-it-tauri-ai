use super::{
    operation::{EntityListPlan, EntityType},
    query::{execute, execute_customer_records, QUALIFYING_SALE_FILTER},
    statistics::median,
};
use crate::{
    agent::{AgentError, AgentResponse},
    database::{Database, QueryParameter},
};
use serde_json::Value;
use std::time::Instant;

pub(crate) async fn execute_entity_plan(
    database: &Database,
    question: &str,
    request_id: &str,
    plan: &EntityListPlan,
) -> Result<AgentResponse, AgentError> {
    plan.validate().map_err(AgentError::InvalidPlan)?;
    if plan.entity != EntityType::Customer {
        return Err(AgentError::InvalidPlan(
            "typed order entity lists are not enabled yet".to_owned(),
        ));
    }
    let mut parameters = vec![
        QueryParameter::String(plan.period.start.to_string()),
        QueryParameter::String(plan.period.end_exclusive.to_string()),
    ];
    let mut having = Vec::new();
    let mut latest_filters = Vec::new();
    if let Some(minimum) = plan.minimum_order_count {
        parameters.push(QueryParameter::Integer(minimum as i64));
        having.push(format!("COUNT(DISTINCT o.id) >= ${}", parameters.len()));
    }
    if let Some(maximum) = plan.maximum_order_count {
        parameters.push(QueryParameter::Integer(maximum as i64));
        having.push(format!("COUNT(DISTINCT o.id) <= ${}", parameters.len()));
    }
    if let Some(days) = plan.inactive_days {
        parameters.push(QueryParameter::Integer(days as i64));
        latest_filters.push(format!(
            "latest.last_order_date < (($2::text)::date - (${}::bigint * INTERVAL '1 day'))",
            parameters.len()
        ));
    }
    if let Some(cutoff) = plan.no_orders_since {
        parameters.push(QueryParameter::String(cutoff.to_string()));
        latest_filters.push(format!(
            "latest.last_order_date < ((${}::text)::date)",
            parameters.len()
        ));
    }
    let (joins, product_filter) = if let Some(product) = &plan.purchased_product {
        parameters.push(QueryParameter::String(product.clone()));
        (
            " JOIN line_item AS li ON li.order_id = o.id",
            format!(
                " AND lower(li.title) LIKE '%' || lower((${}::text)) || '%'",
                parameters.len()
            ),
        )
    } else {
        ("", String::new())
    };
    let having_clause = if having.is_empty() {
        String::new()
    } else {
        format!(" HAVING {}", having.join(" AND "))
    };
    let latest_where = if latest_filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", latest_filters.join(" AND "))
    };
    let base = format!(
        "WITH eligible AS (
        SELECT
            o.customer_id,
            COUNT(DISTINCT o.id) AS order_count
        FROM \"order\" AS o{joins}
        WHERE {QUALIFYING_SALE_FILTER}
          AND o.customer_id IS NOT NULL
          AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
          AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
          {product_filter}
        GROUP BY o.customer_id
        {having_clause}
    ),
    latest AS (
        SELECT
            o.customer_id,
            MAX(timezone('Asia/Kolkata', o.created_at)::date) AS last_order_date,
            MAX(o.created_at) AS last_order_at
        FROM \"order\" AS o
        WHERE {QUALIFYING_SALE_FILTER}
          AND o.customer_id IS NOT NULL
          AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
        GROUP BY o.customer_id
    )"
    );

    #[cfg(debug_assertions)]
    eprintln!(
    "[entity plan]\nrequest_id={request_id}\nentity=customer\nminimum_orders={:?}\nmaximum_orders={:?}\ninactive_days={:?}\nno_orders_since={:?}\nreturn_records={}",
    plan.minimum_order_count,
    plan.maximum_order_count,
    plan.inactive_days,
    plan.no_orders_since,
    plan.return_records
);

    if !plan.return_records {
        let sql = format!(
            "{base}
         SELECT COUNT(*) AS customer_count
         FROM eligible
         JOIN latest ON latest.customer_id = eligible.customer_id
         {latest_where}"
        );

        let result = execute(database, &sql, parameters).await?;

        let customer_count = result
            .rows
            .first()
            .and_then(|row| row.get("customer_count"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as usize;

        #[cfg(debug_assertions)]
        eprintln!(
        "[entity completed]\nrequest_id={request_id}\nrows={customer_count}\ndb_duration_ms={}\ncalculation_duration_ms=0\nsummary_duration_ms=0",
        result.duration_ms
    );

        return Ok(AgentResponse {
            answer: format!("{customer_count} customers meet the requested cohort criteria."),
            tables: Vec::new(),
            charts: Vec::new(),
            warnings: Vec::new(),
            data_as_of: result.data_as_of,
        });
    }

    parameters.push(QueryParameter::Integer(plan.limit as i64));
    let limit_parameter = parameters.len();

    let sql = format!(
        "{base}
     SELECT
         c.id AS customer_id,
         c.first_name,
         c.phone,
         eligible.order_count,
         latest.last_order_at::text AS last_order_at,
         (($2::text)::date - latest.last_order_date) AS days_since_last_order
     FROM eligible
     JOIN latest ON latest.customer_id = eligible.customer_id
     JOIN customer AS c ON c.id = eligible.customer_id
     {latest_where}
     ORDER BY days_since_last_order DESC
     LIMIT ${limit_parameter}"
    );

    let result = execute_customer_records(database, &sql, parameters).await?;
    let sql = if plan.return_records {
        format!("{base} SELECT c.id AS customer_id, c.first_name, c.phone, eligible.order_count, latest.last_order_at::text AS last_order_at, (($2::text)::date - latest.last_order_date) AS days_since_last_order FROM eligible JOIN latest ON latest.customer_id = eligible.customer_id JOIN customer AS c ON c.id = eligible.customer_id{latest_where} ORDER BY days_since_last_order DESC LIMIT ${limit_parameter}")
    } else {
        format!("{base} SELECT eligible.customer_id, eligible.order_count, latest.last_order_at::text AS last_order_at, (($2::text)::date - latest.last_order_date) AS days_since_last_order FROM eligible JOIN latest ON latest.customer_id = eligible.customer_id{latest_where} ORDER BY days_since_last_order DESC LIMIT ${limit_parameter}")
    };
    #[cfg(debug_assertions)]
    eprintln!(
        "[entity plan]\nrequest_id={request_id}\nentity=customer\nminimum_orders={:?}\nmaximum_orders={:?}\ninactive_days={:?}\nno_orders_since={:?}\nreturn_records={}",
        plan.minimum_order_count,
        plan.maximum_order_count,
        plan.inactive_days,
        plan.no_orders_since,
        plan.return_records
    );
    let calculation_started = Instant::now();
    let order_counts = result
        .rows
        .iter()
        .filter_map(|row| row.get("order_count").and_then(Value::as_f64))
        .collect::<Vec<_>>();
    let inactivity = result
        .rows
        .iter()
        .filter_map(|row| row.get("days_since_last_order").and_then(Value::as_f64))
        .collect::<Vec<_>>();
    let median_orders = median(&order_counts).unwrap_or(0.0);
    let median_inactivity = median(&inactivity).unwrap_or(0.0);
    let over_120 = inactivity.iter().filter(|days| **days > 120.0).count();
    let share = if inactivity.is_empty() {
        0.0
    } else {
        over_120 as f64 / inactivity.len() as f64 * 100.0
    };
    let calculation_duration_ms = calculation_started.elapsed().as_millis();
    let summary_started = Instant::now();
    let answer = format!(
        "{} customers meet the requested cohort criteria.\n\nMedian inactivity is {:.0} days and the median qualifying order count is {:.0}. {:.1}% have been inactive for more than 120 days.",
        result.row_count, median_inactivity, median_orders, share
    );
    let summary_duration_ms = summary_started.elapsed().as_millis();
    #[cfg(debug_assertions)]
    eprintln!(
        "[entity completed]\nrequest_id={request_id}\nrows={}\ndb_duration_ms={}\ncalculation_duration_ms={}\nsummary_duration_ms={}",
        result.row_count, result.duration_ms, calculation_duration_ms, summary_duration_ms
    );
    if plan.return_records {
        Ok(AgentResponse::from_query(
            answer,
            question,
            "typed customer cohort records",
            "record_list",
            &result,
            None,
            None,
        ))
    } else {
        Ok(AgentResponse {
            answer,
            tables: Vec::new(),
            charts: Vec::new(),
            warnings: Vec::new(),
            data_as_of: result.data_as_of,
        })
    }
}
