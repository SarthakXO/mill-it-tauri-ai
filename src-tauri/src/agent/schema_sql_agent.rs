use super::{
    analysis_payload::AnalysisPayload,
    analytics::{is_customer_record_request, AnalyticalShape, AnalyticsRequestContext},
    insight_analyst::generate_insights,
    response::{AgentResponse, ChartType},
    sql_plan::{
        SqlPlannerResponse, SqlPlannerRoute, SqlQueryPlan, SqlResultShape, SqlVisualization,
    },
    sql_planner::{plan_sql_request, SqlPlannerCorrection},
    summary::analyze_metrics,
    AgentError,
};
use crate::{
    analytics::{
        execute_forecast_plan, execute_tool, latest_complete_business_date, plan_fast_tool,
        ForecastStore,
    },
    database::{
        validate_sql, validate_sql_with_sensitive_allowlist, Database, DatabaseError,
        QueryParameter, QueryResult, SqlPolicyError, ValidatedQuery,
    },
    intelligence::{select_domain_context, DomainContextPack},
    llm::{ollama::OllamaClient, ChatMessage, ChatProvider},
};
use chrono::NaiveDate;
use serde_json::json;
use std::time::Instant;

pub(crate) async fn respond(
    llm: &OllamaClient,
    database: &Database,
    forecast_store: &ForecastStore,
    messages: &[ChatMessage],
    request: &AnalyticsRequestContext,
    data_as_of: Option<&str>,
) -> Result<AgentResponse, AgentError> {
    let question = request.current_user_message.as_str();
    let context_started = Instant::now();
    let context = select_domain_context(question);
    let context_duration_ms = context_started.elapsed().as_millis();
    let started = Instant::now();
    let mut correction: Option<(String, String, bool)> = None;
    let mut structure_repair_used = false;
    let mut sql_repair_used = false;
    #[cfg(debug_assertions)]
    eprintln!(
        "[context selection]\nrequest_id={}\ndomains={:?}\nduration_ms={}",
        request.request_id,
        context
            .domains
            .iter()
            .map(|domain| domain.as_str())
            .collect::<Vec<_>>(),
        context_duration_ms
    );
    #[cfg(debug_assertions)]
    eprintln!(
        "[context files]\nfiles={:?}\n[context version]\n{}",
        context
            .documents
            .iter()
            .map(|document| document.name)
            .collect::<Vec<_>>(),
        context
            .fingerprints()
            .iter()
            .map(|(name, fingerprint)| format!("{name}={fingerprint}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    for attempt in 0..=2 {
        let plan_started = Instant::now();
        let planned = plan_sql_request(
            llm,
            question,
            request.referential_user_context(),
            &context,
            data_as_of,
            correction
                .as_ref()
                .map(|(sql, error, structure_only)| SqlPlannerCorrection {
                    failed_sql: sql,
                    safe_error: error,
                    structure_only: *structure_only,
                }),
        )
        .await;
        let planned = match planned {
            Ok(plan) => plan,
            Err(AgentError::InvalidPlan(detail)) if !structure_repair_used => {
                structure_repair_used = true;
                let safe_detail = detail.chars().take(300).collect::<String>();
                let prior_sql = correction
                    .as_ref()
                    .map(|(sql, _, _)| sql.clone())
                    .unwrap_or_default();
                let prior_correction = correction
                    .as_ref()
                    .map(|(_, error, _)| format!(" Preserve this prior correction too: {error}"))
                    .unwrap_or_default();
                correction = Some((
                    prior_sql,
                    format!(
                        "The prior response was invalid planner JSON or violated the result contract: {safe_detail}.{prior_correction}"
                    ),
                    true,
                ));
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = validate_selected_domains(&planned, &context) {
            if !structure_repair_used {
                structure_repair_used = true;
                correction = Some((String::new(), error, false));
                continue;
            }
            return Err(AgentError::InvalidPlan(error));
        }
        #[cfg(debug_assertions)]
        eprintln!(
            "[schema sql planner]\nrequest_id={}\nroute={:?}\nspecialized_tool={:?}\ndomains={:?}\nmodel={}\nthinking_mode={}\nattempt={}\nplanning_duration_ms={}",
            request.request_id,
            planned.route,
            planned.specialized_tool,
            planned.domains.iter().map(|domain| domain.as_str()).collect::<Vec<_>>(),
            llm.model_name(),
            llm.thinking_enabled(),
            attempt + 1,
            plan_started.elapsed().as_millis()
        );

        match planned.route {
            SqlPlannerRoute::Conversation => {
                return llm
                    .chat(messages)
                    .await
                    .map(AgentResponse::text)
                    .map_err(AgentError::from);
            }
            SqlPlannerRoute::Blocked => {
                return Ok(AgentResponse::text(planned.message.unwrap_or_else(|| {
                    "That request is outside the read-only analytics safety policy.".to_owned()
                })));
            }
            SqlPlannerRoute::Forecast => {
                let freshness = data_as_of.ok_or_else(|| {
                    AgentError::InvalidPlan("database freshness was unavailable".to_owned())
                })?;
                let forecast_plan = planned.forecast_plan.ok_or_else(|| {
                    AgentError::InvalidPlan("forecast route omitted its typed plan".to_owned())
                })?;
                let execution = execute_forecast_plan(
                    database,
                    forecast_store,
                    question,
                    &request.request_id,
                    freshness,
                    &forecast_plan,
                )
                .await?;
                return enrich_algorithm_response(
                    llm,
                    question,
                    execution.response,
                    json!({
                        "selected_method": execution.selected_method,
                        "candidate_backtests": execution.candidate_metrics
                    }),
                )
                .await;
            }
            SqlPlannerRoute::Specialized => {
                let freshness = data_as_of.ok_or_else(|| {
                    AgentError::InvalidPlan("database freshness was unavailable".to_owned())
                })?;
                let expected = planned.specialized_tool.ok_or_else(|| {
                    AgentError::InvalidPlan("specialized route omitted its algorithm".to_owned())
                })?;
                let tool_plan = plan_fast_tool(question, freshness)
                    .map_err(AgentError::InvalidPlan)?
                    .ok_or_else(|| {
                        AgentError::InvalidPlan(
                            "specialized route did not contain deterministic parameters".to_owned(),
                        )
                    })?;
                if tool_plan.kind().as_str() != expected {
                    return Err(AgentError::InvalidPlan(
                        "specialized algorithm disagreed with deterministic routing".to_owned(),
                    ));
                }
                let response = execute_tool(database, question, freshness, &tool_plan).await?;
                return enrich_algorithm_response(
                    llm,
                    question,
                    response,
                    json!({"deterministic_algorithm": expected}),
                )
                .await;
            }
            SqlPlannerRoute::Sql => {}
        }

        let mut query_plan = planned.query.ok_or_else(|| {
            AgentError::InvalidPlan("SQL route omitted its query plan".to_owned())
        })?;
        if normalize_redundant_partial_cancellation_grouping(&mut query_plan, question) {
            #[cfg(debug_assertions)]
            eprintln!(
                "[schema sql normalization]\nrequest_id={}\nrule=remove_line_input_from_product_grouping",
                request.request_id
            );
        }
        if normalize_repaired_customer_inactivity_cohort(&mut query_plan, question, sql_repair_used)
        {
            #[cfg(debug_assertions)]
            eprintln!(
                "[schema sql normalization]\nrequest_id={}\nrule=repair_customer_cohort_placeholder_roles",
                request.request_id
            );
        }
        #[cfg(debug_assertions)]
        eprintln!(
    "[schema sql proposal]\nrequest_id={}\nsql={}\nparameters={:?}\nshape={}\nx_column={:?}\nmetrics={:?}",
    request.request_id,
    query_plan.sql,
    query_plan.parameters,
    query_plan.result_shape.as_str(),
    query_plan.x_column,
    query_plan.metrics
);
        let customer_records = is_customer_record_request(question);
        let validated =
            match validate_query_plan(&query_plan, question, customer_records, data_as_of) {
                Ok(query) => query,
                Err(error) if !sql_repair_used && repairable_policy_error(&error) => {
                    sql_repair_used = true;
                    let feedback = safe_policy_feedback(&error, question, query_plan.result_shape);
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "[schema sql repair]\nrequest_id={}\nstage=validation\nsafe_error={}",
                        request.request_id, feedback
                    );
                    correction = Some((query_plan.sql.clone(), feedback, false));
                    continue;
                }
                Err(error) => return Err(AgentError::Policy(error)),
            };
        let parameters = query_plan
            .parameters
            .iter()
            .map(QueryParameter::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        #[cfg(debug_assertions)]
        eprintln!(
            "[schema sql query]\nrequest_id={}\nshape={}\nsql={}\nparameter_count={}",
            request.request_id,
            query_plan.result_shape.as_str(),
            validated.redacted_sql_for_log(),
            parameters.len()
        );
        let result = match database.execute(&validated, &parameters).await {
            Ok(result) => result,
            Err(error) if !sql_repair_used && repairable_database_error(&error) => {
                sql_repair_used = true;
                #[cfg(debug_assertions)]
                eprintln!(
                    "[schema sql repair]\nrequest_id={}\nstage=database\nsafe_error=query compatibility",
                    request.request_id
                );
                correction = Some((
                    query_plan.sql.clone(),
                    "PostgreSQL rejected the approved SELECT because of syntax, alias, placeholder, or value-type compatibility. Correct only that issue."
                        .to_owned(),
                    false,
                ));
                continue;
            }
            Err(error) => return Err(AgentError::Database(error)),
        };
        if let Err(error) = validate_result_contract(&query_plan, &result) {
            if !sql_repair_used {
                sql_repair_used = true;
                correction = Some((query_plan.sql.clone(), error, false));
                continue;
            }
            return Err(AgentError::InvalidPlan(error));
        }

        let statistics_started = Instant::now();
        let shape = analytical_shape(query_plan.result_shape);
        let deterministic = analyze_metrics(question, shape, &query_plan.metrics, &result);
        let payload = AnalysisPayload::from_query(
            question,
            &query_plan,
            &result,
            &deterministic,
            customer_records,
        );
        let statistics_duration_ms = statistics_started.elapsed().as_millis();
        let insight_started = Instant::now();
        let use_insight = should_use_insight(question, query_plan.result_shape);
        let answer = if use_insight {
            match generate_insights(llm, &payload).await {
                Ok(insights) => insights.render(&deterministic.answer),
                Err(error) => {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "[insight analyst fallback]\nrequest_id={}\nreason={}",
                        request.request_id, error
                    );
                    deterministic.answer.clone()
                }
            }
        } else {
            deterministic.answer.clone()
        };
        let insight_duration_ms = insight_started.elapsed().as_millis();
        #[cfg(debug_assertions)]
        eprintln!(
            "[schema sql completed]\nrequest_id={}\nrows={}\ncontext_duration_ms={}\ndb_duration_ms={}\nstatistics_duration_ms={}\ninsight_duration_ms={}\ntotal_duration_ms={}\nrepair_used={}\nanalyst={}\nanalysis_rows_sent={}",
            request.request_id,
            result.row_count,
            context_duration_ms,
            result.duration_ms,
            statistics_duration_ms,
            insight_duration_ms,
            started.elapsed().as_millis(),
            attempt > 0,
            if use_insight { "used" } else { "skipped" },
            payload.dataset_policy.rows_sent_to_model
        );
        return Ok(AgentResponse::from_query(
            answer,
            question,
            &query_plan.title,
            query_plan.result_shape.as_str(),
            &result,
            chart_type(query_plan.visualization),
            Some(query_plan.title.clone()),
        ));
    }

    Err(AgentError::InvalidPlan(
        "SQL planning exhausted its independent structure and policy repair budgets".to_owned(),
    ))
}

fn should_use_insight(question: &str, shape: SqlResultShape) -> bool {
    if !matches!(
        shape,
        SqlResultShape::Comparison
            | SqlResultShape::TimeSeries
            | SqlResultShape::Ranking
            | SqlResultShape::CategoryBreakdown
    ) {
        return false;
    }

    let normalized = question.to_lowercase();

    [
        "analyze",
        "analyse",
        "explain",
        "why",
        "insight",
        "interpret",
        "what does this mean",
        "what do you notice",
    ]
    .iter()
    .any(|term| normalized.contains(term))
}

pub(crate) async fn enrich_algorithm_response(
    llm: &OllamaClient,
    question: &str,
    mut response: AgentResponse,
    deterministic_facts: serde_json::Value,
) -> Result<AgentResponse, AgentError> {
    let Some(table) = response.tables.first() else {
        return Ok(response);
    };
    let result = QueryResult {
        columns: table
            .columns
            .iter()
            .map(|column| column.key.clone())
            .collect(),
        rows: table.rows.clone(),
        row_count: table.row_count,
        truncated: table.truncated,
        duration_ms: 0,
        data_as_of: response.data_as_of.clone(),
    };
    let x_column = result.columns.first().cloned();
    let metrics = result
        .columns
        .iter()
        .filter(|column| {
            result
                .rows
                .iter()
                .any(|row| row.get(*column).is_some_and(serde_json::Value::is_number))
        })
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    let plan = SqlQueryPlan {
        sql: "deterministic Rust algorithm".to_owned(),
        parameters: Vec::new(),
        result_shape: SqlResultShape::TimeSeries,
        title: table.title.clone(),
        x_column,
        metrics,
        visualization: SqlVisualization::Line,
    };
    let summary = super::summary::SummaryAnalysis {
        answer: response.answer.clone(),
        facts: deterministic_facts,
    };
    let payload = AnalysisPayload::from_query(question, &plan, &result, &summary, false);

    if should_use_insight(question, SqlResultShape::TimeSeries) {
        if let Ok(insights) = generate_insights(llm, &payload).await {
            response.answer = insights.render(&summary.answer);
        }
    }

    Ok(response)
}

fn validate_selected_domains(
    planned: &SqlPlannerResponse,
    context: &DomainContextPack,
) -> Result<(), String> {
    let exact_domains = planned.domains.len() == context.domains.len()
        && planned
            .domains
            .iter()
            .all(|domain| context.domains.contains(domain));
    if !exact_domains {
        return Err(
            "The plan must copy exactly the supplied selected_domains; do not add or omit domains."
                .to_owned(),
        );
    }
    if planned.route == SqlPlannerRoute::Forecast
        && !context
            .domains
            .contains(&crate::intelligence::IntelligenceDomain::Forecasting)
    {
        return Err(
            "A historical descriptive comparison cannot use route forecast because forecasting.md was not selected. Use route sql."
                .to_owned(),
        );
    }
    Ok(())
}

fn has_non_scalar_analytical_intent(question: &str) -> bool {
    let normalized = question.to_lowercase();

    [
        "compare",
        " compared ",
        " versus ",
        " vs ",
        " by day",
        " by week",
        " by month",
        " by store",
        " by status",
        " each day",
        " each week",
        " each month",
        " week by week",
        " month by month",
        " day by day",
        "top ",
        "ranking",
        "breakdown",
    ]
    .iter()
    .any(|term| normalized.contains(term))
}

fn validate_query_plan(
    plan: &SqlQueryPlan,
    question: &str,
    customer_records: bool,
    data_as_of: Option<&str>,
) -> Result<ValidatedQuery, SqlPolicyError> {
    let normalized = question.to_lowercase();
    let explicit_count = normalized.contains("how many") || normalized.contains("count of");

    if explicit_count
        && !has_non_scalar_analytical_intent(question)
        && plan.result_shape != SqlResultShape::Scalar
    {
        return Err(SqlPolicyError::Unsupported(
            "an ungrouped count request requires result_shape scalar".to_owned(),
        ));
    }
    let order_record_request = normalized.contains("orders")
        && ["show ", "list ", "give me "]
            .iter()
            .any(|term| normalized.contains(term))
        && ![
            "how many",
            "count",
            "breakdown",
            " by day",
            " by week",
            " by month",
        ]
        .iter()
        .any(|term| normalized.contains(term));
    if order_record_request && plan.result_shape != SqlResultShape::RecordList {
        return Err(SqlPolicyError::Unsupported(
            "show/list/give-me orders requires result_shape record_list and the five-field operational projection"
                .to_owned(),
        ));
    }
    if customer_records && plan.result_shape != SqlResultShape::RecordList {
        return Err(SqlPolicyError::Unsupported(
            "an explicit which/list customer request requires result_shape record_list at one row per customer"
                .to_owned(),
        ));
    }
    let query = if customer_records {
        validate_sql_with_sensitive_allowlist(&plan.sql, &["first_name", "phone"])?
    } else {
        validate_sql(&plan.sql)?
    };
    if query.parameter_count() != plan.parameters.len()
        || plan
            .parameters
            .iter()
            .any(|parameter| QueryParameter::try_from(parameter).is_err())
    {
        return Err(SqlPolicyError::InvalidPlaceholder);
    }
    validate_projection_shape(plan, question, customer_records, query.projection_count())?;
    validate_business_semantics(plan, question)?;
    validate_historical_parameters(plan, data_as_of)?;
    Ok(query)
}

fn normalize_redundant_partial_cancellation_grouping(
    plan: &mut SqlQueryPlan,
    question: &str,
) -> bool {
    let normalized = question.to_lowercase();
    let product_ranking = ["product", "sku", "variant", "sellable item"]
        .iter()
        .any(|term| normalized.contains(term))
        && ["top", "best", "selling", "sold", "rank", "quantity"]
            .iter()
            .any(|term| normalized.contains(term));
    if !product_ranking {
        return false;
    }
    let original = plan.sql.clone();
    for alias in ["pc", "pcq", "partial_cancelled"] {
        plan.sql = plan
            .sql
            .replace(&format!(", {alias}.partial_cancelled_qty"), "")
            .replace(&format!(",{alias}.partial_cancelled_qty"), "");
    }
    plan.sql != original
}

fn normalize_repaired_customer_inactivity_cohort(
    plan: &mut SqlQueryPlan,
    question: &str,
    sql_repair_used: bool,
) -> bool {
    let normalized = question.to_lowercase();
    let customer_inactivity = normalized.contains("customer")
        && (normalized.contains("inactive")
            || normalized.contains("have not placed")
            || normalized.contains("haven't placed")
            || normalized.contains("have not ordered"));
    let typed_parameters = plan.parameters.len() == 4
        && plan.parameters[0].as_str().is_some()
        && plan.parameters[1..]
            .iter()
            .all(serde_json::Value::is_number);
    if !sql_repair_used
        || !customer_inactivity
        || plan.result_shape != SqlResultShape::RecordList
        || !typed_parameters
    {
        return false;
    }

    let original = plan.sql.clone();
    plan.sql = plan
        .sql
        .replace("(($3::text)::date)", "(($1::text)::date)")
        .replace("(($3::TEXT)::DATE)", "(($1::TEXT)::DATE)")
        .replace(
            "HAVING COUNT(DISTINCT o.id) >= $4",
            "HAVING COUNT(DISTINCT o.id) >= $3",
        )
        .replace(
            ", latest.last_order_date FROM eligible",
            ", latest.last_order_date, (($1::text)::date - latest.last_order_date) AS days_since_last_order FROM eligible",
        );
    plan.sql != original
}

fn validate_business_semantics(plan: &SqlQueryPlan, question: &str) -> Result<(), SqlPolicyError> {
    let normalized = question.to_lowercase();
    let sql = plan.sql.to_lowercase();
    let time_request = [
        "today",
        "yesterday",
        "last ",
        "previous",
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ]
    .iter()
    .any(|term| normalized.contains(term));
    if time_request && sql.contains("created_at") && !sql.contains("asia/kolkata") {
        return Err(SqlPolicyError::Unsupported(
            "business-date queries must use Asia/Kolkata".to_owned(),
        ));
    }
    if time_request && sql.contains("created_at") && !sql.contains("timezone('asia/kolkata',") {
        return Err(SqlPolicyError::Unsupported(
            "PostgreSQL timezone must use zone first: timezone('Asia/Kolkata', timestamp_column)"
                .to_owned(),
        ));
    }
    if time_request && (sql.contains("current_date") || sql.contains("current_timestamp")) {
        return Err(SqlPolicyError::Unsupported(
            "historical periods must be anchored to supplied data freshness, not wall-clock time"
                .to_owned(),
        ));
    }
    let asks_for_filtered_population = normalized.contains("status")
        || normalized.contains("fulfilled")
        || normalized.contains("payment-pending")
        || normalized.contains("cancel")
        || normalized.contains("qualifying")
        || normalized.contains("non-cancelled")
        || normalized.contains("non-canceled")
        || normalized.contains("order value")
        || normalized.contains("aov")
        || normalized.contains("revenue");

    let has_cancellation_or_status_filter = sql.contains("canceled_at")
        || sql.contains("payment_status")
        || sql.contains("fulfillment_status")
        || sql.contains("lower(o.status")
        || sql.contains("lower (o.status")
        || sql.contains("zoho_order_status");

    if !asks_for_filtered_population
        && matches!(
            plan.result_shape,
            SqlResultShape::Scalar | SqlResultShape::Comparison | SqlResultShape::TimeSeries
        )
        && has_cancellation_or_status_filter
    {
        return Err(SqlPolicyError::Unsupported(
            "general placed-order volume must not add unrequested status or cancellation filters"
                .to_owned(),
        ));
    }
    let ranking_intent = ["top", "best", "selling", "sold", "rank", "quantity"]
        .iter()
        .any(|term| normalized.contains(term));

    let variant_ranking = ranking_intent
        && ["variant", "sku", "sellable item"]
            .iter()
            .any(|term| normalized.contains(term));

    let product_ranking = ranking_intent && normalized.contains("product") && !variant_ranking;

    let product_or_variant_ranking = product_ranking || variant_ranking;

    let raw_sku = normalized.contains("raw sku") || normalized.contains("product_variant.sku");

    if product_or_variant_ranking
        && !(sql.contains("line_item")
            && sql.contains("line_item_id")
            && sql.contains("cancelled_quantity")
            && sql.contains("greatest(")
            && sql.contains("sum(")
            && sql.contains("o.status")
            && sql.contains("zoho_order_status"))
    {
        return Err(SqlPolicyError::Unsupported(
        "product and variant rankings require net quantity after preaggregated partial cancellations and the canonical full-cancellation predicate"
            .to_owned(),
    ));
    }

    if variant_ranking && !raw_sku && !sql.contains("variant_id") {
        return Err(SqlPolicyError::Unsupported(
            "variant/SKU rankings require line_item.variant_id as the analytical identity"
                .to_owned(),
        ));
    }

    if product_ranking && !(sql.contains("product_variant") && sql.contains("product_id")) {
        return Err(SqlPolicyError::Unsupported(
        "product rankings require product_variant.product_id as the analytical product identity"
            .to_owned(),
    ));
    }

    if product_or_variant_ranking && sql.contains("canceled_at") {
        return Err(SqlPolicyError::Unsupported(
            "order.canceled_at is not analytical cancellation truth".to_owned(),
        ));
    }

    if product_or_variant_ranking
        && sql
            .rsplit_once("group by")
            .is_some_and(|(_, grouping)| grouping.contains("partial_cancelled_qty"))
    {
        return Err(SqlPolicyError::Unsupported(
        "partial cancelled quantity is line-grain input to SUM and must not appear in the ranking GROUP BY"
            .to_owned(),
    ));
    }

    if variant_ranking && !raw_sku && sql.contains(".sku") {
        return Err(SqlPolicyError::Unsupported(
            "ordinary SKU analytics must use line_item.variant_id, not sparse raw SKU".to_owned(),
        ));
    }
    if ["order value", "average order value", "aov"]
        .iter()
        .any(|term| normalized.contains(term))
        && !(sql.contains("payment")
            && sql.contains("placed_order_value")
            && sql.contains("refund_request")
            && sql.contains("refund_amount")
            && sql.contains("processed")
            && sql.contains("partial_cancellation_id")
            && sql.contains("computed_item_value")
            && sql.contains("zoho_order_status")
            && sql.contains("o.status")
            && (sql.contains("group by li.order_id") || sql.contains("group by order_id")))
    {
        return Err(SqlPolicyError::Unsupported(
            "generic order value/AOV require qualifying order value at order grain: latest active payment amount minus processed partial-cancellation refunds, with computed net-item value only as fallback"
                .to_owned(),
        ));
    }
    if normalized.contains("payment-pending")
        && !(sql.contains("payment_status") && sql.contains("awaiting"))
    {
        return Err(SqlPolicyError::Unsupported(
            "payment-pending orders require payment_status = awaiting".to_owned(),
        ));
    }
    if normalized.contains("fulfilled")
        && !normalized.contains("unfulfilled")
        && !(sql.contains("zoho_order_status") && sql.contains("fulfilled"))
    {
        return Err(SqlPolicyError::Unsupported(
            "fulfilled Ops orders require zoho_order_status = FULFILLED".to_owned(),
        ));
    }
    if normalized.contains("delivery") && sql.contains("o.delivery_status") {
        return Err(SqlPolicyError::Unsupported(
            "actual delivery analytics must use fulfillment.delivery_status aggregated to order grain"
                .to_owned(),
        ));
    }
    let customer_inactivity = normalized.contains("customer")
        && (normalized.contains("inactive")
            || normalized.contains("have not placed")
            || normalized.contains("haven't placed")
            || normalized.contains("have not ordered"));

    if customer_inactivity
        && !(sql.contains("eligible")
            && sql.contains("latest")
            && sql.contains("count(distinct")
            && sql.contains("max("))
    {
        return Err(SqlPolicyError::Unsupported(
        "customer inactivity cohorts require separate eligibility-count and latest-qualifying-order calculations"
            .to_owned(),
    ));
    }

    if customer_inactivity
        && plan.result_shape == SqlResultShape::RecordList
        && !(sql.contains("days_since_last_order")
            && (sql.contains("::date -") || sql.contains("- latest.")))
    {
        return Err(SqlPolicyError::Unsupported(
        "customer inactivity record lists require days_since_last_order using DATE minus DATE arithmetic"
            .to_owned(),
    ));
    }
    Ok(())
}

fn validate_projection_shape(
    plan: &SqlQueryPlan,
    question: &str,
    customer_records: bool,
    projections: usize,
) -> Result<(), SqlPolicyError> {
    let sql = plan.sql.to_lowercase();
    match plan.result_shape {
        SqlResultShape::Scalar if projections != 1 => Err(SqlPolicyError::Unsupported(
            "scalar shape requires exactly one projection".to_owned(),
        )),
        SqlResultShape::Comparison
        | SqlResultShape::TimeSeries
        | SqlResultShape::CategoryBreakdown
            if projections != plan.metrics.len() + 1 =>
        {
            Err(SqlPolicyError::Unsupported(
                "aggregate shape projections must exactly match x_column plus metrics".to_owned(),
            ))
        }
        SqlResultShape::Ranking
            if projections < plan.metrics.len() + 1
                || projections > plan.metrics.len() + 5 =>
        {
            Err(SqlPolicyError::Unsupported(
                "ranking projections must contain declared metrics and at most five label/period/rank columns"
                    .to_owned(),
            ))
        }
        SqlResultShape::RecordList if customer_records => {
            if (2..=6).contains(&projections)
                && (sql.contains("distinct") || sql.contains("group by") || sql.contains("with "))
                && ["customer_id", "order_count"]
                    .iter()
                    .all(|field| sql.contains(field))
            {
                Ok(())
            } else {
                Err(SqlPolicyError::Unsupported(
                    "customer record lists require one customer-grain row with customer_id, order_count, and only permitted local fields"
                        .to_owned(),
                ))
            }
        }
        SqlResultShape::RecordList => {
            if projections <= 5
                && [
                    "order_id",
                    "display_id",
                    "store_invoice_number",
                    "zoho_order_status",
                    "created_at",
                ]
                .iter()
                .all(|field| sql.contains(field))
            {
                Ok(())
            } else {
                Err(SqlPolicyError::Unsupported(
                    "order record lists require the five-field operational projection".to_owned(),
                ))
            }
        }
        SqlResultShape::Table if projections > 8 => Err(SqlPolicyError::Unsupported(
            "table results may project at most eight columns".to_owned(),
        )),
        SqlResultShape::Scalar
            if !["count(", "sum(", "avg(", "min(", "max("]
                .iter()
                .any(|function| sql.contains(function)) =>
        {
            Err(SqlPolicyError::Unsupported(
                "scalar shape requires an aggregate".to_owned(),
            ))
        }
        _ if question.to_lowercase().contains("customer")
            && plan.result_shape == SqlResultShape::Scalar
            && !(sql.contains("count(distinct") && sql.contains("customer_id")) =>
        {
            Err(SqlPolicyError::Unsupported(
                "customer counts require COUNT(DISTINCT customer_id)".to_owned(),
            ))
        }
        _ => Ok(()),
    }
}

fn validate_historical_parameters(
    plan: &SqlQueryPlan,
    data_as_of: Option<&str>,
) -> Result<(), SqlPolicyError> {
    let Some(latest_complete) = data_as_of.and_then(latest_complete_business_date) else {
        return Ok(());
    };
    let boundary = latest_complete
        .succ_opt()
        .ok_or_else(|| SqlPolicyError::Unsupported("historical boundary overflowed".to_owned()))?;
    let dates = plan
        .parameters
        .iter()
        .filter_map(|value| value.as_str())
        .filter_map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .collect::<Vec<_>>();
    for pair in dates.chunks_exact(2) {
        validate_historical_bounds(pair[0], pair[1], boundary)?;
    }
    let future_date = plan
        .parameters
        .iter()
        .filter_map(|value| value.as_str())
        .any(|value| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok_and(|date| date > boundary)
        });
    if future_date {
        Err(SqlPolicyError::Unsupported(format!(
            "historical SQL dates must not extend beyond exclusive boundary {boundary}"
        )))
    } else {
        Ok(())
    }
}

fn validate_historical_bounds(
    start: NaiveDate,
    end_exclusive: NaiveDate,
    allowed_exclusive_boundary: NaiveDate,
) -> Result<(), SqlPolicyError> {
    if start >= allowed_exclusive_boundary || end_exclusive > allowed_exclusive_boundary {
        Err(SqlPolicyError::Unsupported(format!(
            "historical SQL dates must not extend beyond exclusive boundary {allowed_exclusive_boundary}"
        )))
    } else {
        Ok(())
    }
}

fn validate_result_contract(plan: &SqlQueryPlan, result: &QueryResult) -> Result<(), String> {
    if plan.result_shape == SqlResultShape::Scalar && result.columns.len() == 1 {
        return Ok(());
    }
    if plan
        .metrics
        .iter()
        .any(|metric| !result.columns.contains(metric))
        || plan
            .x_column
            .as_ref()
            .is_some_and(|column| !result.columns.contains(column))
    {
        return Err(
            "Returned SQL aliases did not match the declared x_column and metrics.".to_owned(),
        );
    }
    Ok(())
}

fn repairable_policy_error(error: &SqlPolicyError) -> bool {
    matches!(
        error,
        SqlPolicyError::Parse | SqlPolicyError::InvalidPlaceholder | SqlPolicyError::Unsupported(_)
    ) || matches!(
        error,
        SqlPolicyError::ForbiddenRelation(relation)
            if matches!(relation.as_str(), "customers" | "orders")
    )
}

fn safe_policy_feedback(error: &SqlPolicyError, question: &str, shape: SqlResultShape) -> String {
    let normalized = question.to_lowercase();

    let customer_inactivity = normalized.contains("customer")
        && (normalized.contains("inactive")
            || normalized.contains("have not placed")
            || normalized.contains("haven't placed")
            || normalized.contains("have not ordered"));

    match error {
        SqlPolicyError::Parse => {
            "The SELECT had invalid PostgreSQL syntax. Return one complete PostgreSQL SELECT or CTE ending in SELECT. Preserve the requested metric, population, grain, parameters, and result shape."
                .to_owned()
        }

        SqlPolicyError::InvalidPlaceholder if customer_inactivity => {
            let output_contract = if shape == SqlResultShape::RecordList {
                "The final SELECT must return one row per eligible customer and may project last_order_date and (($1::text)::date - latest.last_order_date::date) AS days_since_last_order."
            } else {
                "For a scalar how-many request, the final SELECT must return exactly one aggregate such as COUNT(DISTINCT eligible.customer_id) AS customer_count. Do not project days_since_last_order."
            };

            format!(
                "Customer inactivity cohort parameters are exactly: \
                 $1=historical_exclusive_boundary date, \
                 $2=rolling calendar-month count, \
                 $3=minimum qualifying-order count, \
                 $4=inactivity-day threshold. \
                 In eligible, use created_at >= (($1::text)::date - ($2::bigint * INTERVAL '1 month')) \
                 and created_at < (($1::text)::date), then HAVING COUNT(DISTINCT o.id) >= $3. \
                 In latest, use all qualifying orders before (($1::text)::date) with no eligibility lower bound. \
                 Filter latest qualifying activity using $4. \
                 Use placeholders $1 through $4 contiguously and provide exactly four scalar parameters. \
                 {output_contract}"
            )
        }

        SqlPolicyError::InvalidPlaceholder => {
            "Placeholders must be contiguous starting at $1 and must exactly match the scalar parameter array. Do not skip placeholder numbers or include unused parameters."
                .to_owned()
        }

        SqlPolicyError::Unsupported(detail)
            if detail.contains("customer inactivity record lists") =>
        {
            "Customer inactivity record lists require separate eligible and latest CTEs. Eligibility uses the requested rolling-month window; latest qualifying order uses all qualifying history before the historical exclusive boundary. Return one row per eligible customer and project DATE minus DATE as days_since_last_order."
                .to_owned()
        }

        SqlPolicyError::Unsupported(detail)
            if detail.contains("customer inactivity cohorts") =>
        {
            "Customer inactivity cohorts require separate eligible and latest calculations. Eligibility counts qualifying orders only inside the requested rolling eligibility window. Latest qualifying order must search all qualifying history before the historical exclusive boundary without the eligibility lower bound."
                .to_owned()
        }

        SqlPolicyError::Unsupported(detail)
            if detail.contains("historical SQL dates") =>
        {
            if normalized.contains("yesterday") {
                format!(
                    "Result shape/type contract: {detail}. \
                     This request is for yesterday, not a rolling window. \
                     Use historical_exclusive_boundary as the exclusive end date and exactly one calendar day before it as the inclusive start date. \
                     Do not generate any date after historical_exclusive_boundary."
                )
            } else if customer_inactivity {
                format!(
                    "Result shape/type contract: {detail}. \
                     Customer inactivity uses historical_exclusive_boundary as $1. \
                     Compute the rolling-month eligibility start inside SQL from $1 and the month-count parameter. \
                     Do not generate a separate future or current-wall-clock date."
                )
            } else if normalized.contains("past ") || normalized.contains("last ") {
                format!(
                    "Result shape/type contract: {detail}. \
                     Preserve the requested rolling-window semantics. \
                     Anchor the rolling window to historical_exclusive_boundary and compute its start from that boundary. \
                     Do not generate a date after the boundary."
                )
            } else {
                format!(
                    "Result shape/type contract: {detail}. \
                     Preserve explicit named calendar periods exactly. \
                     Do not reinterpret named months or explicit date ranges as rolling windows, and do not generate a date after historical_exclusive_boundary."
                )
            }
        }

        SqlPolicyError::Unsupported(detail) => {
            format!("Result shape/type contract: {detail}")
        }

        SqlPolicyError::ForbiddenRelation(relation)
            if matches!(relation.as_str(), "customers" | "orders") =>
        {
            format!(
                "The schema has no plural relation {relation}; use singular customer or quoted order. Numeric threshold parameters must be JSON numbers, not numeric strings."
            )
        }

        _ => "The query violated a non-repairable safety policy.".to_owned(),
    }
}

fn repairable_database_error(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::QueryFailed(_) | DatabaseError::InvalidParameters
    )
}

const fn analytical_shape(shape: SqlResultShape) -> AnalyticalShape {
    match shape {
        SqlResultShape::Scalar => AnalyticalShape::Scalar,
        SqlResultShape::Comparison => AnalyticalShape::Comparison,
        SqlResultShape::TimeSeries => AnalyticalShape::TimeSeries,
        SqlResultShape::Ranking => AnalyticalShape::Ranking,
        SqlResultShape::CategoryBreakdown => AnalyticalShape::CategoryBreakdown,
        SqlResultShape::RecordList | SqlResultShape::Table => AnalyticalShape::RecordList,
    }
}

const fn chart_type(visualization: SqlVisualization) -> Option<ChartType> {
    match visualization {
        SqlVisualization::None => None,
        SqlVisualization::Bar => Some(ChartType::Bar),
        SqlVisualization::HorizontalBar => Some(ChartType::HorizontalBar),
        SqlVisualization::Line => Some(ChartType::Line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan(shape: SqlResultShape, sql: &str) -> SqlQueryPlan {
        SqlQueryPlan {
            sql: sql.to_owned(),
            parameters: Vec::new(),
            result_shape: shape,
            title: "Test".to_owned(),
            x_column: None,
            metrics: vec!["order_count".to_owned()],
            visualization: SqlVisualization::None,
        }
    }

    #[test]
    fn mutation_and_pii_failures_are_never_repaired() {
        assert!(!repairable_policy_error(&SqlPolicyError::ReadOnlyOnly));
        assert!(!repairable_policy_error(&SqlPolicyError::SensitiveColumn(
            "email".to_owned()
        )));
        assert!(!repairable_policy_error(
            &SqlPolicyError::MultipleStatements
        ));
        assert!(!repairable_policy_error(&SqlPolicyError::ExplicitLock));
    }

    #[test]
    fn scalar_shape_rejects_record_projection() {
        let query = plan(
            SqlResultShape::Scalar,
            "SELECT o.id AS order_id, COUNT(*) AS order_count FROM \"order\" AS o GROUP BY o.id",
        );
        let error = validate_query_plan(&query, "How many orders?", false, None).unwrap_err();
        assert!(matches!(error, SqlPolicyError::Unsupported(_)));
    }

    #[test]
    fn result_alias_contract_is_exact() {
        let mut query = plan(
            SqlResultShape::TimeSeries,
            "SELECT '2026-01-01' AS month, COUNT(*) AS wrong_alias FROM \"order\"",
        );
        query.x_column = Some("month".to_owned());
        let result = QueryResult {
            columns: vec!["month".to_owned(), "wrong_alias".to_owned()],
            rows: vec![serde_json::Map::from_iter([
                ("month".to_owned(), json!("2026-01-01")),
                ("wrong_alias".to_owned(), json!(1)),
            ])],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        assert!(validate_result_contract(&query, &result).is_err());
    }

    #[test]
    fn historical_end_may_equal_exclusive_boundary() {
        let boundary = NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        assert!(validate_historical_bounds(
            NaiveDate::from_ymd_opt(2026, 2, 13).unwrap(),
            boundary,
            boundary
        )
        .is_ok());
        assert!(validate_historical_bounds(
            NaiveDate::from_ymd_opt(2026, 2, 13).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            boundary
        )
        .is_err());
    }

    #[test]
    fn date_minus_date_customer_expression_is_accepted() {
        let mut query = plan(
            SqlResultShape::Table,
            "SELECT (($2::text)::date - latest.last_order_date::date) AS days_since_last_order FROM (SELECT MAX(timezone('Asia/Kolkata', o.created_at)::date) AS last_order_date FROM \"order\" AS o WHERE o.created_at < (($2::text)::date) AND o.created_at >= (($1::text)::date)) AS latest",
        );
        query.parameters = vec![json!("2026-02-13"), json!("2026-08-13")];
        query.metrics = vec!["days_since_last_order".to_owned()];
        assert!(validate_query_plan(&query, "days since last order", false, None).is_ok());
    }

    #[test]
    fn insight_analysis_requires_explicit_analysis_intent() {
        assert!(!should_use_insight(
            "show weekly order volume",
            SqlResultShape::TimeSeries
        ));

        assert!(should_use_insight(
            "analyze weekly order volume",
            SqlResultShape::TimeSeries
        ));

        assert!(!should_use_insight(
            "analyze order count",
            SqlResultShape::Scalar
        ));

        assert!(!should_use_insight(
            "show the product ranking",
            SqlResultShape::Ranking
        ));

        assert!(should_use_insight(
            "explain this product ranking",
            SqlResultShape::Ranking
        ));
    }

    #[test]
    fn removes_only_redundant_partial_cancel_grouping_from_product_rankings() {
        let mut query = plan(
            SqlResultShape::Ranking,
            "SELECT li.variant_id, li.title AS historical_product_label, SUM(GREATEST(li.quantity - COALESCE(pc.partial_cancelled_qty, 0), 0)) AS quantity_sold FROM line_item AS li LEFT JOIN partial_cancelled AS pc ON pc.line_item_id = li.id GROUP BY li.variant_id, li.title, pc.partial_cancelled_qty ORDER BY quantity_sold DESC",
        );
        query.x_column = Some("historical_product_label".to_owned());
        assert!(normalize_redundant_partial_cancellation_grouping(
            &mut query,
            "most selling products"
        ));
        assert!(query.sql.contains("COALESCE(pc.partial_cancelled_qty"));
        assert!(!query.sql.contains("li.title, pc.partial_cancelled_qty"));
    }

    #[test]
    fn repaired_customer_cohort_normalizes_only_known_stale_fragments() {
        let mut query = plan(
            SqlResultShape::RecordList,
            "WITH eligible AS (SELECT o.customer_id, COUNT(DISTINCT o.id) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date - ($2 * INTERVAL '1 month')) AND timezone('Asia/Kolkata', o.created_at) < (($3::text)::date) GROUP BY o.customer_id HAVING COUNT(DISTINCT o.id) >= $4), latest AS (SELECT o.customer_id, MAX(timezone('Asia/Kolkata', o.created_at)::date) AS last_order_date FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) < (($3::text)::date) GROUP BY o.customer_id) SELECT c.id AS customer_id, eligible.order_count, latest.last_order_date FROM eligible JOIN latest ON latest.customer_id = eligible.customer_id JOIN customer AS c ON c.id = eligible.customer_id WHERE latest.last_order_date < (($1::text)::date - ($4 * INTERVAL '1 day')) ORDER BY days_since_last_order DESC",
        );
        query.parameters = vec![json!("2026-08-13"), json!(6), json!(5), json!(45)];

        assert!(normalize_repaired_customer_inactivity_cohort(
            &mut query,
            "which customers have not placed an order",
            true
        ));
        assert!(!query.sql.contains("$3::text"));
        assert!(query.sql.contains("HAVING COUNT(DISTINCT o.id) >= $3"));
        assert!(query.sql.contains("AS days_since_last_order FROM eligible"));
        assert!(!normalize_repaired_customer_inactivity_cohort(
            &mut query,
            "which customers have not placed an order",
            false
        ));
    }
}
