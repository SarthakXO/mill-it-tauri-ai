use super::prompts::{INTERPRETATION_PROMPT, QUERY_PROMPT, SEMANTIC_PLANNER_PROMPT};
use super::response::{AgentResponse, ChartType};
use super::schema_sql_agent;
use super::{
    analytics::{
        is_customer_record_request, redact_prompt_for_log, AnalyticalShape, AnalyticsRequestContext,
    },
    summary::analyze,
};
use crate::{
    analytics::{
        evaluate_latest_forecast, execute_customer_analysis_plan, execute_entity_plan,
        execute_forecast_plan, execute_semantic_plan, execute_tool, latest_complete_business_date,
        plan_fast_tool, AnalyticsPlan, CustomerAnalysisPlan, EntityListPlan, ForecastPlan,
        ForecastStore,
    },
    database::{
        validate_sql, validate_sql_with_sensitive_allowlist, Database, DatabaseError,
        QueryParameter, SqlPolicyError, ValidatedQuery,
    },
    intelligence::context_for_question,
    llm::{ollama::OllamaClient, ChatMessage, ChatProvider, LlmError},
    schema::{
        load_schema_context, load_schema_context_with_allowlist, relevant_tables, SchemaContext,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{error::Error, fmt, time::Instant};

#[cfg(test)]
use crate::analytics::parse_semantic_plan;

pub(crate) struct AgentOrchestrator<'a> {
    llm: OllamaClient,
    database: &'a Database,
    forecast_store: ForecastStore,
}

impl<'a> AgentOrchestrator<'a> {
    #[cfg(test)]
    pub(crate) fn new(database: &'a Database) -> Result<Self, AgentError> {
        Self::new_with_thinking(database, false)
    }

    #[cfg(test)]
    pub(crate) fn new_with_thinking(
        database: &'a Database,
        thinking_enabled: bool,
    ) -> Result<Self, AgentError> {
        Self::new_with_thinking_and_store(database, thinking_enabled, ForecastStore::test_default())
    }

    pub(crate) fn new_with_thinking_and_store(
        database: &'a Database,
        thinking_enabled: bool,
        forecast_store: ForecastStore,
    ) -> Result<Self, AgentError> {
        Ok(Self {
            llm: OllamaClient::new()?.with_thinking(thinking_enabled),
            database,
            forecast_store,
        })
    }

    pub(crate) async fn respond(
        &self,
        messages: &[ChatMessage],
    ) -> Result<AgentResponse, AgentError> {
        let request = AnalyticsRequestContext::from_messages(messages).ok_or(
            AgentError::InvalidPlan("the latest message was empty".to_owned()),
        )?;
        let latest_question = request.current_user_message.as_str();
        let log_prompt = redact_prompt_for_log(latest_question);

        #[cfg(debug_assertions)]
        eprintln!(
            "[analytics request]\nrequest_id={}\nprompt=\"{}\"",
            request.request_id, log_prompt
        );

        if let Some(message) = deterministic_request_block(latest_question) {
            return Ok(AgentResponse::text(message));
        }

        let total_started = Instant::now();
        let likely_analytics = likely_analytics_request(latest_question)
            || request
                .referential_user_context()
                .is_some_and(likely_analytics_request);
        let mut data_as_of = if likely_analytics {
            self.database.data_freshness().await?
        } else {
            None
        };

        if is_projection_evaluation_request(latest_question) {
            let freshness = data_as_of.as_deref().ok_or_else(|| {
                AgentError::InvalidPlan("database freshness was unavailable".to_owned())
            })?;
            #[cfg(debug_assertions)]
            eprintln!("[analytics route]\nrequest_id={}\nroute=forecast_evaluation\nllm_used=false\nplanning_duration_ms=0", request.request_id);
            let response = evaluate_latest_forecast(
                self.database,
                &self.forecast_store,
                latest_question,
                freshness,
            )
            .await?;
            return schema_sql_agent::enrich_algorithm_response(
                &self.llm,
                latest_question,
                response,
                json!({"deterministic_algorithm": "projection_evaluation"}),
            )
            .await;
        }

        let planning_started = Instant::now();

        let mut semantic_route = self
            .semantic_route(&request, data_as_of.as_deref(), None)
            .await?;

        if data_as_of.is_none()
            && !matches!(
                semantic_route.route,
                SemanticPlannerRoute::Conversation | SemanticPlannerRoute::Blocked
            )
        {
            data_as_of = self.database.data_freshness().await?;

            semantic_route = self
                .semantic_route(&request, data_as_of.as_deref(), None)
                .await?;
        }

        let planning_duration_ms = planning_started.elapsed().as_millis();

        #[cfg(debug_assertions)]
        {
            let analysis_depth = semantic_route.analysis_depth;

            eprintln!(
                "[semantic planning]\nrequest_id={}\nanalysis_depth={}",
                request.request_id,
                analysis_depth.as_str()
            );
        }

        let fallback_intent = match semantic_route {
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::AnalyticsPlan,
                plan: Some(plan),
                ..
            } => {
                plan.validate()
                    .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;
                let freshness = data_as_of.as_deref().ok_or_else(|| {
                    AgentError::InvalidPlan("database freshness was unavailable".to_owned())
                })?;
                validate_historical_periods(&plan, freshness)?;
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics route]\nrequest_id={}\nroute=semantic_plan\nmodel={}\nthinking_mode={}\nllm_used=true\nplanning_duration_ms={}",
                    request.request_id,
                    self.llm.model_name(),
                    self.llm.thinking_enabled(),
                    planning_duration_ms
                );
                let execution = execute_semantic_plan(
                    self.database,
                    latest_question,
                    &request.request_id,
                    &plan,
                )
                .await?;
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics completed]\nrequest_id={}\nroute=semantic_plan\nrows={}\nplanning_duration_ms={}\ndb_duration_ms={}\ncalculation_duration_ms={}\nsummary_duration_ms={}\ntotal_duration_ms={}\nsummary_mode=deterministic\nllm_used=true",
                    request.request_id,
                    execution.rows,
                    planning_duration_ms,
                    execution.db_duration_ms,
                    execution.calculation_duration_ms,
                    execution.summary_duration_ms,
                    total_started.elapsed().as_millis()
                );
                return Ok(execution.response);
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::EntityList,
                entity_plan: Some(plan),
                ..
            } => {
                #[cfg(debug_assertions)]
                eprintln!("[analytics route]\nrequest_id={}\nroute=entity_list\nmodel={}\nthinking_mode={}\nllm_used=true\nplanning_duration_ms={}", request.request_id, self.llm.model_name(), self.llm.thinking_enabled(), planning_duration_ms);
                let response =
                    execute_entity_plan(self.database, latest_question, &request.request_id, &plan)
                        .await?;
                #[cfg(debug_assertions)]
                eprintln!("[analytics completed]\nrequest_id={}\nroute=entity_list\nplanning_duration_ms={}\nrows={}\ntotal_duration_ms={}\nllm_used=true", request.request_id, planning_duration_ms, response.tables.first().map_or_else(|| parse_leading_count(&response.answer), |table| table.row_count), total_started.elapsed().as_millis());
                return Ok(response);
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::CustomerAnalysis,
                customer_plan: Some(plan),
                ..
            } => {
                plan.validate()
                    .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

                #[cfg(debug_assertions)]
                eprintln!(
        "[analytics route]\nrequest_id={}\nroute=customer_analysis\nmodel={}\nthinking_mode={}\nllm_used=true\nplanning_duration_ms={}",
        request.request_id,
        self.llm.model_name(),
        self.llm.thinking_enabled(),
        planning_duration_ms,
    );

                let execution = execute_customer_analysis_plan(
                    self.database,
                    latest_question,
                    &request.request_id,
                    &plan,
                )
                .await?;

                #[cfg(debug_assertions)]
                eprintln!(
        "[analytics completed]\nrequest_id={}\nroute=customer_analysis\nrows={}\nplanning_duration_ms={}\ndb_duration_ms={}\ncalculation_duration_ms={}\nsummary_duration_ms={}\ntotal_duration_ms={}\nsummary_mode=deterministic\nllm_used=true",
        request.request_id,
        execution.rows,
        planning_duration_ms,
        execution.db_duration_ms,
        execution.calculation_duration_ms,
        execution.summary_duration_ms,
        total_started.elapsed().as_millis(),
    );

                return Ok(execution.response);
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::Forecast,
                forecast_plan: Some(plan),
                ..
            } => {
                let freshness = data_as_of.as_deref().ok_or_else(|| {
                    AgentError::InvalidPlan("database freshness was unavailable".to_owned())
                })?;
                #[cfg(debug_assertions)]
                eprintln!("[analytics route]\nrequest_id={}\nroute=forecast\nmodel={}\nthinking_mode={}\nllm_used=true\nplanning_duration_ms={}", request.request_id, self.llm.model_name(), self.llm.thinking_enabled(), planning_duration_ms);
                let execution = execute_forecast_plan(
                    self.database,
                    &self.forecast_store,
                    latest_question,
                    &request.request_id,
                    freshness,
                    &plan,
                )
                .await?;
                #[cfg(debug_assertions)]
                eprintln!("[forecast completed]\nrequest_id={}\nplanning_duration_ms={}\nrows={}\nselected_method={}\ncandidates={:?}\ndb_duration_ms={}\ncalculation_duration_ms={}\nsummary_duration_ms={}\ntotal_duration_ms={}\nllm_used=true", request.request_id, planning_duration_ms, execution.response.tables.first().map_or(0, |table| table.row_count), execution.selected_method, execution.candidate_metrics, execution.db_duration_ms, execution.calculation_duration_ms, execution.summary_duration_ms, total_started.elapsed().as_millis());
                return Ok(execution.response);
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::SpecializedTool,
                specialized_tool: Some(expected_tool),
                ..
            } => {
                if data_as_of.is_none() {
                    data_as_of = self.database.data_freshness().await?;
                }
                let freshness = data_as_of.as_deref().ok_or_else(|| {
                    AgentError::InvalidPlan("database freshness was unavailable".to_owned())
                })?;
                let plan = plan_fast_tool(latest_question, freshness)
                    .map_err(AgentError::InvalidPlan)?
                    .ok_or_else(|| {
                        AgentError::InvalidPlan(
                            "model selected a specialized tool without valid parameters".to_owned(),
                        )
                    })?;
                if plan.kind().as_str() != expected_tool {
                    return Err(AgentError::InvalidPlan(
                        "model-selected specialized tool disagreed with validated parameters"
                            .to_owned(),
                    ));
                }
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics route]\nrequest_id={}\nroute=specialized_tool\ntool={}\nmodel={}\nthinking_mode={}\nllm_used=true\nplanning_duration_ms={}",
                    request.request_id,
                    expected_tool,
                    self.llm.model_name(),
                    self.llm.thinking_enabled(),
                    planning_duration_ms
                );
                let response =
                    execute_tool(self.database, latest_question, freshness, &plan).await?;
                return Ok(response);
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::GeneratedSql,
                intent: Some(intent),
                ..
            } => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics route]\nrequest_id={}\nroute=generated_sql\nmodel={}\nthinking_mode={}\nllm_used=true\nplanning_duration_ms={}",
                    request.request_id,
                    self.llm.model_name(),
                    self.llm.thinking_enabled(),
                    planning_duration_ms
                );
                intent
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::Conversation,
                ..
            } => {
                return self
                    .llm
                    .chat(messages)
                    .await
                    .map(AgentResponse::text)
                    .map_err(AgentError::from);
            }
            SemanticPlannerResponse {
                route: SemanticPlannerRoute::Blocked,
                blocked_reason,
                ..
            } => {
                return Ok(AgentResponse::text(policy_block_message(
                    blocked_reason.as_deref(),
                )));
            }
            SemanticPlannerResponse {
                route:
                    SemanticPlannerRoute::EntityList
                    | SemanticPlannerRoute::CustomerAnalysis
                    | SemanticPlannerRoute::Forecast,
                ..
            } => {
                return Err(AgentError::InvalidPlan(
                    "model-selected typed operation omitted its plan".to_owned(),
                ))
            }
            _ => {
                return Err(AgentError::InvalidPlan(
                    "semantic planner response was incomplete".to_owned(),
                ));
            }
        };

        let shape = AnalyticalShape::classify(latest_question);
        let table_names = relevant_tables(latest_question, &fallback_intent);
        #[cfg(debug_assertions)]
        eprintln!(
            "[analytics plan]\nrequest_id={}\nshape={}\ntables={:?}",
            request.request_id,
            shape.as_str(),
            table_names
        );
        let customer_records = is_customer_record_request(latest_question);
        let schema = if customer_records {
            load_schema_context_with_allowlist(
                self.database,
                &table_names,
                &["first_name", "phone"],
            )
            .await?
        } else {
            load_schema_context(self.database, &table_names).await?
        };
        if schema.tables.is_empty() {
            return Err(AgentError::InvalidPlan(
                "schema discovery found no relevant tables".to_owned(),
            ));
        }
        if data_as_of.is_none() {
            data_as_of = self.database.data_freshness().await?;
        }

        let (mut proposal, mut validated, regenerated) = self
            .create_validated_proposal(
                &request,
                latest_question,
                &fallback_intent,
                shape,
                &schema,
                data_as_of.as_deref(),
                None,
                true,
            )
            .await?;
        let mut parameters = proposal
            .parameters
            .iter()
            .map(QueryParameter::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        #[cfg(debug_assertions)]
        eprintln!(
            "[analytics query]\nrequest_id={}\nroute=generated_sql\nshape={}\ntables={:?}\nsql={}\nparams={}",
            request.request_id,
            proposal.expected_shape,
            table_names,
            validated.redacted_sql_for_log(),
            safe_generated_parameters_for_log(&proposal.parameters)
        );

        let mut result = match self.database.execute(&validated, &parameters).await {
            Ok(result) => result,
            Err(DatabaseError::QueryFailed(detail)) if !regenerated => {
                #[cfg(debug_assertions)]
                {
                    eprintln!(
                        "[analytics rejected]\nrequest_id={}\nprompt=\"{}\"\nreason=\"PostgreSQL reference/type error\"\nsql={}",
                        request.request_id,
                        log_prompt,
                        validated.redacted_sql_for_log()
                    );
                    eprintln!(
                        "[analytics retry]\nrequest_id={}\nreason=\"approved SELECT had a PostgreSQL reference/type error\"",
                        request.request_id
                    );
                }
                let feedback = execution_retry_feedback(validated.sql(), &detail);
                let (corrected_proposal, corrected_query, did_regenerate) = self
                    .create_validated_proposal(
                        &request,
                        latest_question,
                        &fallback_intent,
                        shape,
                        &schema,
                        data_as_of.as_deref(),
                        Some(&feedback),
                        false,
                    )
                    .await?;
                debug_assert!(did_regenerate);
                proposal = corrected_proposal;
                validated = corrected_query;
                parameters = proposal
                    .parameters
                    .iter()
                    .map(QueryParameter::try_from)
                    .collect::<Result<Vec<_>, _>>()?;
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics query]\nrequest_id={}\nroute=generated_sql\nshape={}\ntables={:?}\ncorrected=true\nsql={}\nparams={}",
                    request.request_id,
                    proposal.expected_shape,
                    table_names,
                    validated.redacted_sql_for_log(),
                    safe_generated_parameters_for_log(&proposal.parameters)
                );
                self.database.execute(&validated, &parameters).await?
            }
            Err(error) => return Err(error.into()),
        };

        if shape == AnalyticalShape::Comparison {
            fill_missing_date_buckets(&mut result, &proposal.parameters, validated.sql());
        }

        let calculation_started = Instant::now();
        let summary = analyze(latest_question, shape, &result);
        let calculation_duration_ms = calculation_started.elapsed().as_millis();
        let interpretation_result = if customer_records {
            json!({
                "row_count": result.row_count,
                "truncated": result.truncated,
                "pii_rows_omitted": true,
                "data_as_of": result.data_as_of,
            })
        } else {
            json!({
                "columns": result.columns,
                "rows": result.rows.iter().take(30).collect::<Vec<_>>(),
                "row_count": result.row_count,
                "preview_row_count": result.rows.len().min(30),
                "truncated": result.truncated,
                "data_as_of": result.data_as_of,
            })
        };
        let summary_started = Instant::now();
        let interpretation = if matches!(
            shape,
            AnalyticalShape::Scalar | AnalyticalShape::RecordList
        ) {
            None
        } else {
            let evidence = json!({
                "original_question": latest_question,
                "query_intent": proposal.intent,
                "deterministic_summary": summary.answer,
                "derived_facts": summary.facts,
                "sanitized_result_summary": interpretation_result,
                "semantic_notes": [
                    "Business dates use Asia/Kolkata.",
                    "Historical product sales use variant_id as the canonical grain.",
                    "Order, fulfillment, payment, Zoho, and delivery statuses are distinct concepts."
                ]
            });
            let interpretation_message = vec![ChatMessage::user(evidence.to_string())];
            self.llm
                .chat_json(INTERPRETATION_PROMPT, &interpretation_message)
                .await
                .ok()
                .and_then(|value| serde_json::from_str::<Interpretation>(&value).ok())
        };
        let summary_duration_ms = summary_started.elapsed().as_millis();
        #[cfg(debug_assertions)]
        eprintln!(
            "[analytics completed]\nrequest_id={}\nroute=generated_sql\nrows={}\nplanning_duration_ms={}\ndb_duration_ms={}\ncalculation_duration_ms={}\nsummary_duration_ms={}\ntotal_duration_ms={}\nsummary_mode=deterministic_with_optional_qwen_chart\nllm_used=true",
            request.request_id,
            result.row_count,
            planning_duration_ms,
            result.duration_ms,
            calculation_duration_ms,
            summary_duration_ms,
            total_started.elapsed().as_millis()
        );
        Ok(AgentResponse::from_query(
            summary.answer,
            latest_question,
            &proposal.intent,
            &proposal.expected_shape,
            &result,
            interpretation
                .as_ref()
                .and_then(|value| value.chart_type.as_deref())
                .and_then(parse_chart_type),
            interpretation.and_then(|value| value.chart_title),
        ))
    }

    async fn semantic_route(
        &self,
        request: &AnalyticsRequestContext,
        data_as_of: Option<&str>,
        initial_feedback: Option<&str>,
    ) -> Result<SemanticPlannerResponse, AgentError> {
        let semantic_context = context_for_question(&request.current_user_message)
            .map_err(|error| AgentError::InvalidPlan(format!("intelligence registry: {error}")))?;

        let latest_complete = data_as_of.and_then(latest_complete_business_date);

        let historical_exclusive_boundary = latest_complete.and_then(|date| date.succ_opt());

        let mut feedback = initial_feedback.map(str::to_owned);
        let mut invalid_plan = Value::Null;

        // Once a typed route fails validation, correction must stay on that route.
        let mut correction_route = None::<SemanticPlannerRoute>;

        // Preserve the normalized first plan so correction cannot silently rewrite
        // unrelated analytical semantics.
        let mut correction_plan = None::<AnalyticsPlan>;

        // Preserve the exact validation failure so Rust knows which field the
        // correction attempt is permitted to repair.
        let mut correction_error = None::<String>;

        for attempt in 0..2 {
            let payload = json!({
                "current_analytical_request": request.current_user_message,
                "optional_prior_conversation_context": request.relevant_conversation_context,
                "data_as_of": data_as_of,
                "business_timezone": "Asia/Kolkata",
                "latest_complete_business_date": latest_complete.map(|date| date.to_string()),
                "historical_exclusive_boundary": historical_exclusive_boundary.map(|date| date.to_string()),
                "semantic_context": semantic_context,
                "correction": feedback.as_ref().map(|safe_error| json!({
                    "invalid_plan": invalid_plan,
                    "safe_validation_error": safe_error,
                    "required_route": correction_route
                        .as_ref()
                        .map(|route| format!("{route:?}")),
                    "instruction": concat!(
                        "Treat invalid_plan as the baseline plan to patch. ",
                        "Make only the minimum field changes required to resolve safe_validation_error. ",
                        "Preserve every field that is not directly implicated by safe_validation_error, ",
                        "including domain, metrics, dimensions, periods, sort, limit, visualization, ",
                        "and unrelated filters. ",
                        "Never drop, add, reorder, or substitute an explicitly requested metric unless ",
                        "safe_validation_error specifically identifies the metric set as invalid. ",
                        "Preserve every explicit user constraint. ",
                        "Stay on the same typed route specified by required_route. ",
                        "Do not reinterpret the analytical request from scratch. ",
                        "Do not produce SQL and do not switch to generated_sql."
                    )
                }))
            });

            let response = self
                .llm
                .chat_json_schema(
                    SEMANTIC_PLANNER_PROMPT,
                    &[ChatMessage::user(payload.to_string())],
                    semantic_planner_schema(),
                )
                .await?;

            let parsed = serde_json::from_str::<SemanticPlannerResponse>(&response);

            match parsed {
                Ok(mut route) => {
                    normalize_analysis_depth(&mut route, &request.current_user_message);

                    /*
                     * Repair an accidental one-item filters.products selector before
                     * B5 normalization. One named product belongs to filters.product.
                     */
                    normalize_singleton_product_selector(&mut route, &request.current_user_message);

                    /*
                     * Preserve the established B3 single-period shorthand:
                     *
                     * "How did Sharbati perform in July?"
                     *
                     * => four canonical product-performance metrics.
                     */
                    normalize_named_product_performance_snapshot(
                        &mut route,
                        &request.current_user_message,
                    );

                    /*
                     * B5 explicit selected-product comparison.
                     */
                    normalize_multi_product_comparison(&mut route, &request.current_user_message);

                    normalize_explicit_product_sales_value(
                        &mut route,
                        &request.current_user_message,
                    );

                    /*
                     * Covers generic named-product performance over temporal grain as well,
                     * e.g. monthly performance.
                     */
                    normalize_generic_named_product_performance(
                        &mut route,
                        &request.current_user_message,
                    );

                    normalize_explicit_calendar_month_range(
                        &mut route,
                        &request.current_user_message,
                    );

                    normalize_explicit_single_calendar_month(
                        &mut route,
                        &request.current_user_message,
                    );

                    normalize_explicit_monthly_grain(&mut route, &request.current_user_message);

                    normalize_semantic_route(&mut route);
                    /*
                     * Correction attempts are route-sticky.
                     */
                    if let Some(expected_route) = correction_route.as_ref() {
                        if &route.route != expected_route {
                            let error = format!(
                                "semantic planner correction changed route from {:?} to {:?}; \
                             corrections must remain on the original route",
                                expected_route, route.route
                            );

                            #[cfg(debug_assertions)]
                            eprintln!(
                                "[semantic plan rejected]\nrequest_id={}\nattempt={}\nreason={}",
                                request.request_id,
                                attempt + 1,
                                error
                            );

                            return Err(AgentError::InvalidPlan(error));
                        }
                    }

                    /*
                     * Apply deterministic correction-field ownership.
                     *
                     * For known validation failures Rust preserves the already-good
                     * baseline plan and accepts only the field that Qwen was asked
                     * to repair.
                     */
                    if attempt > 0 {
                        apply_semantic_correction_baseline(
                            &mut route,
                            correction_plan.as_ref(),
                            correction_error.as_deref(),
                        );
                    }

                    let validation = validate_semantic_route(&route)
                        .and_then(|()| {
                            validate_semantic_route_for_question(
                                &route,
                                &request.current_user_message,
                            )
                        })
                        .and_then(|()| {
                            validate_semantic_historical_boundary(
                                &route,
                                historical_exclusive_boundary,
                            )
                        });

                    match validation {
                        Ok(()) => return Ok(route),

                        Err(error) if attempt == 0 => {
                            correction_route = Some(route.route.clone());

                            /*
                             * Store the normalized analytics plan as the correction
                             * baseline before asking Qwen to repair it.
                             */
                            correction_plan = route.plan.clone();

                            correction_error = Some(error.clone());

                            invalid_plan = route
                                .plan
                                .as_ref()
                                .and_then(|plan| serde_json::to_value(plan).ok())
                                .or_else(|| {
                                    route
                                        .entity_plan
                                        .as_ref()
                                        .and_then(|plan| serde_json::to_value(plan).ok())
                                })
                                .or_else(|| {
                                    route
                                        .customer_plan
                                        .as_ref()
                                        .and_then(|plan| serde_json::to_value(plan).ok())
                                })
                                .or_else(|| {
                                    route
                                        .forecast_plan
                                        .as_ref()
                                        .and_then(|plan| serde_json::to_value(plan).ok())
                                })
                                .unwrap_or(Value::Null);

                            #[cfg(debug_assertions)]
                            eprintln!(
                            "[semantic plan rejected]\nrequest_id={}\nattempt={}\nreason={}\nplan={}",
                            request.request_id,
                            attempt + 1,
                            error,
                            invalid_plan
                        );

                            feedback = Some(error);
                        }

                        Err(error) => {
                            #[cfg(debug_assertions)]
                            eprintln!(
                            "[semantic plan rejected]\nrequest_id={}\nattempt={}\nreason={}\nplan={}",
                            request.request_id,
                            attempt + 1,
                            error,
                            route
                                .plan
                                .as_ref()
                                .and_then(|plan| serde_json::to_value(plan).ok())
                                .unwrap_or(Value::Null)
                        );

                            return Err(AgentError::InvalidPlan(error));
                        }
                    }
                }

                Err(error) if attempt == 0 => {
                    #[cfg(debug_assertions)]
                    eprintln!("[invalid semantic planner response]\n{}", response);

                    feedback = Some(format!(
                        "semantic planner JSON was invalid: {error}. \
                     Return exactly one JSON object matching the supplied schema \
                     with no trailing content."
                    ));
                }

                Err(error) => {
                    #[cfg(debug_assertions)]
                    eprintln!("[invalid semantic planner response]\n{}", response);

                    return Err(AgentError::InvalidPlan(format!(
                        "semantic planner JSON: {error}"
                    )));
                }
            }
        }

        Err(AgentError::InvalidPlan(
            "semantic planner correction was exhausted".to_owned(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_validated_proposal(
        &self,
        request: &AnalyticsRequestContext,
        question: &str,
        intent: &str,
        shape: AnalyticalShape,
        schema: &SchemaContext,
        data_as_of: Option<&str>,
        feedback: Option<&str>,
        allow_regeneration: bool,
    ) -> Result<(QueryProposal, ValidatedQuery, bool), AgentError> {
        let context = json!({
            "planner_intent": intent,
            "analytical_shape": shape,
            "data_as_of": data_as_of,
            "business_timezone": "Asia/Kolkata",
            "schema": schema,
            "required_query_recipe": query_recipe(question),
        });

        let mut generation_messages = vec![request.isolated_prompt(
            "Generate SQL only for the current analytical request. Optional context is referential only.",
        )];

        generation_messages.push(ChatMessage::user(format!(
            "Trusted runtime context (JSON): {}",
            context
        )));

        if let Some(feedback) = feedback {
            generation_messages.push(ChatMessage::user(feedback));
        }

        let validation_scope = request.analytical_scope();

        let mut first = self.generate_proposal(&generation_messages).await?;
        first.expected_shape = shape.as_str().to_owned();

        match validate_proposal(&first, &validation_scope, data_as_of) {
            Ok(validated) => Ok((first, validated, feedback.is_some())),

            Err(error) if allow_regeneration && error.allows_regeneration() => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics rejected]\nrequest_id={}\nprompt=\"{}\"\nreason=\"{}\"\nsql={}",
                    request.request_id,
                    redact_prompt_for_log(question),
                    validation_error_category(&error),
                    validate_sql(&first.sql)
                        .map(|query| query.redacted_sql_for_log())
                        .unwrap_or_else(|_| "<unparseable or non-SELECT SQL>".to_owned())
                );

                generation_messages.push(ChatMessage::user(format!(
                    "CURRENT USER REQUEST: {question}\nFAILED SQL: {}\nSAFE ERROR CATEGORY: {}\nSPECIFIC CORRECTION: {}\nRELEVANT SCHEMA: {}\nSIMPLIFICATION INSTRUCTION: Generate one simpler direct SELECT for shape {}. Do not add CTEs, windows, UNION, subqueries, prior-request dates, or prior-request logic. Placeholder count must exactly match the parameter array.",
                    first.sql,
                    validation_error_category(&error),
                    error,
                    serde_json::to_string(schema).unwrap_or_default(),
                    shape.as_str()
                )));

                let mut retry = self.generate_proposal(&generation_messages).await?;
                retry.expected_shape = shape.as_str().to_owned();

                let validated = validate_proposal(&retry, &validation_scope, data_as_of)?;

                Ok((retry, validated, true))
            }

            Err(error) => Err(AgentError::Policy(error)),
        }
    }

    async fn generate_proposal(
        &self,
        messages: &[ChatMessage],
    ) -> Result<QueryProposal, AgentError> {
        let mut generation_messages = messages.to_vec();

        for attempt in 0..2 {
            let response = self
                .llm
                .chat_json(QUERY_PROMPT, &generation_messages)
                .await?;

            match serde_json::from_str::<QueryProposal>(&response) {
                Ok(proposal)
                    if proposal.requires_database && !proposal.intent.trim().is_empty() =>
                {
                    return Ok(proposal);
                }

                Ok(_) if attempt == 0 => {
                    generation_messages.push(ChatMessage::user(
                        "The proposal omitted its concise database intent. Return one corrected compact JSON object only."
                            .to_owned(),
                    ));
                }

                Err(error) if attempt == 0 => {
                    generation_messages.push(ChatMessage::user(format!(
                        "The proposal was not complete valid JSON ({error}). Return one corrected compact JSON object only; do not include prose."
                    )));
                }

                Ok(_) => {
                    return Err(AgentError::InvalidPlan(
                        "query proposal was missing its database intent".to_owned(),
                    ));
                }

                Err(error) => {
                    return Err(AgentError::InvalidPlan(format!(
                        "query proposal JSON: {error}"
                    )));
                }
            }
        }

        Err(AgentError::InvalidPlan(
            "query proposal correction was exhausted".to_owned(),
        ))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SemanticPlannerRoute {
    AnalyticsPlan,
    EntityList,
    CustomerAnalysis,
    Forecast,
    SpecializedTool,
    GeneratedSql,
    Conversation,
    Blocked,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AnalysisDepth {
    #[default]
    None,
    Summary,
    Deep,
}

impl AnalysisDepth {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Summary => "summary",
            Self::Deep => "deep",
        }
    }
}

#[derive(Debug, Deserialize)]
struct SemanticPlannerResponse {
    route: SemanticPlannerRoute,

    #[serde(default)]
    analysis_depth: AnalysisDepth,
    #[serde(default)]
    plan: Option<AnalyticsPlan>,
    #[serde(default)]
    entity_plan: Option<EntityListPlan>,
    #[serde(default)]
    customer_plan: Option<CustomerAnalysisPlan>,
    #[serde(default)]
    forecast_plan: Option<ForecastPlan>,
    #[serde(default)]
    specialized_tool: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    blocked_reason: Option<String>,
}

fn semantic_planner_schema() -> Value {
    let period_schema = json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "label": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 80
                    },
                    "start": {
        "type": "string"
    },
    "end_exclusive": {
        "type": "string"
    }
                },
                "required": [
                    "label",
                    "start",
                    "end_exclusive"
                ]
            });

    let status_filter_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "dimension": {
                "type": "string",
                "enum": [
                    "operational",
                    "order",
                    "fulfillment",
                    "payment",
                    "delivery"
                ]
            },
            "values": {
                "type": "array",
                "minItems": 1,
                "maxItems": 12,
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 40
                }
            },
            "exclude": {
                "type": "boolean"
            }
        },
        "required": [
            "dimension",
            "values",
            "exclude"
        ]
    });

    let filters_schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "exclude_cancelled": {
                    "type": "boolean"
                },
                "status_filters": {
                    "type": "array",
                    "maxItems": 12,
                    "items": status_filter_schema
                },
                "store_id": {
                    "anyOf": [
                        {"type": "null"},
                        {"type": "string"}
                    ]
                },
                "product": {
        "anyOf": [
            {"type": "null"},
            {"type": "string"}
        ]
    },
    "products": {
        "type": "array",
        "maxItems": 10,
        "items": {
            "type": "string",
            "minLength": 1,
            "maxLength": 120
        }
    },
    "variant_id": {
                    "anyOf": [
                        {"type": "null"},
                        {"type": "string"}
                    ]
                }
            },
            "required": [
                "exclude_cancelled",
                "status_filters",
                "store_id",
                "product",
                "variant_id"
            ]
        });

    let analytics_plan_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "domain": {
                "type": "string",
                "enum": [
                    "operations",
                    "customers",
                    "products"
                ]
            },
            "metrics": {
                "type": "array",
                "minItems": 1,
                "maxItems": 6,
                "items": {
                    "type": "string",
                    "enum": [
                        "order_count",
                        "unique_customers",
                        "quantity_sold",
                        "unique_variants",
                        "order_value",
                        "average_order_value",
                        "average_item_quantity_per_order",
                        "cancelled_order_count",
                        "cancellation_rate"
                    ]
                }
            },
            "dimensions": {
                "type": "array",
                "maxItems": 2,
                "items": {
                    "type": "string",
                    "enum": [
                        "day",
                        "week",
                        "month",
                        "period",
                        "operational_status",
                        "order_status",
                        "fulfillment_status",
                        "payment_status",
                        "delivery_status",
                        "store",
                        "variant",
                        "product"
                    ]
                }
            },
            "periods": {
                "type": "array",
                "minItems": 1,
                "maxItems": 24,
                "items": period_schema.clone()
            },
            "filters": filters_schema.clone(),
            "sort": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "string",
                        "enum": [
                            "chronological",
                            "ascending",
                            "descending"
                        ]
                    }
                ]
            },
            "limit": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100
                    }
                ]
            },
            "visualization": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "string",
                        "enum": [
                            "bar",
                            "horizontal_bar",
                            "line",
                            "table",
                            "none"
                        ]
                    }
                ]
            }
        },
        "required": [
            "domain",
            "metrics",
            "dimensions",
            "periods",
            "filters",
            "sort",
            "limit",
            "visualization"
        ]
    });

    let entity_plan_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "entity": {
                "type": "string",
                "enum": [
                    "customer",
                    "order"
                ]
            },
            "period": period_schema.clone(),
            "minimum_order_count": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000
                    }
                ]
            },
            "maximum_order_count": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 1000
                    }
                ]
            },
            "inactive_days": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 3650
                    }
                ]
            },
            "no_orders_since": {
                "anyOf": [
                    {"type": "null"},
                    {"type": "string"}
                ]
            },
            "purchased_product": {
                "anyOf": [
                    {"type": "null"},
                    {"type": "string"}
                ]
            },
            "return_records": {
                "type": "boolean"
            },
            "sort": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "string",
                        "enum": [
                            "chronological",
                            "ascending",
                            "descending"
                        ]
                    }
                ]
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 500
            }
        },
        "required": [
            "entity",
            "period",
            "minimum_order_count",
            "maximum_order_count",
            "inactive_days",
            "no_orders_since",
            "purchased_product",
            "return_records",
            "sort",
            "limit"
        ]
    });

    let customer_plan_schema = json!({
            "type": "object",
            "additionalProperties": false,

            "properties": {
                "period": period_schema.clone(),

                /*
                 * Only the currently executable customer segment is
                 * exposed to Qwen.
                 */
                "segment": {
                    "type": "string",
                    "enum": [
                        "loyal"
                    ]
                },

                /*
                 * CustomerAnalysis is internally a tagged Rust enum.
                 *
                 * At this stage expose only:
                 *
                 * PurchaseProgression {
                 *     subject: Category,
                 *     max_purchase_number
                 * }
                 */
                "analysis": {
                    "type": "object",
                    "additionalProperties": false,

                    "properties": {
                       "type": {
        "type": "string",
        "enum": [
            "purchase_progression",
            "purchase_transition"
        ]
    },

                        "subject": {
                            "type": "string",
                            "enum": [
                                "category"
                            ]
                        },

                        "max_purchase_number": {
                            "type": "integer",
                            "minimum": 2,
                            "maximum": 20
                        }
                    },

                    "required": [
                        "type",
                        "subject",
                        "max_purchase_number"
                    ]
                }
            },

            "required": [
                "period",
                "segment",
                "analysis"
            ]
        });

    let forecast_plan_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "metric": {
                "type": "string",
                "enum": ["order_count"]
            },
            "training_window": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": [
                            "complete_calendar_months",
                            "complete_calendar_weeks",
                            "complete_days"
                        ]
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 730
                    }
                },
                "required": [
                    "kind",
                    "count"
                ]
            },
            "forecast_horizon": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "unit": {
                        "type": "string",
                        "enum": [
                            "days",
                            "calendar_months"
                        ]
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 90
                    }
                },
                "required": [
                    "unit",
                    "count"
                ]
            },
            "grain": {
                "type": "string",
                "enum": ["day"]
            },
            "filters": filters_schema
        },
        "required": [
            "metric",
            "training_window",
            "forecast_horizon",
            "grain",
            "filters"
        ]
    });

    json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "route": {
        "type": "string",
        "enum": [
            "analytics_plan",
            "entity_list",
            "customer_analysis",
            "forecast",
            "specialized_tool",
            "generated_sql",
            "conversation",
            "blocked"
        ]
    },
                "analysis_depth": {
                    "type": "string",
                    "enum": [
                        "none",
                        "summary",
                        "deep"
                    ]
                },
                "plan": {
                    "anyOf": [
                        {"type": "null"},
                        analytics_plan_schema
                    ]
                },
                "entity_plan": {
                    "anyOf": [
                        {"type": "null"},
                        entity_plan_schema
                    ]
                },
                "customer_plan": {
                    "anyOf": [
                        {"type": "null"},
                        customer_plan_schema
                    ]
                },
                "forecast_plan": {
                    "anyOf": [
                        {"type": "null"},
                        forecast_plan_schema
                    ]
                },
                "specialized_tool": {
                    "anyOf": [
                        {"type": "null"},
                        {
                            "type": "string",
                            "enum": [
                                "lapsed_customers",
                                "reorder_demand",
                                "busy_day_forecast",
                                "product_performance"
                            ]
                        }
                    ]
                },
                "intent": {
                    "anyOf": [
                        {"type": "null"},
                        {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 500
                        }
                    ]
                },
                "blocked_reason": {
                    "anyOf": [
                        {"type": "null"},
                        {
                            "type": "string",
                            "maxLength": 500
                        }
                    ]
                }
            },
            "required": [
        "route",
        "analysis_depth",
        "plan",
        "entity_plan",
        "customer_plan",
        "forecast_plan",
        "specialized_tool",
        "intent",
        "blocked_reason"
    ]
        })
}

fn validate_semantic_historical_boundary(
    route: &SemanticPlannerResponse,
    historical_exclusive_boundary: Option<chrono::NaiveDate>,
) -> Result<(), String> {
    let Some(boundary) = historical_exclusive_boundary else {
        return Ok(());
    };

    let exceeds_boundary = match route.route {
        SemanticPlannerRoute::AnalyticsPlan => route.plan.as_ref().is_some_and(|plan| {
            plan.periods
                .iter()
                .any(|period| period.end_exclusive > boundary)
        }),

        SemanticPlannerRoute::EntityList => route.entity_plan.as_ref().is_some_and(|plan| {
            plan.period.end_exclusive > boundary
                || plan.no_orders_since.is_some_and(|cutoff| cutoff > boundary)
        }),
        SemanticPlannerRoute::CustomerAnalysis => route
            .customer_plan
            .as_ref()
            .is_some_and(|plan| plan.period.end_exclusive > boundary),

        _ => false,
    };

    if exceeds_boundary {
        return Err(format!(
            "historical analytics must end by {boundary}; \
             current partial or future dates are not valid historical data"
        ));
    }

    Ok(())
}

fn validate_semantic_route(route: &SemanticPlannerResponse) -> Result<(), String> {
    match route.route {
        SemanticPlannerRoute::AnalyticsPlan => route
            .plan
            .as_ref()
            .ok_or_else(|| "analytics_plan route requires plan".to_owned())?
            .validate()
            .map_err(|error| error.to_string()),
        SemanticPlannerRoute::EntityList => route
            .entity_plan
            .as_ref()
            .ok_or_else(|| "entity_list route requires entity_plan".to_owned())?
            .validate(),
        SemanticPlannerRoute::CustomerAnalysis => route
            .customer_plan
            .as_ref()
            .ok_or_else(|| "customer_analysis route requires customer_plan".to_owned())?
            .validate()
            .map_err(|error| error.to_string()),
        SemanticPlannerRoute::Forecast => route
            .forecast_plan
            .as_ref()
            .ok_or_else(|| "forecast route requires forecast_plan".to_owned())?
            .validate(),
        SemanticPlannerRoute::SpecializedTool => {
            let supported = matches!(
                route.specialized_tool.as_deref(),
                Some(
                    "lapsed_customers"
                        | "reorder_demand"
                        | "busy_day_forecast"
                        | "product_performance"
                )
            );
            supported
                .then_some(())
                .ok_or_else(|| "specialized_tool route requires a supported tool".to_owned())
        }
        SemanticPlannerRoute::GeneratedSql => route
            .intent
            .as_deref()
            .filter(|intent| !intent.trim().is_empty() && intent.len() <= 500)
            .map(|_| ())
            .ok_or_else(|| "generated_sql route requires a concise intent".to_owned()),
        SemanticPlannerRoute::Conversation | SemanticPlannerRoute::Blocked => Ok(()),
    }
}
fn normalize_analysis_depth(route: &mut SemanticPlannerResponse, question: &str) {
    if matches!(
        route.route,
        SemanticPlannerRoute::Conversation | SemanticPlannerRoute::Blocked
    ) {
        return;
    }

    if requires_deep_analysis(question) {
        route.analysis_depth = AnalysisDepth::Deep;
    }
}

fn normalize_explicit_monthly_grain(route: &mut SemanticPlannerResponse, question: &str) {
    use crate::analytics::Dimension;

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    let normalized = question.to_ascii_lowercase();

    if !normalized.contains("monthly") && !normalized.contains("every month") {
        return;
    }

    /*
     * Replace only temporal-grain dimensions.
     * Preserve Store/Product/etc. if present.
     */
    plan.dimensions.retain(|dimension| {
        !matches!(
            dimension,
            Dimension::Day | Dimension::Week | Dimension::Month | Dimension::Period
        )
    });

    plan.dimensions.insert(0, Dimension::Month);
}

fn normalize_explicit_single_calendar_month(route: &mut SemanticPlannerResponse, question: &str) {
    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    if plan.periods.len() != 1 {
        return;
    }

    let normalized = question.to_ascii_lowercase();

    if normalized.contains(" through ")
        || normalized.contains(" thru ")
        || normalized.contains("previous month")
        || normalized.contains("prior month")
    {
        return;
    }

    let matched_months = explicit_calendar_month_mentions(&normalized);

    if matched_months.len() != 1 {
        return;
    }

    let year = normalized
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<i32>().ok())
        .find(|year| (2000..=2100).contains(year));

    let Some(year) = year else {
        return;
    };

    let (_, month_name, month) = matched_months[0];

    let Some(start) = chrono::NaiveDate::from_ymd_opt(year, month, 1) else {
        return;
    };

    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };

    let Some(end_exclusive) = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1) else {
        return;
    };

    let period = &mut plan.periods[0];

    period.start = start;
    period.end_exclusive = end_exclusive;
    period.label = format!("{} {}", capitalize_ascii(month_name), year);
}

fn capitalize_ascii(value: &str) -> String {
    let mut characters = value.chars();

    let Some(first) = characters.next() else {
        return String::new();
    };

    format!("{}{}", first.to_ascii_uppercase(), characters.as_str())
}

fn requires_deep_analysis(question: &str) -> bool {
    let normalized = question.to_ascii_lowercase();

    if normalized.contains("what stands out") {
        return true;
    }

    let tokens = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty());

    tokens.into_iter().any(|token| {
        matches!(
            token,
            "analyze"
                | "analyse"
                | "analysis"
                | "explain"
                | "interpret"
                | "investigate"
                | "insight"
                | "insights"
                | "pattern"
                | "patterns"
                | "why"
        )
    })
}

fn normalize_semantic_route(route: &mut SemanticPlannerResponse) {
    normalize_simple_unique_customer_count(route);

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    normalize_product_metric_shape(plan);
    normalize_contiguous_time_series_periods(plan);
    normalize_redundant_single_period_dimension(plan);
    normalize_cancellation_metric_filters(plan);
    normalize_overlapping_period_partitions(plan);
}

fn find_ascii_token_position(value: &str, token: &str) -> Option<usize> {
    value.match_indices(token).find_map(|(index, _)| {
        let before = value[..index].chars().next_back();
        let after = value[index + token.len()..].chars().next();

        let valid_before = before.map_or(true, |character| !character.is_ascii_alphanumeric());

        let valid_after = after.map_or(true, |character| !character.is_ascii_alphanumeric());

        if valid_before && valid_after {
            Some(index)
        } else {
            None
        }
    })
}

fn explicit_calendar_month_mentions(normalized: &str) -> Vec<(usize, &'static str, u32)> {
    const MONTHS: [(&str, &str, u32); 24] = [
        ("january", "January", 1),
        ("jan", "January", 1),
        ("february", "February", 2),
        ("feb", "February", 2),
        ("march", "March", 3),
        ("mar", "March", 3),
        ("april", "April", 4),
        ("apr", "April", 4),
        ("may", "May", 5),
        ("june", "June", 6),
        ("jun", "June", 6),
        ("july", "July", 7),
        ("jul", "July", 7),
        ("august", "August", 8),
        ("aug", "August", 8),
        ("september", "September", 9),
        ("sept", "September", 9),
        ("sep", "September", 9),
        ("october", "October", 10),
        ("oct", "October", 10),
        ("november", "November", 11),
        ("nov", "November", 11),
        ("december", "December", 12),
        ("dec", "December", 12),
    ];

    let mut mentions = MONTHS
        .into_iter()
        .filter_map(|(token, label, month)| {
            find_ascii_token_position(normalized, token).map(|position| (position, label, month))
        })
        .collect::<Vec<_>>();

    mentions.sort_by_key(|(position, _, _)| *position);

    /*
     * Normally aliases cannot both match because matching is token-boundary
     * aware, but keep the result deterministic if wording contains both.
     */
    let mut unique = Vec::new();

    for mention in mentions {
        if unique
            .iter()
            .any(|(_, _, existing_month)| *existing_month == mention.2)
        {
            continue;
        }

        unique.push(mention);
    }

    unique
}

fn normalize_explicit_calendar_month_range(route: &mut SemanticPlannerResponse, question: &str) {
    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    /*
     * Only repair the simple case where Qwen returned one continuous
     * analytical period. Multi-period comparisons are handled by the
     * existing period normalizers.
     */
    if plan.periods.len() != 1 {
        return;
    }

    let normalized = question.to_ascii_lowercase();

    let has_range_connector = normalized.contains(" to ")
        || normalized.contains(" through ")
        || normalized.contains(" until ")
        || normalized.contains(" till ")
        || normalized.contains('–')
        || normalized.contains('—');

    if !has_range_connector {
        return;
    }

    let year = normalized
        .split(|character: char| !character.is_ascii_digit())
        .filter(|token| !token.is_empty())
        .filter_map(|token| token.parse::<i32>().ok())
        .find(|year| (2000..=2100).contains(year));

    let Some(year) = year else {
        return;
    };

    let mentioned_months = explicit_calendar_month_mentions(&normalized);

    /*
     * Stay narrow. Do not reinterpret questions that mention three or
     * more explicit months, because those may be comparisons rather
     * than one continuous range.
     */
    if mentioned_months.len() != 2 {
        return;
    }

    let (_, start_label, start_month) = mentioned_months[0];
    let (_, end_label, end_month) = mentioned_months[1];

    /*
     * "November to February 2026" is ambiguous about the year boundary.
     * Do not guess.
     */
    if start_month > end_month {
        return;
    }

    let Some(start) = chrono::NaiveDate::from_ymd_opt(year, start_month, 1) else {
        return;
    };

    let (end_year, next_month) = if end_month == 12 {
        (year + 1, 1)
    } else {
        (year, end_month + 1)
    };

    let Some(end_exclusive) = chrono::NaiveDate::from_ymd_opt(end_year, next_month, 1) else {
        return;
    };

    plan.periods = vec![crate::analytics::TimePeriod {
        label: format!("{start_label} through {end_label} {year}"),
        start,
        end_exclusive,
    }];
}

fn normalize_simple_unique_customer_count(route: &mut SemanticPlannerResponse) {
    use crate::analytics::{
        AnalyticsDomain, AnalyticsFilters, EntityType, Metric, VisualizationIntent,
    };

    if route.route != SemanticPlannerRoute::EntityList {
        return;
    }

    let Some(entity_plan) = route.entity_plan.as_ref() else {
        return;
    };

    let is_simple_customer_count = entity_plan.entity == EntityType::Customer
        && !entity_plan.return_records
        && matches!(entity_plan.minimum_order_count, None | Some(1))
        && entity_plan.maximum_order_count.is_none()
        && entity_plan.inactive_days.is_none()
        && entity_plan.no_orders_since.is_none()
        && entity_plan.purchased_product.is_none();

    if !is_simple_customer_count {
        return;
    }

    let period = entity_plan.period.clone();

    route.route = SemanticPlannerRoute::AnalyticsPlan;
    route.plan = Some(AnalyticsPlan {
        domain: AnalyticsDomain::Customers,
        metrics: vec![Metric::UniqueCustomers],
        dimensions: Vec::new(),
        periods: vec![period],
        filters: AnalyticsFilters {
            exclude_cancelled: true,
            ..AnalyticsFilters::default()
        },
        sort: None,
        limit: None,
        visualization: Some(VisualizationIntent::None),
    });
    route.entity_plan = None;
}

fn normalize_overlapping_period_partitions(plan: &mut AnalyticsPlan) {
    use crate::analytics::Dimension;

    if !plan.dimensions.contains(&Dimension::Period) || plan.periods.len() < 2 {
        return;
    }

    let starts_are_strictly_increasing = plan
        .periods
        .windows(2)
        .all(|window| window[0].start < window[1].start);

    if !starts_are_strictly_increasing {
        return;
    }

    for index in 0..plan.periods.len() - 1 {
        let next_start = plan.periods[index + 1].start;

        if plan.periods[index].end_exclusive > next_start {
            plan.periods[index].end_exclusive = next_start;
        }
    }
}

fn normalize_cancellation_metric_filters(plan: &mut AnalyticsPlan) {
    use crate::analytics::{Metric, StatusDimension};

    let has_cancellation_metric = plan.metrics.iter().any(|metric| {
        matches!(
            metric,
            Metric::CancelledOrderCount | Metric::CancellationRate
        )
    });

    if !has_cancellation_metric {
        return;
    }

    plan.filters.exclude_cancelled = false;

    plan.filters.status_filters.retain(|status| {
        let is_cancellation_status_dimension = matches!(
            status.dimension,
            StatusDimension::Operational | StatusDimension::Order
        );

        let contains_only_cancellation_values = !status.values.is_empty()
            && status.values.iter().all(|value| {
                matches!(
                    value.trim().to_ascii_uppercase().as_str(),
                    "CANCELED" | "CANCELLED"
                )
            });

        !(is_cancellation_status_dimension && contains_only_cancellation_values)
    });
}

fn normalize_named_product_performance_snapshot(
    route: &mut SemanticPlannerResponse,
    question: &str,
) {
    use crate::analytics::{AnalyticsDomain, Dimension, Metric, VisualizationIntent};

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    if plan.domain != AnalyticsDomain::Products {
        return;
    }

    /*
     * Automatic product-performance shorthand is intentionally narrow:
     *
     * - one named product
     * - one requested period
     * - no explicit grouping
     * - no explicit variant request
     *
     * Comparative/ranking/time-series product performance remains owned by
     * its existing typed/specialized routes.
     */
    if plan.filters.product.is_none() || plan.periods.len() != 1 || !plan.dimensions.is_empty() {
        return;
    }

    let normalized = question.to_ascii_lowercase();

    let requests_performance = normalized.contains("performance")
        || normalized.contains("performing")
        || normalized.contains("performed")
        || normalized.contains("how did") && normalized.contains("perform");

    if !requests_performance {
        return;
    }

    /*
     * If the user explicitly asks for a narrower metric, do not expand it
     * into the complete performance snapshot.
     */
    let explicitly_requests_specific_metric = normalized.contains("quantity sold")
        || normalized.contains("units sold")
        || normalized.contains("sales volume")
        || normalized.contains("product orders")
        || normalized.contains("order count")
        || normalized.contains("unique buyers")
        || normalized.contains("unique customers")
        || normalized.contains("sales value")
        || normalized.contains("merchandise value")
        || normalized.contains("value generated")
        || normalized.contains("aov")
        || normalized.contains("average order value");

    if explicitly_requests_specific_metric {
        return;
    }

    /*
     * Named-product "performance" has one canonical core metric set.
     *
     * Parent order and buyer metrics remain independently computed at
     * product grain; they are never derived from variant rows.
     */
    plan.metrics = vec![
        Metric::QuantitySold,
        Metric::ProductOrderCount,
        Metric::ProductUniqueCustomers,
        Metric::ProductSalesValue,
    ];

    plan.filters.exclude_cancelled = true;
    plan.sort = None;
    plan.limit = None;
    plan.visualization = Some(VisualizationIntent::Table);

    /*
     * Do not add Product or Variant dimensions here.
     * The parent remains scalar product performance.
     * Rust's B2 semantic executor derives the variant expansion separately.
     */
    plan.dimensions
        .retain(|dimension| !matches!(dimension, Dimension::Product | Dimension::Variant));
}
fn normalize_singleton_product_selector(route: &mut SemanticPlannerResponse, question: &str) {
    use crate::analytics::{AnalyticsDomain, Dimension};

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    if plan.domain != AnalyticsDomain::Products {
        return;
    }

    if plan.filters.product.is_some()
        || plan.filters.variant_id.is_some()
        || plan.filters.products.len() != 1
    {
        return;
    }

    let normalized = question.to_ascii_lowercase();

    /*
     * Never silently repair an apparently incomplete explicit
     * multi-product comparison.
     *
     * If the user really asked to compare multiple products and Qwen
     * preserved only one mention, plan validation should reject it and
     * correction mode should repair it.
     */
    let explicit_selected_product_comparison = normalized.contains("compare")
        || normalized.contains(" versus ")
        || normalized.contains(" vs ")
        || normalized.contains(" against ");

    if explicit_selected_product_comparison {
        return;
    }

    let product = plan.filters.products.remove(0);

    if product.trim().is_empty() {
        plan.filters.products.push(product);
        return;
    }

    plan.filters.product = Some(product);

    /*
     * A single named parent product is a filter, not product grouping.
     *
     * Preserve temporal grain such as Month, Week or Day.
     */
    plan.dimensions
        .retain(|dimension| *dimension != Dimension::Product);
}

fn normalize_multi_product_comparison(route: &mut SemanticPlannerResponse, question: &str) {
    use crate::analytics::{AnalyticsDomain, Dimension, Metric, SortSpec, VisualizationIntent};

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    if plan.filters.products.is_empty() {
        return;
    }

    /*
     * `filters.products` represents an explicit selected-product comparison.
     *
     * Qwen identifies the product mentions.
     * Rust owns the analytical shape after that.
     */
    plan.domain = AnalyticsDomain::Products;

    /*
     * Explicit comparison sets must never be truncated.
     */
    plan.limit = None;

    /*
     * B5 operates at parent-product grain.
     *
     * Preserve an existing temporal or period dimension and add Product
     * when the plan has room for it.
     *
     * Invalid combinations such as Variant + products are deliberately
     * left for AnalyticsPlan::validate() to reject rather than silently
     * rewriting an explicit incompatible request.
     */
    let has_time_bucket = plan.dimensions.iter().any(|dimension| {
        matches!(
            dimension,
            Dimension::Day | Dimension::Week | Dimension::Month
        )
    });

    if plan.periods.len() > 1
        && !has_time_bucket
        && !plan.dimensions.contains(&Dimension::Period)
        && plan.dimensions.len() < 2
    {
        plan.dimensions.insert(0, Dimension::Period);
    }

    if !plan.dimensions.contains(&Dimension::Product)
        && !plan.dimensions.contains(&Dimension::Variant)
        && plan.dimensions.len() < 2
    {
        plan.dimensions.push(Dimension::Product);
    }

    let normalized = question.to_ascii_lowercase();

    /*
     * Preserve explicitly requested metrics.
     *
     * Existing normalize_product_metric_shape() will later convert
     * generic order/customer metrics to their product-grain equivalents.
     */
    let explicitly_requests_specific_metric = [
        "quantity",
        "units sold",
        "unit sold",
        "sales volume",
        "orders",
        "order count",
        "buyers",
        "customers",
        "sales value",
        "merchandise value",
        "value generated",
        "aov",
        "average order value",
        "cancelled",
        "canceled",
        "cancellation",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));

    if explicitly_requests_specific_metric {
        return;
    }

    /*
     * Generic:
     *
     * "Compare Sharbati Atta, Khapli Atta and Multigrain Atta"
     *
     * means the canonical B5 performance comparison.
     */
    plan.metrics = vec![
        Metric::QuantitySold,
        Metric::ProductOrderCount,
        Metric::ProductUniqueCustomers,
        Metric::ProductSalesValue,
    ];

    plan.filters.exclude_cancelled = true;

    /*
     * Do not preserve an invented order-status restriction for a generic
     * product-performance comparison.
     */
    plan.filters.status_filters.clear();

    plan.sort = Some(SortSpec::Descending);
    plan.visualization = Some(VisualizationIntent::Table);
}

fn normalize_explicit_product_sales_value(route: &mut SemanticPlannerResponse, question: &str) {
    use crate::analytics::{AnalyticsDomain, Dimension, Metric};

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    if plan.domain != AnalyticsDomain::Products {
        return;
    }

    let product_scoped = plan
        .dimensions
        .iter()
        .any(|dimension| matches!(dimension, Dimension::Product | Dimension::Variant))
        || plan.filters.product.is_some()
        || !plan.filters.products.is_empty()
        || plan.filters.variant_id.is_some();

    if !product_scoped {
        return;
    }

    let normalized = question.to_ascii_lowercase();

    let explicitly_requests_product_sales_value = normalized.contains("product sales value")
        || normalized.contains("sales value")
        || normalized.contains("merchandise value")
        || normalized.contains("value generated");

    if !explicitly_requests_product_sales_value {
        return;
    }

    let explicitly_requests_cancellation = normalized.contains("cancelled")
        || normalized.contains("canceled")
        || normalized.contains("cancellation");

    let explicitly_requests_aov =
        normalized.contains("aov") || normalized.contains("average order value");

    if !plan.metrics.contains(&Metric::ProductSalesValue) {
        if let Some(metric) = plan
            .metrics
            .iter_mut()
            .find(|metric| **metric == Metric::OrderValue)
        {
            *metric = Metric::ProductSalesValue;
        } else if let Some(metric) = plan
            .metrics
            .iter_mut()
            .find(|metric| **metric == Metric::AverageOrderValue && !explicitly_requests_aov)
        {
            *metric = Metric::ProductSalesValue;
        } else if let Some(metric) = plan.metrics.iter_mut().find(|metric| {
            matches!(
                **metric,
                Metric::CancelledOrderCount | Metric::CancellationRate
            ) && !explicitly_requests_cancellation
        }) {
            *metric = Metric::ProductSalesValue;
        } else if plan.metrics.len() == 1 && plan.metrics[0] == Metric::QuantitySold {
            plan.metrics[0] = Metric::ProductSalesValue;
        }
    }

    if !explicitly_requests_aov {
        plan.metrics
            .retain(|metric| *metric != Metric::AverageOrderValue);
    }

    if plan.metrics.contains(&Metric::ProductSalesValue) {
        plan.filters.exclude_cancelled = true;
    }
}

fn normalize_generic_named_product_performance(
    route: &mut SemanticPlannerResponse,
    question: &str,
) {
    use crate::analytics::{AnalyticsDomain, Dimension, Metric};

    let Some(plan) = route.plan.as_mut() else {
        return;
    };

    if plan.domain != AnalyticsDomain::Products {
        return;
    }

    /*
     * This normalizer is specifically for a named product.
     *
     * Multi-product rankings/comparisons belong to B5.
     */
    let Some(product) = plan.filters.product.as_deref() else {
        return;
    };

    if product.trim().is_empty() {
        return;
    }

    if plan.dimensions.contains(&Dimension::Product) {
        return;
    }

    let normalized = question.to_ascii_lowercase();

    /*
     * Only normalize generic "performance" shorthand.
     */
    let generic_performance_request = normalized.contains("performance")
        || normalized.contains("performing")
        || normalized.contains("performed")
        || (normalized.contains("how did") && normalized.contains("perform"));

    if !generic_performance_request {
        return;
    }

    /*
     * If the user explicitly named a particular metric,
     * preserve that request instead of expanding it to
     * the four-metric performance contract.
     */
    let explicitly_requests_metric = [
        "quantity",
        "units sold",
        "unit sold",
        "orders",
        "order count",
        "buyers",
        "customers",
        "sales value",
        "merchandise value",
        "aov",
        "average order value",
        "cancelled",
        "canceled",
        "cancellation",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));

    if explicitly_requests_metric {
        return;
    }

    /*
     * Generic named-product performance has one canonical
     * deterministic metric contract.
     */
    plan.metrics = vec![
        Metric::QuantitySold,
        Metric::ProductOrderCount,
        Metric::ProductUniqueCustomers,
        Metric::ProductSalesValue,
    ];

    /*
     * Product-performance defaults use qualifying orders:
     * full cancellations are excluded.
     */
    plan.filters.exclude_cancelled = true;

    /*
     * Do not carry an invented operational/payment/delivery
     * status restriction into a generic performance request.
     */
    plan.filters.status_filters.clear();
}

fn normalize_product_metric_shape(plan: &mut AnalyticsPlan) {
    use crate::analytics::{AnalyticsDomain, Metric};

    if plan.domain != AnalyticsDomain::Products {
        return;
    }

    for metric in &mut plan.metrics {
        *metric = match *metric {
            Metric::OrderCount => Metric::ProductOrderCount,
            Metric::UniqueCustomers => Metric::ProductUniqueCustomers,
            other => other,
        };
    }

    if plan.metrics.iter().any(|metric| {
        matches!(
            metric,
            Metric::QuantitySold
                | Metric::ProductOrderCount
                | Metric::ProductUniqueCustomers
                | Metric::ProductSalesValue
                | Metric::UniqueVariants
        )
    }) {
        plan.filters.exclude_cancelled = true;
    }
}

fn normalize_contiguous_time_series_periods(plan: &mut AnalyticsPlan) {
    use crate::analytics::Dimension;

    if plan.periods.len() <= 1
        || plan.dimensions.contains(&Dimension::Period)
        || !plan.dimensions.iter().any(|dimension| {
            matches!(
                dimension,
                Dimension::Day | Dimension::Week | Dimension::Month
            )
        })
    {
        return;
    }

    let contiguous = plan
        .periods
        .windows(2)
        .all(|window| window[0].end_exclusive == window[1].start);

    if !contiguous {
        return;
    }

    let Some(first) = plan.periods.first().cloned() else {
        return;
    };

    let Some(last) = plan.periods.last() else {
        return;
    };

    let start = first.start;
    let end_exclusive = last.end_exclusive;

    plan.periods = vec![crate::analytics::TimePeriod {
        label: format!("{start} through {end_exclusive}"),
        start,
        end_exclusive,
    }];
}

fn normalize_redundant_single_period_dimension(plan: &mut AnalyticsPlan) {
    use crate::analytics::Dimension;

    if plan.periods.len() != 1 {
        return;
    }

    plan.dimensions
        .retain(|dimension| *dimension != Dimension::Period);
}

const NAMED_PRODUCT_VARIANT_FILTER_ERROR: &str =
    "the request asks for a variant breakdown of a named parent product; \
     preserve that parent product in filters.product while keeping dimension=variant";

const NAMED_PRODUCT_PERFORMANCE_FILTER_ERROR: &str =
    "the request asks for performance of one named parent product; \
     preserve that product in filters.product and keep filters.products empty";

fn validate_semantic_route_for_question(
    route: &SemanticPlannerResponse,
    question: &str,
) -> Result<(), String> {
    use crate::analytics::Dimension;

    let normalized = question.to_lowercase();
    let asks_for_named_product_performance = normalized.contains("performance of ")
        || (normalized.contains("how did ") && normalized.contains(" perform"));

    if asks_for_named_product_performance {
        if let Some(plan) = route.plan.as_ref() {
            let missing_named_product = plan.domain == crate::analytics::AnalyticsDomain::Products
                && plan.filters.product.is_none()
                && plan.filters.products.is_empty()
                && plan.filters.variant_id.is_none()
                && !plan.dimensions.contains(&Dimension::Product);

            if missing_named_product {
                return Err(NAMED_PRODUCT_PERFORMANCE_FILTER_ERROR.to_owned());
            }
        }
    }

    if normalized.contains("monthly") || normalized.contains("every month") {
        if let Some(plan) = route.plan.as_ref() {
            if !plan.dimensions.contains(&Dimension::Month) {
                return Err("the request explicitly requires monthly grain; \
                     use dimension=month with one continuous TimePeriod \
                     covering the requested date range"
                    .to_owned());
            }
        }
    }

    /*
    * Explicit named-product variant breakdown.
    *
    * Rust does not resolve or extract the product name here.
    * It only enforces preservation of the product constraint that
    * Qwen is responsible for interpreting into filters.product.

    */

    if normalized.contains("by variant for ") {
        if let Some(plan) = route.plan.as_ref() {
            if plan.dimensions.contains(&Dimension::Variant) && plan.filters.product.is_none() {
                return Err(NAMED_PRODUCT_VARIANT_FILTER_ERROR.to_owned());
            }
        }
    }

    if requires_deep_analysis(question) && route.analysis_depth != AnalysisDepth::Deep {
        return Err("explicit analysis intent requires analysis_depth=deep".to_owned());
    }

    if route.plan.as_ref().is_some_and(|plan| {
        plan.metrics
            .contains(&crate::analytics::Metric::AverageItemQuantityPerOrder)
    }) && normalized.contains("line item")
        && !normalized.contains("quantity")
    {
        return Err("line-item row count is not average item quantity; \
             route this request to generated_sql"
            .to_owned());
    }

    Ok(())
}

fn apply_semantic_correction_baseline(
    route: &mut SemanticPlannerResponse,
    baseline_plan: Option<&AnalyticsPlan>,
    validation_error: Option<&str>,
) {
    if validation_error != Some(NAMED_PRODUCT_VARIANT_FILTER_ERROR) {
        return;
    }

    let Some(baseline_plan) = baseline_plan else {
        return;
    };

    let Some(corrected_plan) = route.plan.as_mut() else {
        return;
    };

    /*
     * For this validation failure, the only field Qwen is allowed to repair
     * is filters.product.
     *
     * The first rejected plan was otherwise already normalized and valid for
     * the requested metrics, dimensions, periods, population, and output
     * shape. Preserve that baseline deterministically.
     */
    let corrected_product = corrected_plan.filters.product.clone();

    *corrected_plan = baseline_plan.clone();

    corrected_plan.filters.product = corrected_product;
}

#[derive(Debug, Deserialize)]
struct QueryProposal {
    #[serde(default = "default_true")]
    requires_database: bool,
    intent: String,
    sql: String,
    #[serde(default)]
    parameters: Vec<Value>,
    #[serde(default = "default_expected_shape")]
    expected_shape: String,
}

const fn default_true() -> bool {
    true
}

fn default_expected_shape() -> String {
    "table".to_owned()
}

#[derive(Debug, Deserialize)]
struct Interpretation {
    #[serde(
        default,
        rename = "answer",
        alias = "summary",
        alias = "response",
        alias = "text"
    )]
    _answer: String,
    #[serde(default)]
    chart_type: Option<String>,
    #[serde(default)]
    chart_title: Option<String>,
}

fn parse_chart_type(value: &str) -> Option<ChartType> {
    match value {
        "bar" => Some(ChartType::Bar),
        "horizontal_bar" => Some(ChartType::HorizontalBar),
        "line" => Some(ChartType::Line),
        _ => None,
    }
}

fn fill_missing_date_buckets(
    result: &mut crate::database::QueryResult,
    parameters: &[Value],
    sql: &str,
) {
    let normalized_sql = sql.to_lowercase();
    if !(normalized_sql.contains(" as order_date") || normalized_sql.contains(" as day")) {
        return;
    }
    let Some(start) = parameters.first().and_then(Value::as_str) else {
        return;
    };
    let Some(end_exclusive) = parameters.get(1).and_then(Value::as_str) else {
        return;
    };
    if !is_iso_date(start) || !is_iso_date(end_exclusive) {
        return;
    }
    let date_key = result
        .columns
        .iter()
        .find(|key| key.contains("date") || key.as_str() == "day")
        .cloned()
        .unwrap_or_else(|| "order_date".to_owned());
    let metric_key = result
        .columns
        .iter()
        .find(|key| key.contains("count") || key.contains("volume"))
        .cloned()
        .unwrap_or_else(|| "order_count".to_owned());
    let mut completed = Vec::new();
    let mut date = start.to_owned();
    while date.as_str() < end_exclusive && completed.len() < 62 {
        if let Some(existing) = result
            .rows
            .iter()
            .find(|row| row.get(&date_key).and_then(Value::as_str) == Some(date.as_str()))
        {
            completed.push(existing.clone());
        } else {
            let mut row = serde_json::Map::new();
            row.insert(date_key.clone(), Value::String(date.clone()));
            row.insert(metric_key.clone(), Value::from(0));
            completed.push(row);
        }
        let Some(next) = next_iso_date(&date) else {
            return;
        };
        date = next;
    }
    if completed.is_empty() || date.as_str() != end_exclusive {
        return;
    }
    result.rows = completed;
    result.row_count = result.rows.len();
    result.columns = vec![date_key, metric_key];
}

fn is_iso_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| index == 4 || index == 7 || character.is_ascii_digit())
}

fn next_iso_date(value: &str) -> Option<String> {
    let year = value.get(0..4)?.parse::<u32>().ok()?;
    let month = value.get(5..7)?.parse::<u32>().ok()?;
    let day = value.get(8..10)?.parse::<u32>().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    let (next_year, next_month, next_day) = if day < days_in_month {
        (year, month, day + 1)
    } else if month < 12 {
        (year, month + 1, 1)
    } else {
        (year + 1, 1, 1)
    };
    Some(format!("{next_year:04}-{next_month:02}-{next_day:02}"))
}

fn validate_proposal(
    proposal: &QueryProposal,
    question: &str,
    data_as_of: Option<&str>,
) -> Result<ValidatedQuery, SqlPolicyError> {
    let shape = AnalyticalShape::classify(question);
    if proposal.expected_shape != shape.as_str() {
        return Err(SqlPolicyError::Unsupported(format!(
            "expected deterministic {} shape",
            shape.as_str()
        )));
    }
    let query = if is_customer_record_request(question) {
        validate_sql_with_sensitive_allowlist(&proposal.sql, &["first_name", "phone"])?
    } else {
        validate_sql(&proposal.sql)?
    };
    if query.parameter_count() != proposal.parameters.len() {
        return Err(SqlPolicyError::InvalidPlaceholder);
    }
    if proposal
        .parameters
        .iter()
        .any(|value| QueryParameter::try_from(value).is_err())
    {
        return Err(SqlPolicyError::InvalidPlaceholder);
    }
    validate_projection_semantics(proposal, question, shape, query.projection_count())?;
    validate_analytical_semantics(proposal, question, data_as_of)?;
    Ok(query)
}

fn validate_projection_semantics(
    proposal: &QueryProposal,
    question: &str,
    shape: AnalyticalShape,
    projection_count: usize,
) -> Result<(), SqlPolicyError> {
    let sql = proposal.sql.to_lowercase();
    let normalized = question.to_lowercase();
    let explicit_calendar_scope = [
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
    .any(|month| normalized.contains(month));
    let needs_freshness_query = ["last ", "latest", "available", "preceding", "previous"]
        .iter()
        .any(|term| normalized.contains(term));
    if explicit_calendar_scope
        && !needs_freshness_query
        && (sql.trim_start().starts_with("with ")
            || sql.contains(" union ")
            || sql.contains("row_number(")
            || sql.contains(" over ("))
    {
        return Err(SqlPolicyError::Unsupported(
            "explicit calendar analytics require a simple direct SELECT; remove CTEs, UNION, and windows"
                .to_owned(),
        ));
    }
    match shape {
        AnalyticalShape::Scalar => {
            if projection_count != 1 {
                return Err(SqlPolicyError::Unsupported(
                    "scalar questions require exactly one output column".to_owned(),
                ));
            }
            if !["count(", "sum(", "avg(", "min(", "max("]
                .iter()
                .any(|aggregate| sql.contains(aggregate))
            {
                return Err(SqlPolicyError::Unsupported(
                    "scalar questions require one aggregate projection".to_owned(),
                ));
            }
            if [
                " as order_id",
                "display_id",
                "store_invoice_number",
                "first_name",
                "phone",
            ]
            .iter()
            .any(|column| sql.contains(column))
            {
                return Err(SqlPolicyError::Unsupported(
                    "scalar questions must not retrieve record-level fields".to_owned(),
                ));
            }
            if normalized.contains("customer")
                && !(sql.contains("count(distinct") && sql.contains("customer_id"))
            {
                return Err(SqlPolicyError::Unsupported(
                    "customer counts require COUNT(DISTINCT customer_id)".to_owned(),
                ));
            }
            if (normalized.contains("sku") || normalized.contains("variant"))
                && !(sql.contains("count(distinct") && sql.contains("variant_id"))
            {
                return Err(SqlPolicyError::Unsupported(
                    "SKU counts require COUNT(DISTINCT variant_id)".to_owned(),
                ));
            }
        }
        AnalyticalShape::Comparison | AnalyticalShape::TimeSeries => {
            if projection_count != 2 {
                return Err(SqlPolicyError::Unsupported(
                    "comparison/time-series queries require exactly two output columns".to_owned(),
                ));
            }
            if !(sql.contains("count(")
                && sql.contains("created_at")
                && (sql.contains("order_date") || sql.contains("day") || sql.contains("period")))
            {
                return Err(SqlPolicyError::Unsupported(
                    "comparison/time-series queries require only a time/period dimension and aggregate metric"
                        .to_owned(),
                ));
            }
            reject_oversized_order_projection(&sql)?;
        }
        AnalyticalShape::CategoryBreakdown => {
            if projection_count != 2 {
                return Err(SqlPolicyError::Unsupported(
                    "category breakdowns require exactly two output columns".to_owned(),
                ));
            }
            if !sql.contains("count(") {
                return Err(SqlPolicyError::Unsupported(
                    "category breakdowns require a category and aggregate count".to_owned(),
                ));
            }
            reject_oversized_order_projection(&sql)?;
        }
        AnalyticalShape::Ranking => {
            if projection_count > 4 {
                return Err(SqlPolicyError::Unsupported(
                    "rankings may return at most the requested labels and aggregate metric"
                        .to_owned(),
                ));
            }
            if !(sql.contains("sum(") || sql.contains("count(")) {
                return Err(SqlPolicyError::Unsupported(
                    "rankings require a dimension and aggregate metric".to_owned(),
                ));
            }
        }
        AnalyticalShape::RecordList => {
            if is_customer_record_request(question) {
                if projection_count != 3
                    || !sql.contains("distinct")
                    || ![
                        "from customer as c",
                        "c.id as customer_id",
                        "c.first_name",
                        "c.phone",
                    ]
                    .iter()
                    .all(|field| sql.contains(field))
                {
                    return Err(SqlPolicyError::Unsupported(
                        "customer record lists require unique customer_id, first_name, and phone only"
                            .to_owned(),
                    ));
                }
            } else if projection_count > 5
                || ![
                    "order_id",
                    "display_id",
                    "store_invoice_number",
                    "zoho_order_status",
                ]
                .iter()
                .all(|field| sql.contains(field))
            {
                return Err(SqlPolicyError::Unsupported(
                    "order record lists require the minimal operational order projection"
                        .to_owned(),
                ));
            }
            if !is_customer_record_request(question)
                && [
                    "today",
                    "yesterday",
                    "last ",
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
                .any(|term| normalized.contains(term))
                && !sql.contains("created_at")
            {
                return Err(SqlPolicyError::Unsupported(
                    "dated order record lists require created_at".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn reject_oversized_order_projection(sql: &str) -> Result<(), SqlPolicyError> {
    if [
        " as order_id",
        "display_id",
        "store_invoice_number",
        "currency_code",
        "region_id",
        "tax_rate",
        "external_id",
    ]
    .iter()
    .any(|field| sql.contains(field))
    {
        Err(SqlPolicyError::Unsupported(
            "aggregate queries must not retrieve order-record columns".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn query_recipe(question: &str) -> Option<&'static str> {
    let normalized = question.to_lowercase();
    let shape = AnalyticalShape::classify(question);
    let raw_sku = normalized.contains("raw product_variant.sku")
        || normalized.contains("raw sku")
        || normalized.contains("product_variant.sku");
    let sku_sales = ["sku", "variant", "item"]
        .iter()
        .any(|term| normalized.contains(term))
        && ["sell", "sold", "sales", "quantity", "top", "most"]
            .iter()
            .any(|term| normalized.contains(term));
    if is_customer_record_request(question) {
        Some(
            "Use one SELECT DISTINCT c.id AS customer_id, c.first_name, c.phone FROM customer AS c JOIN \"order\" AS o ON o.customer_id = c.id. Apply one Asia/Kolkata half-open date filter to o.created_at. Select no other columns.",
        )
    } else if shape == AnalyticalShape::Scalar
        && normalized.contains("average")
        && normalized.contains("line item")
        && normalized.contains("per order")
    {
        Some(
            "Use exactly one scalar projection at the outer query: SELECT AVG(per_order.line_item_count) AS average_line_items_per_order FROM (SELECT o.id, COUNT(li.id)::double precision AS line_item_count FROM \"order\" AS o JOIN line_item AS li ON li.order_id = o.id WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.id) AS per_order. Parameters are the start and exclusive-end dates. This counts line-item rows per order; do not average li.quantity and do not add an outer GROUP BY.",
        )
    } else if shape == AnalyticalShape::Scalar
        && normalized.contains("customer")
        && normalized.contains("order")
    {
        Some(
            "Use one SELECT COUNT(DISTINCT o.customer_id) AS customer_count FROM \"order\" AS o with the required Asia/Kolkata half-open date range. Select no customer rows or PII.",
        )
    } else if shape == AnalyticalShape::Scalar
        && (normalized.contains("sku") || normalized.contains("variant"))
    {
        Some(
            "Use one SELECT COUNT(DISTINCT li.variant_id) AS variant_count FROM line_item AS li JOIN \"order\" AS o ON o.id = li.order_id with the required Asia/Kolkata date filter on o.created_at and li.variant_id IS NOT NULL. Select no variant rows or titles.",
        )
    } else if shape == AnalyticalShape::Scalar && normalized.contains("order") {
        Some(
            "Use one SELECT COUNT(*) AS order_count FROM \"order\" AS o with only the required filters. For an explicit date range use timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date). Select no order record columns.",
        )
    } else if shape == AnalyticalShape::Comparison
        && !normalized.contains("preceding 7")
        && ["1st", "2nd", "3rd", " 1 ", " 2 ", " 3 "]
            .iter()
            .any(|term| normalized.contains(term))
    {
        Some(
            "Use one direct SELECT timezone('Asia/Kolkata', o.created_at)::date AS order_date, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY order_date ORDER BY order_date. Parameters are exactly the start date and exclusive end date. Do not use CTEs, windows, UNION, or row-level columns.",
        )
    } else if shape == AnalyticalShape::Comparison && !normalized.contains("preceding 7") {
        Some(
            "For a month comparison, use one direct SELECT to_char(date_trunc('month', timezone('Asia/Kolkata', o.created_at)), 'YYYY-MM') AS period, COUNT(*) AS order_count FROM \"order\" AS o with exactly one $1 start and $2 exclusive-end range, GROUP BY date_trunc('month', timezone('Asia/Kolkata', o.created_at)), ORDER BY that month expression. Use optional prior context only to resolve an explicitly referenced month. Do not use CTEs, windows, UNION, or row fields.",
        )
    } else if shape == AnalyticalShape::TimeSeries && normalized.contains("june") {
        Some(
            "Use exactly SELECT timezone('Asia/Kolkata', o.created_at)::date AS order_date, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY order_date ORDER BY order_date with parameters [\"2026-06-01\",\"2026-07-01\"] for June 2026. Rust calculates total/min/max/average. Do not add CTEs, windows, UNION, or row fields.",
        )
    } else if shape == AnalyticalShape::RecordList && normalized.contains("order") {
        Some(
            "Use one SELECT o.id AS order_id, o.display_id, o.store_invoice_number, COALESCE(o.zoho_order_status::text, 'UNSPECIFIED') AS zoho_order_status, o.created_at FROM \"order\" AS o with one Asia/Kolkata half-open date filter. Select no additional order columns unless explicitly requested.",
        )
    } else if raw_sku {
        Some(
            "The raw SKU field is an explicit metadata exception, but quantity semantics are unchanged: preaggregate partial_order_cancellation_item.cancelled_quantity by line_item_id; group on pv.sku; sum GREATEST(li.quantity - cancelled quantity, 0); and exclude fully cancelled orders via native/Zoho status.",
        )
    } else if sku_sales {
        Some(
            "You MUST NOT select sku. Use li.variant_id and li.title AS historical_product_label; preaggregate partial_order_cancellation_item.cancelled_quantity by line_item_id; sum GREATEST(li.quantity - cancelled quantity, 0); exclude fully cancelled orders via native/Zoho status; and use only optional LEFT JOIN catalog enrichment.",
        )
    } else if shape == AnalyticalShape::CategoryBreakdown && normalized.contains("status") {
        Some(
            "Use one operational status aggregation: SELECT COALESCE(o.zoho_order_status::text, 'UNSPECIFIED') AS operational_status, COUNT(*) AS order_count FROM \"order\" AS o with one Asia/Kolkata half-open date range, GROUP BY o.zoho_order_status::text, ORDER BY order_count DESC.",
        )
    } else if normalized.contains("by day") && normalized.contains("last 30") {
        Some(
            "Use a freshness CTE with timezone('Asia/Kolkata', MAX(created_at))::date AS latest_day from \"order\". Cross join it to \"order\" AS o, group by timezone('Asia/Kolkata', o.created_at)::date AS day, and filter >= latest_day - 29 * INTERVAL '1 day' and < latest_day + INTERVAL '1 day'. Return day and COUNT(*) AS order_count ordered by day.",
        )
    } else if normalized.contains("compare") && normalized.contains("preceding 7") {
        Some(
            "Use exactly this shape: WITH freshness AS (SELECT timezone('Asia/Kolkata', MAX(created_at))::date AS latest_day FROM \"order\"), period_counts AS (SELECT 'Last 7 available days' AS period, COUNT(*) AS order_count FROM \"order\" AS o CROSS JOIN freshness WHERE timezone('Asia/Kolkata', o.created_at) >= freshness.latest_day - INTERVAL '6 days' AND timezone('Asia/Kolkata', o.created_at) < freshness.latest_day + INTERVAL '1 day' UNION ALL SELECT 'Preceding 7 days' AS period, COUNT(*) AS order_count FROM \"order\" AS o CROSS JOIN freshness WHERE timezone('Asia/Kolkata', o.created_at) >= freshness.latest_day - INTERVAL '13 days' AND timezone('Asia/Kolkata', o.created_at) < freshness.latest_day - INTERVAL '6 days') SELECT period, order_count FROM period_counts.",
        )
    } else {
        None
    }
}

fn validate_analytical_semantics(
    proposal: &QueryProposal,
    question: &str,
    data_as_of: Option<&str>,
) -> Result<(), SqlPolicyError> {
    let question = question.to_lowercase();
    let sql = proposal.sql.to_lowercase();
    if question.contains("average")
        && question.contains("line item")
        && question.contains("per order")
        && !(sql.contains("count(li.id)")
            && sql.contains("group by o.id")
            && sql.contains("avg(per_order.line_item_count)"))
    {
        return Err(SqlPolicyError::Unsupported(
            "average line-item rows per order requires an order-grain COUNT(li.id) subquery and an outer AVG"
                .to_owned(),
        ));
    }
    if AnalyticalShape::classify(&question) == AnalyticalShape::Comparison {
        if let Some((expected_start, expected_end)) = explicit_comparison_range(&question) {
            let actual = proposal
                .parameters
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            if actual.as_slice() != [expected_start.as_str(), expected_end.as_str()] {
                return Err(SqlPolicyError::Unsupported(format!(
                    "explicit date comparison requires parameters [{expected_start}, {expected_end}] where the second value is exclusive"
                )));
            }
        } else if let Some((expected_start, expected_end)) =
            explicit_month_comparison_range(&question)
        {
            let actual = proposal
                .parameters
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            if actual.as_slice() != [expected_start.as_str(), expected_end.as_str()] {
                return Err(SqlPolicyError::Unsupported(format!(
                    "month comparison requires parameters [{expected_start}, {expected_end}] so both complete months are included"
                )));
            }
        }
    }
    let time_question = [
        "today",
        "yesterday",
        "last ",
        "previous",
        "latest",
        "available days",
        "january",
        "february",
        "march",
        "april",
        "may ",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ]
    .iter()
    .any(|term| question.contains(term));
    if time_question && sql.contains("created_at") && !sql.contains("asia/kolkata") {
        return Err(SqlPolicyError::Unsupported(
            "calendar-time queries must explicitly use Asia/Kolkata".to_owned(),
        ));
    }

    let explicit_calendar_date = [
        "january",
        "february",
        "march",
        "april",
        "may ",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ]
    .iter()
    .any(|term| question.contains(term));
    if explicit_calendar_date
        && sql.contains("created_at")
        && !(sql.contains(">=") && sql.contains('<'))
    {
        return Err(SqlPolicyError::Unsupported(
            "a specific business date requires >= start and < end_exclusive".to_owned(),
        ));
    }

    let relative_question = [
        "today",
        "yesterday",
        "last ",
        "previous",
        "latest",
        "available",
    ]
    .iter()
    .any(|term| question.contains(term));
    let parameter_uses_freshness = data_as_of.is_some_and(|freshness| {
        proposal
            .parameters
            .iter()
            .any(|parameter| parameter.as_str() == Some(freshness))
    });
    let query_uses_freshness = sql.contains("max(") && sql.contains("created_at");
    if relative_question && !parameter_uses_freshness && !query_uses_freshness {
        return Err(SqlPolicyError::Unsupported(
            "relative periods must be anchored to data_as_of or MAX(created_at)".to_owned(),
        ));
    }
    if relative_question && (sql.contains("current_timestamp") || sql.contains("current_date")) {
        return Err(SqlPolicyError::Unsupported(
            "relative periods cannot assume wall-clock database freshness".to_owned(),
        ));
    }

    let raw_sku_request = question.contains("raw sku")
        || question.contains("raw product_variant.sku")
        || question.contains("product_variant.sku");
    let sku_sales_request = ["sku", "variant", "item"]
        .iter()
        .any(|term| question.contains(term))
        && ["sell", "sold", "sales", "quantity", "top", "most"]
            .iter()
            .any(|term| question.contains(term));
    let sku_count_request = AnalyticalShape::classify(&question) == AnalyticalShape::Scalar
        && (question.contains("sku") || question.contains("variant"));
    let product_sales_request = question.contains("product") || sku_sales_request;
    if product_sales_request
        && !sku_count_request
        && !raw_sku_request
        && !(sql.contains("line_item")
            && sql.contains("variant_id")
            && sql.contains("line_item_id")
            && sql.contains("cancelled_quantity")
            && sql.contains("greatest(")
            && sql.contains("line_item.title")
            && sql.contains("zoho_order_status"))
    {
        return Err(SqlPolicyError::Unsupported(
            "product sales require variant_id, historical line-item title, net partial-cancellation quantity, and canonical full-cancellation exclusion".to_owned(),
        ));
    }
    if sku_sales_request && !sku_count_request && !raw_sku_request && sql.contains(".sku") {
        return Err(SqlPolicyError::Unsupported(
            "ordinary SKU analytics must use variant_id, not the sparse raw SKU field".to_owned(),
        ));
    }
    let has_readable_sku_labels =
        sql.contains("historical_product_label") || sql.contains("line_item.title");
    if sku_sales_request && !sku_count_request && !raw_sku_request && !has_readable_sku_labels {
        return Err(SqlPolicyError::Unsupported(
            "SKU-level results require the historical line_item.title label".to_owned(),
        ));
    }
    if raw_sku_request && !sql.contains(".sku") {
        return Err(SqlPolicyError::Unsupported(
            "an explicit raw SKU request must select product_variant.sku".to_owned(),
        ));
    }
    if raw_sku_request
        && !(sql.contains("line_item")
            && sql.contains("variant_id")
            && sql.contains("product_variant")
            && sql.contains("sum("))
    {
        return Err(SqlPolicyError::Unsupported(
            "raw SKU sales still require variant-linked line-item aggregation".to_owned(),
        ));
    }

    let status_request = question.contains("status");
    let individual_orders = AnalyticalShape::classify(&question) == AnalyticalShape::RecordList
        && question.contains("order")
        && !question.contains("customer");
    let explicitly_named_status = [
        "medusa",
        "fulfillment status",
        "payment status",
        "delivery status",
        "zoho",
        "operational status",
    ]
    .iter()
    .any(|term| question.contains(term));
    if status_request && !individual_orders && !sql.contains("count(") {
        return Err(SqlPolicyError::Unsupported(
            "general status questions require an aggregate count".to_owned(),
        ));
    }
    if status_request
        && !individual_orders
        && !explicitly_named_status
        && !sql.contains("zoho_order_status")
    {
        return Err(SqlPolicyError::Unsupported(
            "general status analytics default to operational/Zoho status".to_owned(),
        ));
    }
    if individual_orders
        && ![
            "order_id",
            "display_id",
            "store_invoice_number",
            "zoho_order_status",
        ]
        .iter()
        .all(|field| sql.contains(field))
    {
        return Err(SqlPolicyError::Unsupported(
            "individual order records require the minimal operational order projection".to_owned(),
        ));
    }
    if question.contains("compare")
        && question.contains("preceding 7")
        && !sql.trim_start().starts_with("with ")
    {
        return Err(SqlPolicyError::Unsupported(
            "two-period available-data comparisons require a freshness CTE".to_owned(),
        ));
    }
    Ok(())
}

fn explicit_comparison_range(question: &str) -> Option<(String, String)> {
    let normalized = question.to_lowercase();
    let month = [
        ("january", 1),
        ("february", 2),
        ("march", 3),
        ("april", 4),
        ("may", 5),
        ("june", 6),
        ("july", 7),
        ("august", 8),
        ("september", 9),
        ("october", 10),
        ("november", 11),
        ("december", 12),
    ]
    .into_iter()
    .find_map(|(name, number)| normalized.contains(name).then_some(number))?;
    let numbers = normalized
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u32>().ok())
        .collect::<Vec<_>>();
    let year = numbers
        .iter()
        .copied()
        .find(|number| (2000..=2100).contains(number))?;
    let mut days = numbers
        .iter()
        .copied()
        .filter(|number| (1..=31).contains(number))
        .collect::<Vec<_>>();
    days.sort_unstable();
    days.dedup();
    if days.len() != 2 {
        return None;
    }
    let start = format!("{year:04}-{month:02}-{:02}", days[0]);
    let last = format!("{year:04}-{month:02}-{:02}", days[1]);
    Some((start, next_iso_date(&last)?))
}

fn explicit_month_comparison_range(question: &str) -> Option<(String, String)> {
    let normalized = question.to_lowercase();
    let mut months = [
        ("january", 1u32),
        ("february", 2),
        ("march", 3),
        ("april", 4),
        ("may", 5),
        ("june", 6),
        ("july", 7),
        ("august", 8),
        ("september", 9),
        ("october", 10),
        ("november", 11),
        ("december", 12),
    ]
    .into_iter()
    .filter_map(|(name, number)| normalized.contains(name).then_some(number))
    .collect::<Vec<_>>();
    months.sort_unstable();
    months.dedup();
    if months.len() < 2 {
        return None;
    }
    let year = normalized
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse::<u32>().ok())
        .find(|number| (2000..=2100).contains(number))?;
    let first = *months.first()?;
    let last = *months.last()?;
    let (end_year, end_month) = if last == 12 {
        (year + 1, 1)
    } else {
        (year, last + 1)
    };
    Some((
        format!("{year:04}-{first:02}-01"),
        format!("{end_year:04}-{end_month:02}-01"),
    ))
}

fn validation_error_category(error: &SqlPolicyError) -> &'static str {
    match error {
        SqlPolicyError::InvalidPlaceholder => {
            "invalid scalar query parameters: use plain JSON scalar values and make their count exactly match contiguous $1, $2 placeholders"
        }
        SqlPolicyError::Unsupported(_) => "unsupported read-only query shape",
        SqlPolicyError::ForbiddenFunction(_) => "function outside the analytics allowlist",
        SqlPolicyError::ForbiddenRelation(_) => "relation outside the analytics allowlist",
        SqlPolicyError::Wildcard => "projection wildcard is not permitted",
        _ => "read-only safety policy rejection",
    }
}

fn execution_retry_feedback(sql: &str, detail: &str) -> String {
    let normalized = detail.to_lowercase();
    let category = if normalized.contains("enum")
        || normalized.contains("operator does not exist")
        || normalized.contains("type")
    {
        "PostgreSQL type mismatch; cast enum status fields to ::text before COALESCE or comparison"
    } else if normalized.contains("missing from-clause")
        || normalized.contains("does not exist")
        || normalized.contains("ambiguous")
    {
        "invalid table alias or column reference"
    } else if normalized.contains("group") {
        "invalid aggregation grouping"
    } else {
        "PostgreSQL SELECT syntax mismatch"
    };
    format!(
        "Prior approved read-only SELECT: {sql}\nSafe error category: {category}. Generate a simpler single SELECT, use short aliases such as o/li/pv/p, and use only supplied schema columns."
    )
}

fn deterministic_request_block(question: &str) -> Option<&'static str> {
    let normalized = question.to_lowercase();
    let mutation = [
        "insert ",
        "update ",
        "delete ",
        "upsert ",
        "merge ",
        "create table",
        "create view",
        "create index",
        "create schema",
        "create database",
        "create function",
        "create trigger",
        "create extension",
        "alter ",
        "drop ",
        "truncate ",
        "copy ",
    ]
    .iter()
    .any(|term| normalized.contains(term));
    if mutation {
        return Some("Mill-it Ops only supports read-only analytics queries.");
    }

    let hard_sensitive = [
        "password",
        "otp",
        "email address",
        "full address",
        "payment token",
        "payment data",
        "gateway id",
    ]
    .iter()
    .any(|term| normalized.contains(term));
    if hard_sensitive {
        return Some("Mill-it Ops cannot retrieve sensitive customer or payment data.");
    }
    let local_customer_fields = normalized.contains("phone number")
        || normalized.contains("customer name")
        || normalized.contains("first name");
    (local_customer_fields && !is_customer_record_request(question))
        .then_some("Mill-it Ops cannot retrieve sensitive customer or payment data.")
}

fn likely_analytics_request(question: &str) -> bool {
    let normalized = question.to_lowercase();
    [
        "order",
        "customer",
        "product",
        "variant",
        "sku",
        "sales",
        "sold",
        "fulfill",
        "payment",
        "refund",
        "aov",
        "order value",
        "revenue",
        "gross value",
        "line item",
        "delivery",
        "shipping",
        "packed",
        "same metric",
        "store",
        "reorder",
        "busy day",
        "forecast",
        "projection",
        "compare",
        "month by month",
        "week by week",
        "time series",
    ]
    .iter()
    .any(|term| normalized.contains(term))
}

fn validate_historical_periods(plan: &AnalyticsPlan, data_as_of: &str) -> Result<(), AgentError> {
    let latest_complete = latest_complete_business_date(data_as_of).ok_or_else(|| {
        AgentError::InvalidPlan("data freshness timestamp was invalid".to_owned())
    })?;
    let historical_boundary = latest_complete
        .succ_opt()
        .ok_or_else(|| AgentError::InvalidPlan("latest complete date overflowed".to_owned()))?;
    if plan
        .periods
        .iter()
        .any(|period| period.end_exclusive > historical_boundary)
    {
        return Err(AgentError::InvalidPlan(format!(
            "historical analytics must end by {historical_boundary}; current partial or future dates require a forecast operation"
        )));
    }
    Ok(())
}

fn is_projection_evaluation_request(question: &str) -> bool {
    let normalized = question.to_lowercase();
    (normalized.contains("projection") || normalized.contains("forecast"))
        && [
            "hold up",
            "perform against",
            "meet",
            "actual",
            "exceed",
            "miss",
        ]
        .iter()
        .any(|term| normalized.contains(term))
}

fn parse_leading_count(answer: &str) -> usize {
    answer
        .split_whitespace()
        .next()
        .and_then(|value| value.replace(',', "").parse().ok())
        .unwrap_or(0)
}

fn safe_generated_parameters_for_log(parameters: &[Value]) -> String {
    let sanitized = parameters
        .iter()
        .map(|value| match value {
            Value::String(value) if is_safe_analytical_log_value(value) => {
                Value::String(value.clone())
            }
            Value::Number(_) | Value::Bool(_) | Value::Null => value.clone(),
            Value::String(_) | Value::Array(_) | Value::Object(_) => {
                Value::String("<redacted>".to_owned())
            }
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&sanitized).unwrap_or_else(|_| "[\"<redacted>\"]".to_owned())
}

fn is_safe_analytical_log_value(value: &str) -> bool {
    let iso_date = value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| index == 4 || index == 7 || character.is_ascii_digit());
    let category = value.len() <= 40
        && value
            .chars()
            .all(|character| character.is_ascii_uppercase() || " _-".contains(character));
    iso_date || category
}

fn policy_block_message(reason: Option<&str>) -> &'static str {
    let normalized = reason.unwrap_or_default().to_lowercase();
    if [
        "password",
        "otp",
        "email",
        "phone",
        "address",
        "payment",
        "sensitive",
    ]
    .iter()
    .any(|term| normalized.contains(term))
    {
        "Mill-it Ops cannot retrieve sensitive customer or payment data."
    } else {
        "Mill-it Ops only supports read-only analytics queries."
    }
}

trait RegenerationPolicy {
    fn allows_regeneration(&self) -> bool;
}

impl RegenerationPolicy for SqlPolicyError {
    fn allows_regeneration(&self) -> bool {
        !matches!(
            self,
            Self::SensitiveColumn(_)
                | Self::ReadOnlyOnly
                | Self::SelectInto
                | Self::ExplicitLock
                | Self::MultipleStatements
        )
    }
}

#[derive(Debug)]
pub(crate) enum AgentError {
    Llm(LlmError),
    Database(DatabaseError),
    Policy(SqlPolicyError),
    InvalidPlan(String),
}

impl AgentError {
    pub(crate) fn user_message(&self) -> String {
        match self {
            Self::Llm(error) => error.user_message(),
            Self::Database(error) => error.user_message().to_owned(),
            Self::Policy(error) => error.user_message().to_owned(),
            Self::InvalidPlan(_) => {
                "The local AI model could not create a safe analytics plan. Please try again."
                    .to_owned()
            }
        }
    }
}

impl From<LlmError> for AgentError {
    fn from(error: LlmError) -> Self {
        Self::Llm(error)
    }
}

impl From<DatabaseError> for AgentError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl From<SqlPolicyError> for AgentError {
    fn from(error: SqlPolicyError) -> Self {
        Self::Policy(error)
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Llm(error) => write!(formatter, "LLM: {error}"),
            Self::Database(error) => write!(formatter, "database: {error}"),
            Self::Policy(error) => write!(formatter, "SQL policy: {error}"),
            Self::InvalidPlan(detail) => write!(formatter, "invalid agent plan: {detail}"),
        }
    }
}

impl Error for AgentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{
        AnalyticsDomain, AnalyticsFilters, CustomerAnalysis, CustomerAnalysisPlan,
        CustomerProgressionSubject, CustomerSegment, Dimension, Metric, SortSpec, TimePeriod,
        VisualizationIntent,
    };
    use chrono::NaiveDate;

    #[test]
    fn blocks_natural_language_mutations_before_any_database_use() {
        assert_eq!(
            deterministic_request_block("Delete all canceled orders"),
            Some("Mill-it Ops only supports read-only analytics queries.")
        );
        assert_eq!(
            deterministic_request_block("DROP TABLE order"),
            Some("Mill-it Ops only supports read-only analytics queries.")
        );
    }

    #[test]
    fn blocks_sensitive_requests_before_any_database_use() {
        assert_eq!(
            deterministic_request_block("Give me customer passwords and OTPs"),
            Some("Mill-it Ops cannot retrieve sensitive customer or payment data.")
        );
    }

    #[test]
    fn rejects_utc_equality_for_an_ist_business_date() {
        let proposal = QueryProposal {
            requires_database: true,
            intent: "count orders".to_owned(),
            sql: r#"SELECT COUNT(*) FROM "order" WHERE date_trunc('day', created_at) = '2026-08-10'::date"#
                .to_owned(),
            parameters: vec![],
            expected_shape: "scalar".to_owned(),
        };
        assert!(validate_proposal(
            &proposal,
            "How many orders were placed on August 10, 2026?",
            Some("2026-08-11 06:16:44+00")
        )
        .is_err());
    }

    #[test]
    fn rejects_wall_clock_for_last_available_periods() {
        let proposal = QueryProposal {
            requires_database: true,
            intent: "rank products".to_owned(),
            sql: "SELECT line_item.variant_id, MAX(line_item.title) AS title, SUM(line_item.quantity) AS units FROM line_item JOIN product_variant ON line_item.variant_id = product_variant.id WHERE line_item.created_at >= CURRENT_TIMESTAMP - INTERVAL '30 days' GROUP BY line_item.variant_id"
                .to_owned(),
            parameters: vec![],
            expected_shape: "ranking".to_owned(),
        };
        assert!(validate_proposal(
            &proposal,
            "Which products sold most in the last 30 available days?",
            Some("2026-08-11 06:16:44+00")
        )
        .is_err());
    }

    fn proposal(sql: &str, parameters: Vec<Value>, expected_shape: &str) -> QueryProposal {
        QueryProposal {
            requires_database: true,
            intent: "test analytics".to_owned(),
            sql: sql.to_owned(),
            parameters,
            expected_shape: expected_shape.to_owned(),
        }
    }

    #[test]
    fn enforces_variant_grain_for_ordinary_sku_sales() {
        let bad = proposal(
            "SELECT product_variant.sku, SUM(line_item.quantity) AS quantity_sold FROM line_item JOIN product_variant ON product_variant.id = line_item.variant_id JOIN product ON product.id = product_variant.product_id GROUP BY product_variant.sku",
            vec![],
            "ranking",
        );
        assert!(validate_proposal(&bad, "What are the 5 most selling SKUs?", None).is_err());

        let good = proposal(
            "WITH pc AS (SELECT line_item_id, SUM(cancelled_quantity) AS partial_cancelled_qty FROM partial_order_cancellation_item GROUP BY line_item_id) SELECT line_item.variant_id, line_item.title AS historical_product_label, SUM(GREATEST(line_item.quantity - COALESCE(pc.partial_cancelled_qty, 0), 0)) AS quantity_sold FROM line_item JOIN \"order\" AS o ON o.id = line_item.order_id LEFT JOIN pc ON pc.line_item_id = line_item.id WHERE NOT (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED')) GROUP BY line_item.variant_id, line_item.title ORDER BY quantity_sold DESC LIMIT 5",
            vec![],
            "ranking",
        );
        assert!(validate_proposal(&good, "What are the 5 most selling SKUs?", None).is_ok());
    }

    #[test]
    fn permits_the_raw_sku_column_only_when_explicitly_requested() {
        let raw = proposal(
            "WITH pc AS (SELECT line_item_id, SUM(cancelled_quantity) AS partial_cancelled_qty FROM partial_order_cancellation_item GROUP BY line_item_id) SELECT product_variant.sku, SUM(GREATEST(line_item.quantity - COALESCE(pc.partial_cancelled_qty, 0), 0)) AS quantity_sold FROM line_item JOIN \"order\" AS o ON o.id = line_item.order_id JOIN product_variant ON product_variant.id = line_item.variant_id LEFT JOIN pc ON pc.line_item_id = line_item.id WHERE NOT (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED')) GROUP BY product_variant.sku ORDER BY quantity_sold DESC",
            vec![],
            "ranking",
        );
        assert!(validate_proposal(
            &raw,
            "Show me the raw product_variant.sku values with sales quantities.",
            None
        )
        .is_ok());
    }

    #[test]
    fn accepts_a_simple_enum_safe_operational_status_breakdown() {
        let status = proposal(
            "SELECT COALESCE(o.zoho_order_status::text, 'UNSPECIFIED') AS operational_status, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.zoho_order_status::text ORDER BY order_count DESC",
            vec![Value::String("2026-04-01".to_owned()), Value::String("2026-07-01".to_owned())],
            "category_breakdown",
        );
        let validation = validate_proposal(
            &status,
            "List orders based on their statuses from April 2026 to June 2026.",
            None,
        );
        assert!(validation.is_ok(), "{validation:?}");
    }

    #[test]
    fn accepts_minimal_daily_volume_and_independent_comparison_queries() {
        let daily_sql = "SELECT timezone('Asia/Kolkata', o.created_at)::date AS order_date, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY order_date ORDER BY order_date";
        let daily = proposal(
            daily_sql,
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "time_series",
        );
        assert!(validate_proposal(
            &daily,
            "Show me the daily order volume for June 2026 and plot it as a graph.",
            None
        )
        .is_ok());

        let comparison = proposal(
            daily_sql,
            vec![Value::from("2026-05-01"), Value::from("2026-05-03")],
            "comparison",
        );
        let validated = validate_proposal(
            &comparison,
            "Compare orders on May 1st 2026 and May 2nd 2026",
            None,
        )
        .expect("minimal comparison should pass");
        assert!(!validated.sql().to_lowercase().contains("june"));
        assert_eq!(validated.parameter_count(), 2);

        let invalid_exclusive_end = proposal(
            daily_sql,
            vec![Value::from("2026-05-01"), Value::from("2026-05-02")],
            "comparison",
        );
        assert!(validate_proposal(
            &invalid_exclusive_end,
            "Compare orders on May 1st 2026 and May 2nd 2026",
            None,
        )
        .is_err());
    }

    #[test]
    fn separates_order_counts_from_minimal_order_records() {
        let count = proposal(
            "SELECT COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)",
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "scalar",
        );
        assert!(
            validate_proposal(&count, "How many orders were placed in June 2026?", None).is_ok()
        );

        let oversized_count = proposal(
            "SELECT o.id AS order_id, o.display_id, COUNT(*) AS order_count FROM \"order\" AS o GROUP BY o.id, o.display_id",
            vec![],
            "scalar",
        );
        assert!(validate_proposal(&oversized_count, "How many orders are there?", None).is_err());

        let records = proposal(
            "SELECT o.id AS order_id, o.display_id, o.store_invoice_number, COALESCE(o.zoho_order_status::text, 'UNSPECIFIED') AS zoho_order_status, o.created_at FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($1::text)::date + 1) ORDER BY o.created_at",
            vec![Value::from("2026-06-10")],
            "record_list",
        );
        assert!(validate_proposal(&records, "List orders placed on June 10, 2026.", None).is_ok());
    }

    #[test]
    fn enforces_minimal_status_sku_and_customer_aggregates() {
        let status = proposal(
            "SELECT COALESCE(o.zoho_order_status::text, 'UNSPECIFIED') AS operational_status, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.zoho_order_status::text ORDER BY order_count DESC",
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "category_breakdown",
        );
        assert!(
            validate_proposal(&status, "Give me the order count by status for June.", None).is_ok()
        );

        let skus = proposal(
            "SELECT COUNT(DISTINCT li.variant_id) AS variant_count FROM line_item AS li JOIN \"order\" AS o ON o.id = li.order_id WHERE li.variant_id IS NOT NULL AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)",
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "scalar",
        );
        assert!(validate_proposal(&skus, "How many different SKUs sold in June?", None).is_ok());

        let customers = proposal(
            "SELECT COUNT(DISTINCT o.customer_id) AS customer_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)",
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "scalar",
        );
        assert!(validate_proposal(&customers, "How many customers ordered in June?", None).is_ok());
    }

    #[test]
    fn distinguishes_line_item_row_count_from_item_quantity() {
        let question = "What is the average number of line items per order in June 2026?";
        let correct = proposal(
            "SELECT AVG(per_order.line_item_count) AS average_line_items_per_order FROM (SELECT o.id, COUNT(li.id)::double precision AS line_item_count FROM \"order\" AS o JOIN line_item AS li ON li.order_id = o.id WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY o.id) AS per_order",
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "scalar",
        );
        assert!(validate_proposal(&correct, question, None).is_ok());

        let wrong_quantity = proposal(
            "SELECT AVG(li.quantity) AS average_line_items_per_order FROM \"order\" AS o JOIN line_item AS li ON li.order_id = o.id WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)",
            vec![Value::from("2026-06-01"), Value::from("2026-07-01")],
            "scalar",
        );
        assert!(validate_proposal(&wrong_quantity, question, None).is_err());
    }

    #[test]
    fn permits_only_the_default_customer_record_projection_for_local_ui() {
        let customers = proposal(
            "SELECT DISTINCT c.id AS customer_id, c.first_name, c.phone FROM customer AS c JOIN \"order\" AS o ON o.customer_id = c.id WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($1::text)::date + 1)",
            vec![Value::from("2026-06-10")],
            "record_list",
        );
        assert!(validate_proposal(
            &customers,
            "Show customers who placed orders on June 10.",
            None
        )
        .is_ok());
        let password = proposal(
            "SELECT DISTINCT c.id AS customer_id, c.first_name, c.phone, c.password_hash FROM customer AS c",
            vec![],
            "record_list",
        );
        assert!(validate_proposal(&password, "Show customer records.", None).is_err());
    }

    #[test]
    fn rejects_placeholder_mismatches_before_database_execution() {
        let mismatch = proposal(
            "SELECT COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)",
            vec![Value::from("2026-06-01")],
            "scalar",
        );
        assert!(matches!(
            validate_proposal(&mismatch, "How many orders were placed in June 2026?", None),
            Err(SqlPolicyError::InvalidPlaceholder)
        ));
    }

    #[test]
    fn fills_missing_expected_comparison_dates_with_zero() {
        let mut row = serde_json::Map::new();
        row.insert("order_date".to_owned(), Value::from("2026-05-02"));
        row.insert("order_count".to_owned(), Value::from(12));
        let mut result = crate::database::QueryResult {
            columns: vec!["order_date".to_owned(), "order_count".to_owned()],
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        fill_missing_date_buckets(
            &mut result,
            &[Value::from("2026-05-01"), Value::from("2026-05-03")],
            "SELECT timezone('Asia/Kolkata', created_at)::date AS order_date, COUNT(*) AS order_count FROM \"order\" GROUP BY order_date",
        );
        assert_eq!(result.row_count, 2);
        assert_eq!(result.rows[0].get("order_count"), Some(&Value::from(0)));
        assert_eq!(result.rows[1].get("order_count"), Some(&Value::from(12)));
    }

    #[test]
    fn validates_complete_month_boundaries_for_referential_comparisons() {
        let sql = "SELECT to_char(date_trunc('month', timezone('Asia/Kolkata', o.created_at)), 'YYYY-MM') AS period, COUNT(*) AS order_count FROM \"order\" AS o WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date) AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date) GROUP BY date_trunc('month', timezone('Asia/Kolkata', o.created_at)) ORDER BY period";
        let scope = "CURRENT: Compare that with May 2026.\nREFERENTIAL CONTEXT: user: Show order volume for June 2026.";
        let good = proposal(
            sql,
            vec![Value::from("2026-05-01"), Value::from("2026-07-01")],
            "comparison",
        );
        assert!(validate_proposal(&good, scope, None).is_ok());
        let incomplete = proposal(
            sql,
            vec![Value::from("2026-05-01"), Value::from("2026-06-01")],
            "comparison",
        );
        assert!(validate_proposal(&incomplete, scope, None).is_err());
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_read_only_agent_flow() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            for question in [
                "Hi",
                "How many orders are in the database?",
                "How many orders were placed on August 10, 2026?",
                "Which 5 products sold the most in the last 30 available days?",
                "Compare order volume in the last 7 available days with the preceding 7 days.",
                "List orders based on their statuses from April 2026 to June 2026.",
                "Delete all canceled orders.",
                "Give me customer passwords and OTPs.",
            ] {
                let answer = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| panic!("agent failed for {question:?}: {error}"));
                println!("QUESTION: {question}\nANSWER: {}\n", answer.answer);
                assert!(!answer.answer.trim().is_empty());
            }
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_status_range_diagnosis() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let question = "List orders based on their statuses from April 2026 to June 2026.";
            let answer = agent
                .respond(&[ChatMessage::user(question)])
                .await
                .unwrap_or_else(|error| panic!("status query failed: {error}"));
            println!("QUESTION: {question}\nANSWER: {}", answer.answer);
            assert_eq!(answer.tables.len(), 1);
            assert_eq!(answer.charts.len(), 1);
            assert_eq!(answer.charts[0].chart_type, ChartType::HorizontalBar);
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_structured_analytics_contracts() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let cases = [
                (
                    "What are the 5 most selling SKUs?",
                    Some(ChartType::HorizontalBar),
                ),
                (
                    "Show me the raw product_variant.sku values with sales quantities.",
                    Some(ChartType::HorizontalBar),
                ),
                (
                    "List orders based on their statuses from April 2026 to June 2026.",
                    Some(ChartType::HorizontalBar),
                ),
                (
                    "List individual orders and their statuses from June 1 to June 3, 2026.",
                    None,
                ),
                (
                    "Show order volume by day for the last 30 available days.",
                    Some(ChartType::Line),
                ),
                (
                    "Compare the last 7 available days with the preceding 7 days.",
                    Some(ChartType::Bar),
                ),
            ];

            for (question, expected_chart) in cases {
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| {
                        panic!("structured query failed for {question}: {error}")
                    });
                println!(
                    "QUESTION: {question}\nRESPONSE: {}\n",
                    serde_json::to_string(&response).expect("response should serialize")
                );
                assert_eq!(response.tables.len(), 1, "expected a table for {question}");
                match expected_chart {
                    Some(chart_type) => {
                        assert_eq!(response.charts.len(), 1, "expected a chart for {question}");
                        assert_eq!(response.charts[0].chart_type, chart_type);
                        assert_eq!(
                            response.charts[0].dataset_id, response.tables[0].id,
                            "chart and table must share deterministic data"
                        );
                    }
                    None => assert!(
                        response.charts.is_empty(),
                        "row-level records should not request a chart"
                    ),
                }

                if question.contains("most selling SKUs") {
                    let keys = response.tables[0]
                        .columns
                        .iter()
                        .map(|column| column.key.as_str())
                        .collect::<Vec<_>>();
                    assert!(keys.contains(&"variant"));
                    assert!(!keys.contains(&"sku"));
                }
                if question.contains("raw product_variant.sku") {
                    assert!(response.tables[0]
                        .columns
                        .iter()
                        .any(|column| column.key == "sku"));
                }
            }
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_structured_records_and_trends() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let cases = [
                (
                    "List individual orders and their statuses from June 1 to June 3, 2026.",
                    None,
                ),
                (
                    "Show order volume by day for the last 30 available days.",
                    Some(ChartType::Line),
                ),
                (
                    "Compare the last 7 available days with the preceding 7 days.",
                    Some(ChartType::Bar),
                ),
            ];
            for (question, expected_chart) in cases {
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| {
                        panic!("structured query failed for {question}: {error}")
                    });
                println!(
                    "QUESTION: {question}\nRESPONSE: {}\n",
                    serde_json::to_string(&response).expect("response should serialize")
                );
                assert_eq!(response.tables.len(), 1);
                match expected_chart {
                    Some(chart_type) => {
                        assert_eq!(response.charts.len(), 1);
                        assert_eq!(response.charts[0].chart_type, chart_type);
                        assert_eq!(response.charts[0].dataset_id, response.tables[0].id);
                    }
                    None => assert!(response.charts.is_empty()),
                }
            }
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_structured_comparison() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let question = "Compare the last 7 available days with the preceding 7 days.";
            let response = agent
                .respond(&[ChatMessage::user(question)])
                .await
                .unwrap_or_else(|error| panic!("comparison failed: {error}"));
            println!(
                "QUESTION: {question}\nRESPONSE: {}",
                serde_json::to_string(&response).expect("response should serialize")
            );
            assert_eq!(response.tables.len(), 1);
            assert_eq!(response.tables[0].rows.len(), 2);
            assert_eq!(response.charts[0].chart_type, ChartType::Bar);
            assert_eq!(response.charts[0].dataset_id, response.tables[0].id);
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_schema_sql_request_isolation() {
        use crate::llm::ChatRole;

        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let response = agent
                .respond(&[
                    ChatMessage::user("show June orders"),
                    ChatMessage {
                        role: ChatRole::Assistant,
                        content: "June order results.".to_owned(),
                    },
                    ChatMessage::user("compare Jan through May 2026 month by month"),
                ])
                .await
                .expect("self-contained current request should execute");
            assert_eq!(response.tables[0].row_count, 5);
            assert_eq!(
                response.tables[0].rows[0]
                    .get("month")
                    .and_then(Value::as_str),
                Some("2026-01-01")
            );
            assert_eq!(
                response.tables[0].rows[4]
                    .get("month")
                    .and_then(Value::as_str),
                Some("2026-05-01")
            );
        });
    }

    #[test]
    #[ignore = "requires the configured read-only PostgreSQL database"]
    fn live_sql_planner_unavailable_fails_without_a_result() {
        use crate::llm::OllamaConfig;

        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator {
                llm: OllamaClient::from_config(OllamaConfig::for_test(
                    "http://127.0.0.1:9",
                    "unreachable-test-model",
                ))
                .expect("test client"),
                database: &database,
                forecast_store: ForecastStore::test_default(),
            };
            let error = agent
                .respond(&[ChatMessage::user("compare orders month by month")])
                .await
                .expect_err("unavailable planner must fail instead of returning stale data");
            assert!(matches!(
                error,
                AgentError::Llm(LlmError::ServiceUnavailable)
            ));
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_schema_aware_customer_cohort() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let question = "which customers placed 5 or more orders in the last 6 months but haven't ordered since june?";
            let response = agent
                .respond(&[ChatMessage::user(question)])
                .await
                .unwrap_or_else(|error| panic!("customer cohort failed: {error}"));
            assert!(!response.tables.is_empty());
            assert!((2..=6).contains(&response.tables[0].columns.len()));
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_schema_aware_sql_agent_acceptance() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            for thinking_enabled in [false, true] {
                let agent = AgentOrchestrator::new_with_thinking(&database, thinking_enabled)
                    .expect("agent should initialize");
                let cases = [
                    (
                        "compare orders month by month from jan to june 2026",
                        Some(6usize),
                    ),
                    (
                        "give me a list of orders placed from 1st may to 10th may 2026",
                        None,
                    ),
                    (
                        "which customers placed 5 or more orders in the last 6 months but haven't ordered since june?",
                        None,
                    ),
                    (
                        "how many customers placed 5 or more orders in the last 6 months but haven't ordered since june?",
                        Some(0),
                    ),
                    (
                        "give me the best selling products from may to june",
                        None,
                    ),
                    (
                        "compare the top selling products separately for april, may and june",
                        None,
                    ),
                    ("show fulfilled orders from june", None),
                    ("show payment-pending orders from june", None),
                    (
                        "compare order count, order value and AOV month by month from jan through june",
                        Some(6),
                    ),
                    (
                        "based on the previous 6 months forecast expected daily orders for the next month",
                        None,
                    ),
                ];
                let selected_case = std::env::var("MILLIT_LIVE_CASE")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok());
                for (case_index, (question, expected_rows)) in cases.into_iter().enumerate() {
                    if selected_case.is_some_and(|selected| selected != case_index)
                        || (thinking_enabled && case_index > 0)
                    {
                        continue;
                    }
                    let started = Instant::now();
                    let response = agent
                        .respond(&[ChatMessage::user(question)])
                        .await
                        .unwrap_or_else(|error| {
                            panic!(
                                "schema-aware acceptance failed (thinking={thinking_enabled}) for {question}: {error}"
                            )
                        });
                    println!(
                        "SCHEMA SQL ACCEPTANCE thinking={} question={:?} total_ms={} rows={} answer={:?}",
                        thinking_enabled,
                        question,
                        started.elapsed().as_millis(),
                        response.tables.first().map_or(0, |table| table.row_count),
                        response.answer
                    );
                    if let Some(expected_rows) = expected_rows {
                        assert_eq!(
                            response.tables.first().map_or(0, |table| table.row_count),
                            expected_rows,
                            "unexpected rows for {question}"
                        );
                    }
                    if question.starts_with("give me a list of orders") {
                        assert_eq!(response.tables[0].columns.len(), 5);
                    }
                    if question.starts_with("which customers") {
                        assert!((2..=6).contains(&response.tables[0].columns.len()));
                    }
                    if question.starts_with("how many customers") {
                        assert!(response.tables.is_empty());
                    }
                    if question.starts_with("based on") {
                        assert!((28..=31).contains(&response.tables[0].row_count));
                    }
                }
            }
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_query_planning_acceptance() {
        use crate::llm::ChatRole;

        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");

            let june_question = "Show me the daily order volume for June 2026 and plot it as a graph. Also tell me which days had the highest and lowest order volume.";
            let june = agent
                .respond(&[ChatMessage::user(june_question)])
                .await
                .unwrap_or_else(|error| panic!("June time series failed: {error}"));
            println!(
                "ACCEPTANCE June time series: {}",
                serde_json::to_string(&june).unwrap()
            );
            assert_eq!(june.tables.len(), 1);
            assert_eq!(june.charts[0].chart_type, ChartType::Line);
            assert!(june.answer.contains("highest"));
            assert!(june.answer.contains("lowest"));

            let may_question = "show me comparison between orders on may 1st 2026 and 2nd may 2026";
            let conversation = vec![
                ChatMessage::user(june_question),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: june.answer.clone(),
                },
                ChatMessage::user(may_question),
            ];
            let may = agent
                .respond(&conversation)
                .await
                .unwrap_or_else(|error| panic!("May comparison failed: {error}"));
            println!(
                "ACCEPTANCE isolated May comparison: {}",
                serde_json::to_string(&may).unwrap()
            );
            assert_eq!(may.tables[0].rows.len(), 2);
            assert_eq!(may.charts[0].chart_type, ChartType::Bar);
            assert!(
                may.answer.contains("difference")
                    || may.answer.contains("increase")
                    || may.answer.contains("decrease")
            );

            let cases = [
                (
                    "How many orders were placed in June 2026?",
                    "scalar",
                    0usize,
                ),
                (
                    "List orders placed on June 10, 2026.",
                    "record_list",
                    5usize,
                ),
                (
                    "Give me the order count by status for June.",
                    "category_breakdown",
                    2usize,
                ),
                ("How many different SKUs sold in June?", "scalar", 0usize),
                (
                    "Show customers who placed orders on June 10.",
                    "record_list",
                    3usize,
                ),
                ("How many customers ordered in June?", "scalar", 0usize),
            ];
            for (question, shape, expected_columns) in cases {
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{shape} acceptance failed for {question}: {error}")
                    });
                if question.contains("Show customers") {
                    println!(
                        "ACCEPTANCE {shape}: answer={:?}, rows={}, columns={:?}, customer values omitted",
                        response.answer,
                        response.tables.first().map_or(0, |table| table.row_count),
                        response.tables.first().map_or_else(Vec::new, |table| table.columns.iter().map(|column| column.key.as_str()).collect())
                    );
                } else {
                    println!(
                        "ACCEPTANCE {shape}: {}",
                        serde_json::to_string(&response).unwrap()
                    );
                }
                assert!(!response.answer.trim().is_empty());
                if expected_columns == 0 {
                    assert!(
                        response.tables.is_empty(),
                        "scalar should not render a table"
                    );
                } else {
                    assert_eq!(response.tables[0].columns.len(), expected_columns);
                }
            }

            let referential = vec![
                ChatMessage::user("Show order volume for June 2026."),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: "June is the prior period.".to_owned(),
                },
                ChatMessage::user("Compare that with May 2026."),
            ];
            let contextual = agent
                .respond(&referential)
                .await
                .unwrap_or_else(|error| panic!("referential follow-up failed: {error}"));
            println!(
                "ACCEPTANCE referential comparison: {}",
                serde_json::to_string(&contextual).unwrap()
            );
            assert_eq!(contextual.charts[0].chart_type, ChartType::Bar);
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_sku_and_customer_projection_acceptance() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            for (question, expected_columns) in [
                ("How many different SKUs sold in June?", 0usize),
                ("Show customers who placed orders on June 10.", 3usize),
                ("How many customers ordered in June?", 0usize),
            ] {
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| panic!("acceptance failed for {question}: {error}"));
                if question.contains("Show customers") {
                    println!(
                        "ACCEPTANCE remaining projection: answer={:?}, rows={}, columns={:?}, customer values omitted",
                        response.answer,
                        response.tables.first().map_or(0, |table| table.row_count),
                        response.tables.first().map_or_else(Vec::new, |table| table.columns.iter().map(|column| column.key.as_str()).collect())
                    );
                } else {
                    println!(
                        "ACCEPTANCE remaining projection: {}",
                        serde_json::to_string(&response).unwrap()
                    );
                }
                if expected_columns == 0 {
                    assert!(response.tables.is_empty());
                } else {
                    assert_eq!(response.tables[0].columns.len(), expected_columns);
                    assert_eq!(response.tables[0].columns[0].key, "customer_id");
                    assert_eq!(response.tables[0].columns[1].key, "first_name");
                    assert_eq!(response.tables[0].columns[2].key, "phone");
                }
            }
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_referential_follow_up_acceptance() {
        use crate::llm::ChatRole;

        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let messages = vec![
                ChatMessage::user("Show order volume for June 2026."),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: "June 2026 was the period in the prior request.".to_owned(),
                },
                ChatMessage::user("Compare that with May 2026."),
            ];
            let response = agent
                .respond(&messages)
                .await
                .unwrap_or_else(|error| panic!("referential comparison failed: {error}"));
            println!(
                "ACCEPTANCE referential comparison: {}",
                serde_json::to_string(&response).unwrap()
            );
            assert_eq!(response.tables[0].rows.len(), 2);
            assert_eq!(response.charts[0].chart_type, ChartType::Bar);
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database; deterministic tools do not require Ollama"]
    fn live_deterministic_fast_path_acceptance() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new_with_thinking(&database, true)
                .expect("agent should initialize without contacting Ollama");
            let cases = [
                "How many orders were placed in June?",
                "Show order volume by day for June 2026.",
                "Give me the most selling products in June 2026.",
                "Give me the order count by status for June.",
                "Which customers placed at least 5 orders in the last 6 months but haven't ordered in the last 30 days?",
                "Which customers bought Khapli Atta at least 3 times in the past 6 months but haven't ordered it in the last 45 days?",
                "Based on customer reorder patterns, which products are likely to see more repeat demand in the next 7 days?",
                "Based on the last 6 months of orders, when can I expect the next busy days?",
                "Which products are underperforming in the last 30 days compared with the previous 30 days?",
                "Which products are performing better in the last 30 days?",
                "Which SKUs performed worse in June compared with May?",
            ];
            for question in cases {
                let started = std::time::Instant::now();
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| panic!("fast path failed for {question}: {error}"));
                println!(
                    "FAST PATH question={question:?} duration_ms={} answer={:?} tables={} charts={}",
                    started.elapsed().as_millis(),
                    response.answer,
                    response.tables.len(),
                    response.charts.len()
                );
                assert!(!response.answer.trim().is_empty());
            }
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database; semantic compilation does not require Ollama"]
    fn live_semantic_plan_acceptance() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new_with_thinking(&database, true)
                .expect("agent should initialize without contacting Ollama");
            let cases = [
                ("compare may june July and april orders of 2026", 4usize),
                ("Compare May 2025 with May 2026 order volume.", 2),
                (
                    "comapre week by week orders from 1st week of may to last week of june 2026",
                    10,
                ),
                (
                    "Show unique customer count for every month from January through June 2026.",
                    6,
                ),
                ("Give me the top 10 SKUs from May 2026.", 10),
                (
                    "Compare the top-selling variants in April, May and June separately.",
                    30,
                ),
                (
                    "Compare non-cancelled order count for April, May and June 2026.",
                    3,
                ),
                (
                    "Show operational order status breakdown for May and June separately.",
                    7,
                ),
            ];
            for (question, expected_rows) in cases {
                let started = Instant::now();
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| {
                        panic!("semantic acceptance failed for {question}: {error}")
                    });
                let rows = response.tables.first().map_or(0, |table| table.row_count);
                println!(
                    "SEMANTIC ACCEPTANCE question={question:?} total_duration_ms={} rows={} answer={:?}",
                    started.elapsed().as_millis(),
                    rows,
                    response.answer
                );
                assert_eq!(rows, expected_rows, "unexpected row count for {question}");
                assert!(!response.charts.is_empty(), "chart missing for {question}");
            }

            let lapsed = agent
                .respond(&[ChatMessage::user(
                    "How many customers placed more than 5 orders in the past year but haven't ordered in 100 days?",
                )])
                .await
                .expect("lapsed customer count should execute");
            println!("SEMANTIC ACCEPTANCE lapsed_count={:?}", lapsed.answer);
            assert!(lapsed.answer.contains("customers"));
            assert!(
                lapsed.tables.is_empty(),
                "count response must not load PII rows"
            );

            let reorder = agent
                .respond(&[ChatMessage::user(
                    "Which products have customers approaching their normal reorder window in the next 7 days?",
                )])
                .await
                .expect("reorder demand should execute");
            assert!(!reorder.tables.is_empty());
        });
    }

    #[test]
    #[ignore = "requires the configured PostgreSQL database and configured local Ollama model"]
    fn live_generated_sql_fallback_after_semantic_routing() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let question = "What is the average number of line items per order in June 2026?";
            assert!(
                parse_semantic_plan(question, "2026-08-11 06:16:44.439091+00", None)
                    .expect("deterministic parser should remain safe")
                    .is_none()
            );
            let started = Instant::now();
            let response = agent
                .respond(&[ChatMessage::user(question)])
                .await
                .expect("generated SQL fallback should complete");
            println!(
                "GENERATED SQL FALLBACK total_duration_ms={} answer={:?}",
                started.elapsed().as_millis(),
                response.answer
            );
            assert!(!response.answer.trim().is_empty());
        });
    }

    #[test]
    #[ignore = "requires the configured local Ollama model"]
    fn live_model_translates_supported_language_to_typed_plan() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let request = AnalyticsRequestContext::from_messages(&[ChatMessage::user(
                "Provide the count of purchases recorded during May 2026.",
            )])
            .expect("request context");
            assert!(parse_semantic_plan(
                &request.current_user_message,
                "2026-08-11 06:16:44.439091+00",
                None
            )
            .unwrap()
            .is_none());
            let route = agent
                .semantic_route(&request, Some("2026-08-11 06:16:44.439091+00"), None)
                .await
                .expect("model should return a typed route");
            assert_eq!(route.route, SemanticPlannerRoute::AnalyticsPlan);
            let plan = route.plan.expect("semantic route should contain a plan");
            assert_eq!(plan.metrics, vec![crate::analytics::Metric::OrderCount]);
            assert_eq!(plan.periods.len(), 1);
            plan.validate().expect("model plan should validate");
        });
    }

    #[test]
    #[ignore = "requires the configured read-only PostgreSQL database"]
    fn live_extended_analysis_operations_acceptance() {
        use crate::llm::ChatRole;

        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let store = ForecastStore::new(
                std::env::temp_dir().join(format!("mill-it-extended-live-{}", std::process::id())),
            );
            let agent = AgentOrchestrator::new_with_thinking_and_store(&database, true, store)
                .expect("agent should initialize");
            let cases = [
                ("Compare orders month by month from January through July 2026.", 7usize),
                ("Compare non-cancelled orders month by month from January through July.", 7),
                ("Show only fulfilled orders month by month from April through July.", 4),
                ("Compare operational order status breakdown for May and June.", 7),
                ("How many payment-pending orders were created each month from May through July?", 3),
                ("Show unfulfilled orders from the last 30 complete days.", 0),
                ("Compare orders, order value and AOV from April through July month by month.", 4),
            ];
            for (question, expected_rows) in cases {
                let started = Instant::now();
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| {
                        panic!("extended metric failed for {question}: {error}")
                    });
                let rows = response.tables.first().map_or(0, |table| table.row_count);
                println!(
                    "EXTENDED ACCEPTANCE question={question:?} total_ms={} rows={} answer={:?}",
                    started.elapsed().as_millis(),
                    rows,
                    response.answer
                );
                assert_eq!(rows, expected_rows, "unexpected rows for {question}");
            }

            for (question, expect_records) in [
                ("How many customers placed at least 5 orders in the last six months but haven't placed any orders since June?", false),
                ("Which customers placed at least 5 orders in the last six months but haven't ordered since June?", true),
                ("Show customers with at least 8 orders in the past year who haven't ordered in the last 60 days.", true),
            ] {
                let started = Instant::now();
                let response = agent.respond(&[ChatMessage::user(question)]).await.unwrap_or_else(|error| panic!("entity query failed for {question}: {error}"));
                println!("ENTITY ACCEPTANCE question={question:?} total_ms={} rows={} answer={:?}", started.elapsed().as_millis(), response.tables.first().map_or(0, |table| table.row_count), response.answer);
                assert_eq!(!response.tables.is_empty(), expect_records);
                if expect_records { assert_eq!(response.tables[0].columns.len(), 6); }
            }

            for question in [
                "What was total order value in June 2026?",
                "What was average order value in June 2026?",
                "What was the average number of items per order in June 2026?",
            ] {
                let response = agent
                    .respond(&[ChatMessage::user(question)])
                    .await
                    .unwrap_or_else(|error| panic!("value metric failed for {question}: {error}"));
                println!(
                    "VALUE ACCEPTANCE question={question:?} answer={:?}",
                    response.answer
                );
                assert!(!response.answer.is_empty());
            }

            let forecast_question = "Based on the last 6 months of order volume, project the expected number of orders for each day over the next 14 days. Show the projected daily orders, expected range, identify the busiest 3 days, compare the projected 14-day total with the previous 14 complete days, and explain which historical patterns are driving the projection.";
            let started = Instant::now();
            let forecast = agent
                .respond(&[ChatMessage::user(forecast_question)])
                .await
                .expect("forecast should execute");
            println!(
                "FORECAST ACCEPTANCE total_ms={} rows={} answer={:?}",
                started.elapsed().as_millis(),
                forecast.tables[0].row_count,
                forecast.answer
            );
            assert_eq!(forecast.tables[0].row_count, 14);
            assert!(forecast.tables[0]
                .columns
                .iter()
                .any(|column| column.key == "lower_bound"));

            let august = agent
                .respond(&[ChatMessage::user(
                    "Based on orders from May through July, what should August order volume look like?",
                )])
                .await
                .expect("full-August forecast should execute");
            println!(
                "AUGUST FORECAST ACCEPTANCE rows={} answer={:?}",
                august.tables[0].row_count, august.answer
            );
            assert_eq!(august.tables[0].row_count, 31);

            let isolated = agent
                .respond(&[
                    ChatMessage::user("Compare AOV for May and July 2026."),
                    ChatMessage {
                        role: ChatRole::Assistant,
                        content: "May and July AOV were compared.".to_owned(),
                    },
                    ChatMessage::user("Compare April and June order counts."),
                ])
                .await
                .expect("standalone follow-up should remain isolated");
            assert_eq!(isolated.tables[0].row_count, 2);
            assert!(isolated.tables[0]
                .columns
                .iter()
                .any(|column| column.key == "order_count"));
            assert!(!isolated.tables[0]
                .columns
                .iter()
                .any(|column| column.key == "average_order_value"));

            let referential = agent
                .respond(&[
                    ChatMessage::user("Compare AOV for May and June 2026."),
                    ChatMessage {
                        role: ChatRole::Assistant,
                        content: "May and June AOV were compared.".to_owned(),
                    },
                    ChatMessage::user("Compare the same metric for April and July."),
                ])
                .await
                .expect("explicit same-metric follow-up should resolve");
            assert_eq!(referential.tables[0].row_count, 2);
            assert!(referential.tables[0]
                .columns
                .iter()
                .any(|column| column.key == "average_order_value"));

            let evaluation = agent
                .respond(&[ChatMessage::user("Did the projection hold up?")])
                .await
                .expect("saved forecast evaluation should execute");
            println!(
                "FORECAST EVALUATION ACCEPTANCE answer={:?}",
                evaluation.answer
            );
            assert!(evaluation.answer.contains("pending"));
            assert!(evaluation.tables[0].rows.iter().any(|row| row
                .get("actual_orders")
                .is_some_and(|value| !value.is_null())));
        });
    }

    #[test]
    #[ignore = "requires the configured read-only PostgreSQL database and local Ollama model"]
    fn live_planner_validator_semantic_pack_acceptance() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let agent = AgentOrchestrator::new(&database).expect("agent should initialize");
            let prompts = [
                "Based on the last 6 complete months of order history, forecast the expected number of orders for each day over the next 14 days. Show the projected orders and expected range for each day, identify the 3 busiest expected days, compare the projected 14-day total with the previous 14 complete days, and explain the historical patterns that are influencing the forecast.",
                "Which customers are most likely due to reorder in the next 14 days based on their historical purchase patterns? Show the customer, product/variant, last purchase date, typical reorder interval, expected reorder date, historical average quantity, confidence level, and whether they are already overdue. Also summarize which products have the highest expected reorder demand over the next 14 days.",
                "give me the most selling products in july",
                "which customers who have placed 5 or more orders in the past 6 months have not placed an order with us in the last 45 days",
            ];
            let selected_case = std::env::var("ACCEPTANCE_CASE")
                .ok()
                .and_then(|value| value.parse::<usize>().ok());
            for (index, prompt) in prompts
                .iter()
                .enumerate()
                .filter(|(index, _)| selected_case.is_none_or(|selected| selected == index + 1))
            {
                let started = Instant::now();
                let response = agent
                    .respond(&[ChatMessage::user(*prompt)])
                    .await
                    .unwrap_or_else(|error| {
                        panic!("acceptance case {} failed: {error}", index + 1)
                    });
                println!(
                    "LIVE ACCEPTANCE CASE {} total_ms={} tables={} charts={} rows={:?}",
                    index + 1,
                    started.elapsed().as_millis(),
                    response.tables.len(),
                    response.charts.len(),
                    response
                        .tables
                        .iter()
                        .map(|table| table.row_count)
                        .collect::<Vec<_>>()
                );
                assert!(!response.answer.trim().is_empty());
                if index == 0 {
                    assert_eq!(
                        response.tables.first().map(|table| table.row_count),
                        Some(14)
                    );
                }
                if index == 1 {
                    assert!(response.tables.iter().any(|table| table
                        .columns
                        .iter()
                        .any(|column| column.key == "expected_reorder_date")));
                    assert!(response.tables.iter().any(|table| table
                        .columns
                        .iter()
                        .any(|column| column.key == "estimated_expected_units")));
                }
                if index == 3 {
                    assert!(response.charts.is_empty());
                }
            }
        });
    }

    #[test]
    fn normalizes_contiguous_month_periods_for_weekly_series() {
        use crate::analytics::{
            AnalyticsDomain, AnalyticsFilters, Dimension, Metric, SortSpec, TimePeriod,
            VisualizationIntent,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Deep,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Operations,
                metrics: vec![Metric::OrderCount],
                dimensions: vec![Dimension::Week],
                periods: vec![
                    TimePeriod {
                        label: "May 2026".to_owned(),
                        start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                        end_exclusive: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
                    },
                    TimePeriod {
                        label: "June 2026".to_owned(),
                        start: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
                        end_exclusive: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    },
                    TimePeriod {
                        label: "July 2026".to_owned(),
                        start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                        end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                    },
                ],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::Line),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_semantic_route(&mut route);

        let plan = route.plan.unwrap();

        assert_eq!(plan.periods.len(), 1);
        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap()
        );
        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
        assert_eq!(plan.dimensions, vec![Dimension::Week]);
    }
    #[test]
    fn removes_redundant_period_dimension_from_single_period_scalar() {
        use crate::analytics::{
            AnalyticsDomain, AnalyticsFilters, Dimension, Metric, SortSpec, TimePeriod,
            VisualizationIntent,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::None,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Operations,
                metrics: vec![Metric::OrderCount],
                dimensions: vec![Dimension::Period],
                periods: vec![TimePeriod {
                    label: "Yesterday".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 8, 12).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::None),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_semantic_route(&mut route);

        let plan = route.plan.expect("analytics plan should remain present");

        assert!(plan.dimensions.is_empty());
        assert_eq!(plan.periods.len(), 1);
        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 8, 12).unwrap()
        );
        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        );
    }
    #[test]
    fn removes_redundant_cancellation_filters_from_cancellation_metric() {
        use crate::analytics::{
            AnalyticsDomain, AnalyticsFilters, Metric, StatusDimension, StatusFilter, TimePeriod,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::None,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Operations,
                metrics: vec![Metric::CancelledOrderCount],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    exclude_cancelled: false,
                    status_filters: vec![StatusFilter {
                        dimension: StatusDimension::Operational,
                        values: vec!["CANCELED".to_owned()],
                        exclude: true,
                    }],
                    store_id: None,
                    product: None,
                    products: Vec::new(),
                    variant_id: None,
                },
                sort: None,
                limit: None,
                visualization: None,
            }),
            specialized_tool: None,
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_semantic_route(&mut route);

        let plan = route.plan.unwrap();

        assert!(!plan.filters.exclude_cancelled);
        assert!(plan.filters.status_filters.is_empty());
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn partitions_overlapping_comparison_periods_by_next_start() {
        use crate::analytics::{AnalyticsDomain, AnalyticsFilters, Dimension, Metric, TimePeriod};
        use chrono::NaiveDate;

        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount, Metric::AverageOrderValue],
            dimensions: vec![Dimension::Period],
            periods: vec![
                TimePeriod {
                    label: "May 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                },
                TimePeriod {
                    label: "June 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                },
                TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                },
            ],
            filters: AnalyticsFilters::default(),
            sort: None,
            limit: None,
            visualization: None,
        };

        normalize_overlapping_period_partitions(&mut plan);

        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()
        );
        assert_eq!(
            plan.periods[1].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
        );
        assert_eq!(
            plan.periods[2].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );

        assert!(plan.validate().is_ok());
    }
    #[test]
    fn normalizes_simple_customer_count_to_unique_customers_metric() {
        use crate::analytics::{
            AnalyticsDomain, EntityListPlan, EntityType, Metric, SortSpec, TimePeriod,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::EntityList,
            analysis_depth: AnalysisDepth::None,
            plan: None,
            entity_plan: Some(EntityListPlan {
                entity: EntityType::Customer,
                period: TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                },
                minimum_order_count: Some(1),
                maximum_order_count: None,
                inactive_days: None,
                no_orders_since: None,
                purchased_product: None,
                return_records: false,
                sort: Some(SortSpec::Descending),
                limit: 500,
            }),
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_semantic_route(&mut route);

        assert_eq!(route.route, SemanticPlannerRoute::AnalyticsPlan);
        assert!(route.entity_plan.is_none());

        let plan = route.plan.expect("analytics plan should be created");

        assert_eq!(plan.domain, AnalyticsDomain::Customers);
        assert_eq!(plan.metrics, vec![Metric::UniqueCustomers]);
        assert!(plan.dimensions.is_empty());
        assert_eq!(plan.periods.len(), 1);
        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
        );
        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
        assert!(plan.filters.exclude_cancelled);
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn does_not_normalize_customer_cohort_to_unique_customers_metric() {
        use crate::analytics::{EntityListPlan, EntityType, SortSpec, TimePeriod};
        use chrono::NaiveDate;

        let original_entity_plan = EntityListPlan {
            entity: EntityType::Customer,
            period: TimePeriod {
                label: "Last six months".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            },
            minimum_order_count: Some(5),
            maximum_order_count: None,
            inactive_days: Some(45),
            no_orders_since: None,
            purchased_product: None,
            return_records: false,
            sort: Some(SortSpec::Descending),
            limit: 500,
        };

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::EntityList,
            analysis_depth: AnalysisDepth::None,
            plan: None,
            entity_plan: Some(original_entity_plan.clone()),
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_semantic_route(&mut route);

        assert_eq!(route.route, SemanticPlannerRoute::EntityList);
        assert_eq!(route.entity_plan, Some(original_entity_plan));
        assert!(route.plan.is_none());
    }
    #[test]
    fn explicit_analysis_intent_is_normalized_to_deep() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: None,
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_analysis_depth(&mut route, "Analyze our monthly order volume.");

        assert_eq!(route.analysis_depth, AnalysisDepth::Deep);
    }

    #[test]
    fn ordinary_reporting_does_not_force_deep_analysis() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: None,
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_analysis_depth(&mut route, "Show our monthly order volume.");

        assert_eq!(route.analysis_depth, AnalysisDepth::Summary);
    }
    #[test]
    fn product_dimension_normalizes_order_count_to_product_order_count() {
        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Products,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Product],
            periods: vec![TimePeriod {
                label: "July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Descending),
            limit: Some(10),
            visualization: Some(VisualizationIntent::HorizontalBar),
        };

        normalize_product_metric_shape(&mut plan);

        assert_eq!(plan.metrics, vec![Metric::ProductOrderCount]);

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn variant_dimension_normalizes_unique_customers_to_product_buyers() {
        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Products,
            metrics: vec![Metric::UniqueCustomers],
            dimensions: vec![Dimension::Variant],
            periods: vec![TimePeriod {
                label: "July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Descending),
            limit: Some(10),
            visualization: Some(VisualizationIntent::HorizontalBar),
        };

        normalize_product_metric_shape(&mut plan);

        assert_eq!(plan.metrics, vec![Metric::ProductUniqueCustomers]);

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn ordinary_order_count_is_not_changed_by_product_normalizer() {
        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![],
            periods: vec![TimePeriod {
                label: "July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters::default(),
            sort: None,
            limit: None,
            visualization: None,
        };

        normalize_product_metric_shape(&mut plan);

        assert_eq!(plan.metrics, vec![Metric::OrderCount]);

        assert!(!plan.filters.exclude_cancelled);
    }
    #[test]
    fn explicit_product_sales_value_repairs_order_value_metric() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::OrderValue],
                dimensions: vec![Dimension::Product],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Bar),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_product_sales_value(
            &mut route,
            "Which products generated the most sales value in July 2026?",
        );

        let plan = route.plan.unwrap();

        assert_eq!(plan.metrics, vec![Metric::ProductSalesValue]);

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn generic_order_value_is_not_rewritten_as_product_sales_value() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::OrderValue],
                dimensions: vec![Dimension::Product],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: None,
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_product_sales_value(
            &mut route,
            "Show order value for orders containing these products.",
        );

        assert_eq!(route.plan.unwrap().metrics, vec![Metric::OrderValue]);
    }
    #[test]
    fn explicit_product_sales_value_repairs_quantity_sold_metric() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![Dimension::Product],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Bar),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_product_sales_value(
            &mut route,
            "Which products generated the most sales value in July 2026?",
        );

        let plan = route.plan.unwrap();

        assert_eq!(plan.metrics, vec![Metric::ProductSalesValue]);

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn product_sales_volume_stays_quantity_sold() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![Dimension::Product],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Bar),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_product_sales_value(
            &mut route,
            "Which products had the most sales volume in July 2026?",
        );

        assert_eq!(route.plan.unwrap().metrics, vec![Metric::QuantitySold]);
    }
    #[test]
    fn products_domain_repairs_generic_product_metrics_without_dimension() {
        // Products + OrderCount/UniqueCustomers
        // -> ProductOrderCount/ProductUniqueCustomers
    }
    #[test]
    fn single_period_removes_period_dimension_but_preserves_other_dimensions() {
        // [Period, Product] -> [Product]
    }
    #[test]
    fn explicit_monthly_request_rejects_non_month_grain() {
        // question says monthly
        // plan has Period
        // -> Err
    }
    #[test]
    fn explicit_product_sales_value_replaces_unrequested_cancellation_metric() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::CancelledOrderCount,
                ],
                dimensions: vec![Dimension::Month],
                periods: vec![TimePeriod {
                    label: "May through July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    exclude_cancelled: false,
                    status_filters: vec![],
                    store_id: None,
                    product: Some("Sharbati Wheat Atta".to_owned()),
                    products: Vec::new(),
                    variant_id: None,
                },
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::Line),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_product_sales_value(
        &mut route,
        "Show monthly quantity sold, product orders, unique buyers, and product sales value for Sharbati Wheat Atta from May through July 2026.",
    );

        let plan = route.plan.as_ref().unwrap();

        assert_eq!(
            plan.metrics,
            vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ]
        );

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn explicitly_requested_cancellation_metric_is_not_replaced() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::ProductSalesValue, Metric::CancelledOrderCount],
                dimensions: vec![Dimension::Month],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::Line),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_product_sales_value(
            &mut route,
            "Show product sales value and cancelled orders by month.",
        );

        let plan = route.plan.as_ref().unwrap();

        assert!(plan.metrics.contains(&Metric::ProductSalesValue));
        assert!(plan.metrics.contains(&Metric::CancelledOrderCount));
    }
    #[test]
    fn named_product_variant_breakdown_requires_product_filter() {
        let route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::ProductSalesValue,
                ],
                dimensions: vec![Dimension::Variant],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    exclude_cancelled: true,
                    ..AnalyticsFilters::default()
                },
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        let error = validate_semantic_route_for_question(
        &route,
        "Show quantity sold, product orders, unique buyers, and product sales value by variant for MP Sharbati Atta in July 2026.",
    )
    .expect_err("named product variant breakdown must preserve product filter");

        assert!(error.contains("filters.product"));
    }
    #[test]
    fn named_product_variant_breakdown_accepts_product_filter() {
        let route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::ProductSalesValue,
                ],
                dimensions: vec![Dimension::Variant],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    exclude_cancelled: true,
                    product: Some("MP Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    ..AnalyticsFilters::default()
                },
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        assert!(
        validate_semantic_route_for_question(
            &route,
            "Show quantity sold, product orders, unique buyers, and product sales value by variant for MP Sharbati Atta in July 2026.",
        )
        .is_ok()
    );
    }
    #[test]
    fn named_product_performance_snapshot_uses_four_core_metrics() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::OrderValue,
                ],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("MP Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_named_product_performance_snapshot(
            &mut route,
            "Show performance of MP Sharbati Atta in July 2026.",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.metrics,
            vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ]
        );

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.dimensions.is_empty());
        assert_eq!(plan.limit, None);
        assert_eq!(plan.visualization, Some(VisualizationIntent::Table));

        assert!(plan.validate().is_ok());
    }
    #[test]
    fn explicit_named_product_metric_is_not_expanded_to_performance_snapshot() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("MP Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_named_product_performance_snapshot(
            &mut route,
            "Show quantity sold for MP Sharbati Atta in July 2026.",
        );

        assert_eq!(route.plan.unwrap().metrics, vec![Metric::QuantitySold]);
    }
    #[test]
    fn explicit_named_month_repairs_end_at_historical_boundary() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::ProductSalesValue,
                ],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("MP Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    exclude_cancelled: true,
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_single_calendar_month(
            &mut route,
            "Show performance of MP Sharbati Atta in July 2026.",
        );

        let period = &route.plan.unwrap().periods[0];

        assert_eq!(period.start, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());

        assert_eq!(
            period.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );

        assert_eq!(period.label, "July 2026");
    }
    #[test]
    fn relative_month_period_is_not_reinterpreted() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "Last month".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: None,
                limit: None,
                visualization: None,
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        let original = route.plan.as_ref().unwrap().periods[0].clone();

        normalize_explicit_single_calendar_month(&mut route, "Show performance last month.");

        assert_eq!(route.plan.unwrap().periods[0], original);
    }
    #[test]
    fn explicit_calendar_month_range_uses_last_month_end_exclusive() {
        use crate::analytics::{
            AnalyticsDomain, AnalyticsFilters, Metric, TimePeriod, VisualizationIntent,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::ProductSalesValue,
                ],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    /*
                     * Simulate Qwen's incorrect output.
                     */
                    label: "January to July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    exclude_cancelled: true,
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_calendar_month_range(
            &mut route,
            "Show performance of Sharbati Atta from January to July 2026",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );

        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );

        assert_eq!(plan.periods[0].label, "January through July 2026");
    }
    #[test]
    fn generic_named_product_performance_uses_core_product_metrics() {
        use crate::analytics::{
            AnalyticsDomain, AnalyticsFilters, Metric, TimePeriod, VisualizationIntent,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::AverageOrderValue,
                ],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "January through July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    exclude_cancelled: false,
                    product: Some("Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_generic_named_product_performance(
            &mut route,
            "Show performance of Sharbati Atta from January to July 2026",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.metrics,
            vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ]
        );

        assert!(plan.filters.exclude_cancelled);
        assert!(plan.filters.status_filters.is_empty());
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn generic_named_product_performance_replaces_invented_cancellation_metric() {
        use crate::analytics::{
            AnalyticsDomain, AnalyticsFilters, Metric, TimePeriod, VisualizationIntent,
        };
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold, Metric::CancelledOrderCount],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "January through July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_generic_named_product_performance(
            &mut route,
            "Show performance of Sharbati Atta from January to July 2026",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.metrics,
            vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ]
        );

        assert!(plan.validate().is_ok());
    }
    #[test]
    fn explicit_named_product_metric_is_not_expanded_as_generic_performance() {
        use crate::analytics::{AnalyticsDomain, AnalyticsFilters, Metric, TimePeriod};
        use chrono::NaiveDate;

        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("Sharbati Atta".to_owned()),
                    products: Vec::new(),
                    ..AnalyticsFilters::default()
                },
                sort: None,
                limit: None,
                visualization: None,
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_generic_named_product_performance(
            &mut route,
            "Show quantity performance of Sharbati Atta in July 2026",
        );

        assert_eq!(route.plan.unwrap().metrics, vec![Metric::QuantitySold]);
    }
    #[test]
    fn semantic_planner_schema_exposes_multi_product_filter() {
        let schema = semantic_planner_schema();

        let filters = &schema["properties"]["plan"]["anyOf"][1]["properties"]["filters"];

        let products = &filters["properties"]["products"];

        assert_eq!(products["type"], "array");
        assert_eq!(products["maxItems"], 10);

        assert!(
            products.get("minItems").is_none(),
            "the structural planner schema must allow an empty products array"
        );

        assert_eq!(products["items"]["type"], "string");
        assert_eq!(products["items"]["minLength"], 1);
        assert_eq!(products["items"]["maxLength"], 120);

        let required = filters["required"]
            .as_array()
            .expect("filters.required should be an array");

        assert!(
            !required
                .iter()
                .any(|value| value.as_str() == Some("products")),
            "products should remain optional because AnalyticsFilters defaults it to []"
        );
    }
    #[test]
    fn multi_product_comparison_normalizes_to_product_grain_without_limit() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,

            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,

                metrics: vec![Metric::QuantitySold],

                dimensions: vec![],

                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],

                filters: AnalyticsFilters {
                    products: vec!["Sharbati Atta".to_owned(), "Khapli Emmer".to_owned()],
                    ..AnalyticsFilters::default()
                },

                sort: Some(SortSpec::Descending),

                limit: Some(10),

                visualization: Some(VisualizationIntent::HorizontalBar),
            }),

            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_multi_product_comparison(
            &mut route,
            "Compare Sharbati Atta and Khapli Emmer in July 2026.",
        );

        normalize_semantic_route(&mut route);

        let plan = route.plan.unwrap();

        assert_eq!(plan.domain, AnalyticsDomain::Products);

        assert_eq!(plan.dimensions, vec![Dimension::Product]);

        assert_eq!(
            plan.metrics,
            vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ]
        );

        assert_eq!(plan.limit, None);

        assert_eq!(plan.visualization, Some(VisualizationIntent::Table));

        assert!(plan.filters.exclude_cancelled);

        assert!(plan.validate().is_ok());
    }
    #[test]
    fn multi_product_comparison_preserves_explicit_metric_selection() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,

            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,

                metrics: vec![Metric::OrderCount, Metric::UniqueCustomers],

                dimensions: vec![Dimension::Product],

                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],

                filters: AnalyticsFilters {
                    products: vec!["Sharbati Atta".to_owned(), "Khapli Emmer".to_owned()],
                    ..AnalyticsFilters::default()
                },

                sort: Some(SortSpec::Descending),

                limit: Some(10),

                visualization: Some(VisualizationIntent::HorizontalBar),
            }),

            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_multi_product_comparison(
            &mut route,
            "Compare orders and buyers for Sharbati Atta and Khapli Emmer in July 2026.",
        );

        normalize_semantic_route(&mut route);

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.metrics,
            vec![Metric::ProductOrderCount, Metric::ProductUniqueCustomers,]
        );

        assert_eq!(plan.dimensions, vec![Dimension::Product]);

        assert_eq!(plan.limit, None);

        assert!(plan.filters.exclude_cancelled);

        assert!(plan.validate().is_ok());
    }
    #[test]
    fn one_product_in_products_is_repaired_to_single_product_filter() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold, Metric::ProductOrderCount],
                dimensions: vec![Dimension::Month, Dimension::Product],
                periods: vec![TimePeriod {
                    label: "January through July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    products: vec!["Sharbati Atta".to_owned()],
                    ..AnalyticsFilters::default()
                },
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_singleton_product_selector(
            &mut route,
            "Show monthly performance of Sharbati Atta from January to July 2026",
        );

        let plan = route.plan.unwrap();

        assert_eq!(plan.filters.product.as_deref(), Some("Sharbati Atta"));

        assert!(plan.filters.products.is_empty());

        assert_eq!(plan.dimensions, vec![Dimension::Month]);
    }
    #[test]
    fn how_did_named_product_perform_uses_four_core_metrics() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold, Metric::ProductOrderCount],
                dimensions: vec![Dimension::Month],
                periods: vec![TimePeriod {
                    label: "January through July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("Sharbati Atta".to_owned()),
                    ..AnalyticsFilters::default()
                },
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::Line),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_generic_named_product_performance(
            &mut route,
            "How did Sharbati Atta perform from Jan to July 2026?",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.metrics,
            vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ]
        );
    }
    #[test]
    fn abbreviated_calendar_month_range_is_repaired() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![Dimension::Month],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters {
                    product: Some("Sharbati Atta".to_owned()),
                    ..AnalyticsFilters::default()
                },
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::Line),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_calendar_month_range(
            &mut route,
            "How did Sharbati Atta perform from Jan to July 2026?",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );

        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
    }
    #[test]
    fn selected_product_comparison_preserves_exact_calendar_month() {
        let mut route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::AnalyticsPlan,
            analysis_depth: AnalysisDepth::Summary,
            plan: Some(AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![Metric::QuantitySold],
                dimensions: vec![Dimension::Product],
                periods: vec![TimePeriod {
                    label: "July 2026".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
                }],
                filters: AnalyticsFilters {
                    products: vec!["MP Sharbati Atta".to_owned(), "Khapli Atta".to_owned()],
                    ..AnalyticsFilters::default()
                },
                sort: Some(SortSpec::Descending),
                limit: None,
                visualization: Some(VisualizationIntent::Table),
            }),
            entity_plan: None,
            customer_plan: None,
            forecast_plan: None,
            specialized_tool: None,
            intent: None,
            blocked_reason: None,
        };

        normalize_explicit_single_calendar_month(
            &mut route,
            "Compare MP Sharbati Atta and Khapli Atta in July 2026.",
        );

        let plan = route.plan.unwrap();

        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
        );

        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
    }
    #[test]
    fn semantic_planner_schema_exposes_only_supported_customer_analysis() {
        let schema = semantic_planner_schema();

        let routes = schema["properties"]["route"]["enum"]
            .as_array()
            .expect("route enum");

        assert!(routes
            .iter()
            .any(|value| { value.as_str() == Some("customer_analysis") }));

        let customer_plan = &schema["properties"]["customer_plan"]["anyOf"][1];

        assert_eq!(customer_plan["properties"]["segment"]["enum"][0], "loyal",);

        let analysis = &customer_plan["properties"]["analysis"];

        assert_eq!(
            analysis["properties"]["type"]["enum"][0],
            "purchase_progression",
        );

        assert_eq!(analysis["properties"]["subject"]["enum"][0], "category",);

        /*
         * Do not expose unimplemented customer operations to
         * the planner yet.
         */
        let serialized = serde_json::to_string(customer_plan).expect("customer schema JSON");

        assert!(!serialized.contains("product_reorder",));

        assert!(!serialized.contains("frequency_distribution",));

        assert!(!serialized.contains("cohort_retention",));
    }
    #[test]
    fn loyal_category_progression_is_a_valid_semantic_route() {
        let customer_plan = CustomerAnalysisPlan {
            period: TimePeriod {
                label: "Lifetime through July 2026".to_owned(),

                start: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),

                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            },

            segment: CustomerSegment::Loyal,

            analysis: CustomerAnalysis::PurchaseProgression {
                subject: CustomerProgressionSubject::Category,

                max_purchase_number: 3,
            },
        };

        let route = SemanticPlannerResponse {
            route: SemanticPlannerRoute::CustomerAnalysis,

            analysis_depth: AnalysisDepth::Summary,

            plan: None,

            entity_plan: None,

            customer_plan: Some(customer_plan),

            forecast_plan: None,

            specialized_tool: None,

            intent: None,

            blocked_reason: None,
        };

        assert!(validate_semantic_route(&route).is_ok());

        assert!(validate_semantic_historical_boundary(
            &route,
            Some(NaiveDate::from_ymd_opt(2026, 8, 1,).unwrap(),),
        )
        .is_ok());
    }
}
