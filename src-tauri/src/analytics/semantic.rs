use super::{
    compiler::{compile_with_product_id, CompiledAnalyticsQuery},
    facts::{
        build_time_series_facts, enrich_temporal_variant_contribution_facts,
        enrich_variant_contribution_facts,
    },
    insights::{summarize_metric_query_with_facts, summarize_variant_contribution_facts},
    plan::{AnalyticsDomain, AnalyticsPlan, Dimension, Metric, SortSpec, VisualizationIntent},
    product_resolver::{resolve_product, ProductResolution, ResolvedProduct},
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

fn attach_resolved_product_identity(result: &mut QueryResult, product: &ResolvedProduct) {
    for row in &mut result.rows {
        row.insert(
            "product_name".to_owned(),
            Value::String(product.product_name.clone()),
        );

        row.insert(
            "product_id".to_owned(),
            Value::String(product.product_id.clone()),
        );
    }

    result
        .columns
        .retain(|column| column != "product_name" && column != "product_id");

    result.columns.insert(0, "product_id".to_owned());

    result.columns.insert(0, "product_name".to_owned());
}

fn is_core_product_performance_metric(metric: Metric) -> bool {
    matches!(
        metric,
        Metric::QuantitySold
            | Metric::ProductOrderCount
            | Metric::ProductUniqueCustomers
            | Metric::ProductSalesValue
    )
}

fn temporal_dimension_key(plan: &AnalyticsPlan) -> Option<&'static str> {
    if plan.dimensions.contains(&Dimension::Day) {
        Some("day")
    } else if plan.dimensions.contains(&Dimension::Week) {
        Some("week")
    } else if plan.dimensions.contains(&Dimension::Month) {
        Some("month")
    } else {
        None
    }
}

fn derive_variant_expansion_plan(plan: &AnalyticsPlan) -> Option<AnalyticsPlan> {
    if plan.domain != AnalyticsDomain::Products {
        return None;
    }

    let product = plan.filters.product.as_deref()?;

    if product.trim().is_empty() {
        return None;
    }

    // Explicit variant requests already have their grain.
    if plan.dimensions.contains(&Dimension::Variant) {
        return None;
    }

    // Multi-product comparisons belong to B5.
    if plan.dimensions.contains(&Dimension::Product) {
        return None;
    }

    if plan.periods.len() != 1 {
        return None;
    }

    /*
     * B3:
     *   dimensions=[]
     *
     * B4-A:
     *   dimensions=[day]
     *   dimensions=[week]
     *   dimensions=[month]
     *
     * Anything else remains unsupported by automatic
     * variant expansion.
     */
    let temporal_dimension = if plan.dimensions.is_empty() {
        None
    } else if plan.dimensions.len() == 1
        && matches!(
            plan.dimensions[0],
            Dimension::Day | Dimension::Week | Dimension::Month
        )
    {
        Some(plan.dimensions[0])
    } else {
        return None;
    };

    /*
     * Require genuine multi-metric product performance.
     */
    if plan.metrics.len() < 2
        || !plan
            .metrics
            .iter()
            .copied()
            .all(is_core_product_performance_metric)
    {
        return None;
    }

    let mut variant_plan = plan.clone();

    variant_plan.dimensions = if let Some(temporal_dimension) = temporal_dimension {
        vec![temporal_dimension, Dimension::Variant]
    } else {
        vec![Dimension::Variant]
    };

    variant_plan.sort = Some(if temporal_dimension.is_some() {
        SortSpec::Chronological
    } else {
        SortSpec::Descending
    });

    variant_plan.limit = None;
    variant_plan.visualization = Some(VisualizationIntent::Table);

    Some(variant_plan)
}

pub(crate) async fn execute_semantic_plan(
    database: &Database,
    question: &str,
    request_id: &str,
    plan: &AnalyticsPlan,
) -> Result<SemanticExecution, AgentError> {
    plan.validate()
        .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;
    let mut resolver_db_duration_ms = 0_u128;

    let resolved_product = if plan.domain == AnalyticsDomain::Products {
        if let Some(mention) = plan.filters.product.as_deref() {
            let lookup = resolve_product(database, mention).await?;

            resolver_db_duration_ms = lookup.db_duration_ms;

            match lookup.resolution {
                ProductResolution::Resolved(product) => {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "[analytics product resolution]\nrequest_id={}\nmention={:?}\nproduct_id={}\nproduct_name={:?}\nscore={:.3}",
                        request_id,
                        mention,
                        product.product_id,
                        product.product_name,
                        product.score
                    );

                    Some(product)
                }

                ProductResolution::Ambiguous(candidates) => {
                    let choices = candidates
                        .iter()
                        .map(|candidate| candidate.product_name.clone())
                        .collect::<Vec<_>>()
                        .join(", ");

                    #[cfg(debug_assertions)]
                    eprintln!(
                        "[analytics product resolution]\nrequest_id={}\nmention={:?}\nresult=ambiguous\nchoices={:?}",
                        request_id,
                        mention,
                        choices
                    );

                    return Ok(
                        SemanticExecution {
                            response:
                                AgentResponse::text(
                                    format!(
                                        "I found multiple published products matching \"{mention}\": {choices}. Please specify which one you mean."
                                    ),
                                ),
                            rows: 0,
                            db_duration_ms:
                                resolver_db_duration_ms,
                            calculation_duration_ms: 0,
                            summary_duration_ms: 0,
                        },
                    );
                }

                ProductResolution::NotFound => {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "[analytics product resolution]\nrequest_id={}\nmention={:?}\nresult=not_found",
                        request_id,
                        mention
                    );

                    return Ok(
                        SemanticExecution {
                            response:
                                AgentResponse::text(
                                    format!(
                                        "I couldn't confidently match \"{mention}\" to a current published product."
                                    ),
                                ),
                            rows: 0,
                            db_duration_ms:
                                resolver_db_duration_ms,
                            calculation_duration_ms: 0,
                            summary_duration_ms: 0,
                        },
                    );
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let compiled = compile_with_product_id(
        plan,
        resolved_product
            .as_ref()
            .map(|product| product.product_id.as_str()),
    )
    .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

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
                    .map(|period| { format!("{}..{}", period.start, period.end_exclusive) })
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

    // Defense in depth: deterministic compiler output still passes through
    // the same read-only AST validator before reaching PostgreSQL.
    let validated = validate_sql(&compiled.sql)?;

    if validated.parameter_count() != compiled.parameters.len() {
        return Err(AgentError::InvalidPlan(
            "compiled query placeholder count did not match its parameters".to_owned(),
        ));
    }

    let mut result = database
        .execute_deterministic(&validated, &compiled.parameters)
        .await?;

    if let Some(product) = resolved_product.as_ref() {
        /*
         * Product/variant grouped queries already return their own
         * identity columns.
         *
         * The scalar parent product-performance query does not, so
         * attach its canonical resolved identity here.
         */
        if !plan.dimensions.contains(&Dimension::Product)
            && !plan.dimensions.contains(&Dimension::Variant)
        {
            attach_resolved_product_identity(&mut result, product);
        }
    }

    let mut db_duration_ms = resolver_db_duration_ms + result.duration_ms;

    /*
     * Rust-owned automatic variant expansion.
     *
     * Qwen produces one product-level plan.
     * Rust derives the variant-grain plan from that already validated plan.
     */
    let mut variant_execution = None;
    let mut variant_contribution_facts = None;

    if let Some(variant_plan) = derive_variant_expansion_plan(plan) {
        variant_plan
            .validate()
            .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

        let variant_compiled = compile_with_product_id(
            &variant_plan,
            resolved_product
                .as_ref()
                .map(|product| product.product_id.as_str()),
        )
        .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

        #[cfg(debug_assertions)]
        {
            eprintln!(
                "[analytics derived variant plan]\nrequest_id={}\nmetrics={:?}\ndimensions={}\nperiods={}",
                request_id,
                variant_plan.metrics,
                serde_json::to_string(&variant_plan.dimensions)
                    .unwrap_or_else(|_| "[]".to_owned()),
                serde_json::to_string(
                    &variant_plan
                        .periods
                        .iter()
                        .map(|period| {
                            format!(
                                "{}..{}",
                                period.start,
                                period.end_exclusive
                            )
                        })
                        .collect::<Vec<_>>()
                )
                .unwrap_or_else(|_| "[]".to_owned())
            );

            eprintln!(
                "[analytics derived variant query]\nrequest_id={}\nsql={}\nparams={}",
                request_id,
                variant_compiled.sql,
                safe_parameters_for_log(&variant_compiled.parameters)
            );
        }

        let variant_validated = validate_sql(&variant_compiled.sql)?;

        if variant_validated.parameter_count() != variant_compiled.parameters.len() {
            return Err(AgentError::InvalidPlan(
                "compiled variant query placeholder count did not match its parameters".to_owned(),
            ));
        }

        let mut variant_result = database
            .execute_deterministic(&variant_validated, &variant_compiled.parameters)
            .await?;

        db_duration_ms += variant_result.duration_ms;

        /*
         * Avoid redundant nesting when only one qualifying variant
         * is represented in the requested period.
         */

        if variant_result.row_count > 1 {
            let temporal_variant_expansion = plan.dimensions.iter().any(|dimension| {
                matches!(
                    dimension,
                    Dimension::Day | Dimension::Week | Dimension::Month
                )
            });

            if temporal_variant_expansion {
                let temporal_facts =
                    enrich_temporal_variant_contribution_facts(plan, &result, &mut variant_result)
                        .ok_or_else(|| {
                            AgentError::InvalidPlan(
                                "temporal variant contribution could not determine its time axis"
                                    .to_owned(),
                            )
                        })?;

                #[cfg(debug_assertions)]
                eprintln!(
                    "[analytics temporal variant contribution]\nrequest_id={}\nbuckets={:?}",
                    request_id, temporal_facts.buckets,
                );
            } else {
                /*
                 * Existing B3 snapshot behavior.
                 */
                let contribution_facts =
                    enrich_variant_contribution_facts(&result, &mut variant_result);

                #[cfg(debug_assertions)]
                eprintln!(
            "[analytics variant contribution]\nrequest_id={}\nvariant_count={}\nquantity_share_total_pct={:?}\nsales_value_share_total_pct={:?}",
            request_id,
            contribution_facts.variant_count,
            contribution_facts.quantity_share_total_pct,
            contribution_facts.sales_value_share_total_pct,
        );

                #[cfg(debug_assertions)]
                eprintln!(
            "[analytics variant drivers]\nrequest_id={}\nquantity={:?}\nsales_value={:?}\norder_penetration={:?}\nbuyer_penetration={:?}",
            request_id,
            contribution_facts.top_quantity_variant,
            contribution_facts.top_sales_value_variant,
            contribution_facts.top_order_penetration_variant,
            contribution_facts.top_buyer_penetration_variant,
        );

                variant_contribution_facts = Some(contribution_facts);
            }

            variant_execution = Some((variant_plan, variant_compiled, variant_result));
        }
    }

    let calculation_started = Instant::now();

    apply_period_labels(&mut result, &compiled);

    if let Some((_, variant_compiled, variant_result)) = variant_execution.as_mut() {
        apply_period_labels(variant_result, variant_compiled);
    }

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

    let mut answer = summarize_metric_query_with_facts(plan, &result, analytical_facts.as_ref());

    if let Some(contribution_facts) = variant_contribution_facts.as_ref() {
        if let Some(driver_summary) = summarize_variant_contribution_facts(contribution_facts) {
            answer.push_str("\n\n");
            answer.push_str(&driver_summary);
        }
    }

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

    let mut expanded_rows = 0usize;

    if let Some((variant_plan, variant_compiled, variant_result)) = variant_execution {
        let variant_answer =
            summarize_metric_query_with_facts(&variant_plan, &variant_result, None);

        /*
         * Reuse the existing response/table presentation pipeline instead
         * of creating a second frontend-specific result shape.
         */
        let variant_response = AgentResponse::from_query(
            variant_answer.clone(),
            question,
            semantic_intent(&variant_plan),
            variant_compiled.expected_shape,
            &variant_result,
            None,
            None,
        );

        response.rename_primary_table("product-performance", "Product performance");

        if let Some(mut variant_table) = variant_response.tables.into_iter().next() {
            variant_table.id = "variant-performance".to_owned();

            variant_table.title = "Variant performance".to_owned();

            response.tables.push(variant_table);

            /*
             * Temporal product performance gets an explicit
             * multi-series variant chart.
             *
             * Single-period B3 snapshots remain table-only.
             */
            if let Some(x_key) = temporal_dimension_key(&variant_plan) {
                let chart_added = response.add_multi_series_line_chart(
                    "variant-performance-trend",
                    "variant-performance",
                    "Variant performance over time",
                    x_key,
                    "quantity_sold",
                    "variant_id",
                    "variant_title",
                    &[
                        "quantity_sold",
                        "product_sales_value",
                        "product_order_count",
                        "product_unique_customers",
                        "quantity_share_pct",
                        "sales_value_share_pct",
                        "order_penetration_pct",
                        "buyer_penetration_pct",
                    ],
                    true,
                );

                #[cfg(debug_assertions)]
                eprintln!(
            "[analytics variant chart]\nrequest_id={}\nadded={}\ndataset=variant-performance\nx_key={}\ny_key=quantity_sold\nseries_key=variant_id\nseries_label_key=variant_title\nzero_fill_missing_points=true",
            request_id,
            chart_added,
            x_key,
        );
            }
        }

        response.warnings.extend(variant_response.warnings);

        if !variant_answer.trim().is_empty() {
            response.answer.push_str("\n\nVariant breakdown: ");

            response.answer.push_str(variant_answer.trim());
        }

        expanded_rows = variant_result.row_count;

        #[cfg(debug_assertions)]
        eprintln!(
            "[analytics variant expansion]\nrequest_id={}\nvariant_rows={}",
            request_id, variant_result.row_count
        );
    }

    if let Some(message) = partial_bucket_warning(plan, partial_start_bucket, partial_end_bucket) {
        response.warnings.push(WarningBlock { message });
    }

    let summary_duration_ms = summary_started.elapsed().as_millis();

    #[cfg(debug_assertions)]
    eprintln!(
        "[analytics summary]\nrequest_id={}\nmode=deterministic\nmetrics={:?}\nrows={}\nvariant_rows={}",
        request_id,
        plan.metrics,
        result.row_count,
        expanded_rows
    );

    Ok(SemanticExecution {
        rows: result.row_count + expanded_rows,
        db_duration_ms,
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
    #[test]
    fn named_product_multi_metric_snapshot_derives_variant_plan() {
        let plan = AnalyticsPlan {
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
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters {
                exclude_cancelled: true,
                product: Some("MP Sharbati Atta".to_owned()),
                ..AnalyticsFilters::default()
            },
            sort: None,
            limit: None,
            visualization: Some(VisualizationIntent::Table),
        };

        let variant =
            derive_variant_expansion_plan(&plan).expect("named product performance should expand");

        assert_eq!(variant.metrics, plan.metrics);

        assert_eq!(variant.dimensions, vec![Dimension::Variant]);

        assert_eq!(variant.filters.product.as_deref(), Some("MP Sharbati Atta"));

        assert_eq!(variant.sort, Some(SortSpec::Descending));

        assert_eq!(variant.limit, None);

        assert_eq!(variant.visualization, Some(VisualizationIntent::Table));

        assert!(variant.validate().is_ok());
    }
    #[test]
    fn explicit_variant_plan_is_not_auto_expanded_again() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Products,
            metrics: vec![Metric::QuantitySold, Metric::ProductOrderCount],
            dimensions: vec![Dimension::Variant],
            periods: vec![TimePeriod {
                label: "July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters {
                product: Some("MP Sharbati Atta".to_owned()),
                ..AnalyticsFilters::default()
            },
            sort: Some(SortSpec::Descending),
            limit: None,
            visualization: Some(VisualizationIntent::Table),
        };

        assert!(derive_variant_expansion_plan(&plan).is_none());
    }
    #[test]
    fn monthly_product_performance_derives_month_variant_plan() {
        use crate::analytics::plan::{AnalyticsDomain, AnalyticsFilters, SortSpec, TimePeriod};
        use chrono::NaiveDate;

        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Products,
            metrics: vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ],
            dimensions: vec![Dimension::Month],
            periods: vec![TimePeriod {
                label: "January through July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters {
                product: Some("MP Sharbati Atta".to_owned()),
                exclude_cancelled: true,
                ..AnalyticsFilters::default()
            },
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Line),
        };

        let variant = derive_variant_expansion_plan(&plan)
            .expect("monthly product performance should expand");

        assert_eq!(
            variant.dimensions,
            vec![Dimension::Month, Dimension::Variant,]
        );

        assert_eq!(variant.sort, Some(SortSpec::Chronological));

        assert_eq!(variant.limit, None);

        assert_eq!(variant.visualization, Some(VisualizationIntent::Table));

        assert!(variant.validate().is_ok());
    }
}
