use super::{sql_plan::SqlPlannerResponse, AgentError};
use crate::{
    analytics::{detect_specialized_tool, latest_complete_business_date},
    intelligence::{domain_index, DomainContextPack, IntelligenceDomain},
    llm::{ollama::OllamaClient, ChatMessage},
};
use serde_json::json;

const SQL_PLANNER_PROMPT: &str = r#"
You are the Mill-it schema-aware SQL planner. Translate only the current request using only the supplied verified context documents. Return exactly one JSON object and no prose.

SQL route contract:
{"route":"sql","domains":["orders"],"query":{"sql":"SELECT ...","parameters":["2026-01-01","2026-07-01"],"result_shape":"time_series","title":"Monthly order volume","x_column":"month","metrics":["order_count"],"visualization":"bar"},"forecast_plan":null,"specialized_tool":null,"message":null}

Forecast route contract:
{"route":"forecast","domains":["orders","forecasting"],"query":null,"forecast_plan":{"metric":"order_count","training_window":{"kind":"complete_calendar_months","count":6},"forecast_horizon":{"unit":"days","count":14},"grain":"day","filters":{"exclude_cancelled":true,"status_filters":[],"store_id":null,"product":null,"variant_id":null}},"specialized_tool":null,"message":null}

Allowed route values: sql, forecast, specialized, conversation, blocked.
Allowed domains: orders, products, customers, money, fulfillment, forecasting.
Allowed result_shape: scalar, comparison, time_series, ranking, category_breakdown, record_list, table.
Allowed visualization: none, bar, horizontal_bar, line.
Allowed specialized_tool: reorder_demand, product_performance, busy_day_forecast.

Rules:
- Preserve the requested period type exactly.
  Explicit named calendar periods are calendar periods, never rolling windows.
  "May through July 2026" -> 2026-05-01 through exclusive 2026-08-01.
  "June 2026" -> 2026-06-01 through exclusive 2026-07-01.

- "Yesterday" should be anchored to historical_exclusive_boundary:
  use that boundary as the end and one day before it as the start.
  Do not generate a date after historical_exclusive_boundary.

- Only phrases such as "past N months" / "last N months" use a rolling
  calendar-month lookback.
- Ordinary descriptive historical questions use route sql. Do not demand a new API or tool.
- Forecast/predict/future expected-order questions use route forecast. Interpret semantic durations only; Rust resolves every date and generates projections. Route forecast is forbidden unless forecasting appears in selected_domains.
- Historical words such as compare, month by month, week by week, trend, Jan through June, and multiple periods use route sql unless the user explicitly asks about future expected/projected values.
- Likely/due/expected/overdue reorder, reorder-date, reorder-interval, and reorder-demand requests MUST use specialized with specialized_tool=reorder_demand. Product-performance algorithms also use specialized. `busy_day_forecast` is only for a narrow operational-preparation request; a full request for daily projections, ranges, totals, or historical-pattern explanation MUST use route forecast even if it also asks for busiest days. Ordinary top products or customer cohorts use sql.
- Use the supplied current request as authoritative. Optional context is usable only for an explicit reference.
- Use only supplied verified tables and columns. Never invent schema.
- Produce one parameterized read-only PostgreSQL SELECT or read-only CTE ending in SELECT.
- Use contiguous placeholders and a matching scalar parameter array. Never interpolate user text.
- Use Asia/Kolkata and start-inclusive/end-exclusive dates.
- Exact May 1 through May 10 means parameters 2026-05-01 and 2026-05-11, not the full month.
- “Jan through June” means 2026-01-01 through exclusive 2026-07-01.
- “list/show/give me orders” is record_list with the minimal order projection, not a count.
- Result-shape precedence: explicit comparison, time-series, grouping,
  category-breakdown, or ranking intent overrides count/how-many wording.
  Examples:
  "How many orders yesterday?" -> scalar.
  "How many orders July compared with June?" -> comparison.
  "How many orders each month?" -> time_series.
  "How many orders by store?" -> category_breakdown.
  Customer cohort count requests without grouping/comparison use scalar
  with an outer COUNT(DISTINCT customer_id) and no PII.
- Product and variant rankings use different grains.
  Product ranking -> product_variant.product_id.
  Variant/SKU ranking -> line_item.variant_id.
  Display titles are metadata only and must not define aggregation grain.
- Money must follow money.md's pre-aggregated order-grain definition exactly.
- Generic Ops order value/AOV defaults to qualifying order value: latest active/non-cancelled payment amount at order grain minus processed partial-cancellation refunds, with computed net-item value only as fallback. Never use gross line-item value alone.
- Do not use SELECT *, comments, locks, mutations, unsafe functions, sensitive data, or JSON fields.
- Keep title concise. metrics contains one through six exact returned aliases. For record-list/table plans, include the primary returned value alias even when it is an identifier rather than a numeric metric.
- x_column is the exact returned dimension alias or null.
- comparison, time_series, ranking, and category_breakdown require a non-null x_column matching the returned dimension alias.
- Scalar and record-list visualization is none.
- Current partial/future dates cannot be historical SQL periods.
- Never subtract TEXT from DATE. Cast date parameters first: (($2::text)::date - last_order_at::date).
- Rolling "past N months" MUST use historical_exclusive_boundary as its only date parameter and the requested N as a numeric parameter. Compute the start in SQL as `boundary_date - (months * INTERVAL '1 month')`; do not ask the model to calculate a start date. data_as_of is only freshness metadata.
- Customer inactivity cohorts use exactly four parameters:
  $1 = historical_exclusive_boundary date,
  $2 = eligibility rolling calendar-month count,
  $3 = minimum qualifying-order count,
  $4 = inactivity-day threshold.
  The eligibility CTE uses the rolling-month lower bound and $1 as exclusive upper bound.
  The latest-order CTE uses all qualifying orders before $1 and MUST NOT use the eligibility lower bound.
  A scalar "how many customers" request returns only COUNT(DISTINCT customer_id).
  A record-list customer request may additionally return latest order date and days_since_last_order.
- Customer inactivity record lists MUST cast the boundary parameter to date and project `(boundary_date - latest.last_order_date) AS days_since_last_order`, then order by that alias. Do not merely reference an unselected alias.
- If blocked, provide a concise safe message. Do not provide SQL.

Canonical historical monthly example for "compare orders month by month from jan to june 2026":
{"route":"sql","domains":["orders"],"query":{"sql":"SELECT date_trunc('month', timezone('Asia/Kolkata', o.created_at))::date AS month, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY month ORDER BY month","parameters":["2026-01-01","2026-07-01"],"result_shape":"time_series","title":"Monthly order volume","x_column":"month","metrics":["order_count"],"visualization":"bar"},"forecast_plan":null,"specialized_tool":null,"message":null}

- Product ranking:
  first aggregate net qualifying quantity at product_id grain using product_variant.product_id.
  If a readable product label is required, aggregate to product_id first, then LEFT JOIN product after the aggregation.
  Do not group the sales metric by variant_id, line_item.title, product_variant.title, or product.title.
  product_id is the analytical identity; product.title is display metadata only.

- Variant/SKU ranking:
  first aggregate net qualifying quantity at line_item.variant_id grain.
  If a readable variant label is required, aggregate to variant_id first, then add display metadata after the aggregation.
  Do not GROUP BY line_item.title merely to display it.
  variant_id is the analytical identity; titles are display metadata only.
"#;

pub(crate) async fn plan_sql_request(
    llm: &OllamaClient,
    current_question: &str,
    referential_context: Option<&str>,
    context: &DomainContextPack,
    data_as_of: Option<&str>,
    correction: Option<SqlPlannerCorrection<'_>>,
) -> Result<SqlPlannerResponse, AgentError> {
    let historical_exclusive_boundary = data_as_of
        .and_then(latest_complete_business_date)
        .and_then(|date| date.succ_opt())
        .map(|date| date.to_string());
    let payload = json!({
        "current_request": current_question,
        "optional_referential_context": referential_context,
        "data_as_of": data_as_of,
        "historical_exclusive_boundary": historical_exclusive_boundary,
        "business_timezone": "Asia/Kolkata",
        "selected_domains": context.domains,
        "domain_index": domain_index(),
        "verified_context_documents": context.documents,
        "correction": correction.map(|correction| json!({
            "failed_sql": correction.failed_sql,
            "safe_error": correction.safe_error,
            "instruction": if correction.structure_only {
                "Your previous response was not valid structured output. Return exactly one complete minified JSON object matching the required schema. Minify SQL, but keep the entire read-only statement: every WITH chain must end in a top-level SELECT. Preserve failed_sql when it is valid except for the stated safe_error; omit optional customer PII only if needed. Do not include Markdown, explanation, comments, or trailing content."
            } else {
                "Use the same current request and context. Return one corrected plan only."
            }
        }))
    });
    let response = llm
        .chat_json_schema(
            SQL_PLANNER_PROMPT,
            &[ChatMessage::user(payload.to_string())],
            sql_planner_schema(context.domains.contains(&IntelligenceDomain::Forecasting)),
        )
        .await?;
    let mut parsed = parse_sql_planner_response(&response).map_err(|error| {
        #[cfg(debug_assertions)]
        eprintln!("[invalid sql planner response]\n{response}");
        AgentError::InvalidPlan(error)
    })?;
    // The lightweight Rust selector is authoritative for loaded knowledge. The model's
    // domains field is output metadata, not permission to add or drop context packs.
    parsed.domains.clone_from(&context.domains);
    if requires_typed_forecast_route(current_question, context)
        && parsed.route == super::sql_plan::SqlPlannerRoute::Specialized
    {
        return Err(AgentError::InvalidPlan(
            "a full daily forecast request requires route=forecast and a semantic forecast_plan; busiest days are derived from that forecast output"
                .to_owned(),
        ));
    }
    if detect_specialized_tool(current_question)
        .is_some_and(|tool| tool.as_str() == "reorder_demand")
    {
        parsed.route = super::sql_plan::SqlPlannerRoute::Specialized;
        parsed.query = None;
        parsed.forecast_plan = None;
        parsed.specialized_tool = Some("reorder_demand".to_owned());
        parsed.message = None;
    }
    parsed.validate().map_err(AgentError::InvalidPlan)?;
    Ok(parsed)
}

fn requires_typed_forecast_route(question: &str, context: &DomainContextPack) -> bool {
    let normalized = question.to_lowercase();
    context.domains.contains(&IntelligenceDomain::Forecasting)
        && normalized.contains("forecast")
        && [
            "each day",
            "projected orders",
            "expected range",
            "historical patterns",
        ]
        .iter()
        .any(|term| normalized.contains(term))
}

fn parse_sql_planner_response(response: &str) -> Result<SqlPlannerResponse, String> {
    serde_json::from_str::<SqlPlannerResponse>(response)
        .map_err(|error| format!("SQL planner JSON: {error}"))
}

fn sql_planner_schema(allow_forecast: bool) -> serde_json::Value {
    let mut routes = vec!["sql", "specialized", "conversation", "blocked"];
    if allow_forecast {
        routes.push("forecast");
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "route": {"type": "string", "enum": routes},
            "domains": {"type": "array", "minItems": 1, "maxItems": 6, "items": {"type": "string", "enum": ["orders", "products", "customers", "money", "fulfillment", "forecasting"]}},
            "query": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "sql": {"type": "string", "minLength": 1},
                            "parameters": {"type": "array", "maxItems": 64, "items": {"type": ["string", "number", "boolean", "null"]}},
                            "result_shape": {"type": "string", "enum": ["scalar", "comparison", "time_series", "ranking", "category_breakdown", "record_list", "table"]},
                            "title": {"type": "string"},
                            "x_column": {"anyOf": [{"type": "null"}, {"type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]{0,63}$"}]},
                            "metrics": {"type": "array", "minItems": 1, "maxItems": 6, "items": {"type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]{0,63}$"}},
                            "visualization": {"type": "string", "enum": ["none", "bar", "horizontal_bar", "line"]}
                        },
                        "required": ["sql", "parameters", "result_shape", "title", "x_column", "metrics", "visualization"]
                    }
                ]
            },
            "forecast_plan": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "metric": {"type": "string", "enum": ["order_count"]},
                            "training_window": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "kind": {"type": "string", "enum": ["complete_calendar_months", "complete_calendar_weeks", "complete_days"]},
                                    "count": {"type": "integer", "minimum": 1, "maximum": 730}
                                },
                                "required": ["kind", "count"]
                            },
                            "forecast_horizon": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "unit": {"type": "string", "enum": ["days", "calendar_months"]},
                                    "count": {"type": "integer", "minimum": 1, "maximum": 90}
                                },
                                "required": ["unit", "count"]
                            },
                            "grain": {"type": "string", "enum": ["day"]},
                            "filters": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "exclude_cancelled": {"type": "boolean"},
                                    "status_filters": {"type": "array", "maxItems": 6, "items": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "properties": {
                                            "dimension": {"type": "string", "enum": ["operational", "order", "fulfillment", "payment", "delivery"]},
                                            "values": {"type": "array", "items": {"type": "string"}},
                                            "exclude": {"type": "boolean"}
                                        },
                                        "required": ["dimension", "values", "exclude"]
                                    }},
                                    "store_id": {"type": ["string", "null"]},
                                    "product": {"type": ["string", "null"]},
                                    "variant_id": {"type": ["string", "null"]}
                                },
                                "required": ["exclude_cancelled", "status_filters", "store_id", "product", "variant_id"]
                            },
                        },
                        "required": ["metric", "training_window", "forecast_horizon", "grain", "filters"]
                    }
                ]
            },
            "specialized_tool": {"type": ["string", "null"]},
            "message": {"type": ["string", "null"]}
        },
        "required": ["route", "domains", "query", "forecast_plan", "specialized_tool", "message"]
    })
}

#[derive(Clone, Copy)]
pub(crate) struct SqlPlannerCorrection<'a> {
    pub(crate) failed_sql: &'a str,
    pub(crate) safe_error: &'a str,
    pub(crate) structure_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_content_requires_structure_only_repair_then_valid_json_parses() {
        let valid = r#"{"route":"specialized","domains":["orders","products","customers","forecasting"],"query":null,"forecast_plan":null,"specialized_tool":"reorder_demand","message":null}"#;
        assert!(parse_sql_planner_response(&format!("{valid}\nexplanation")).is_err());
        let repaired = parse_sql_planner_response(valid).unwrap();
        assert_eq!(
            repaired.route,
            crate::agent::sql_plan::SqlPlannerRoute::Specialized
        );
        assert!(repaired.validate().is_ok());
    }

    #[test]
    fn full_daily_forecast_is_not_a_busy_day_specialized_request() {
        let context = crate::intelligence::select_domain_context(
            "forecast projected orders for each day and expected range",
        );
        assert!(requires_typed_forecast_route(
            "Forecast projected orders for each day and show the expected range",
            &context
        ));
    }
}
