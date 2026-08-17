use super::{
    compiler::{compile_with_product_id, compile_with_product_ids, CompiledAnalyticsQuery},
    facts::{
        build_temporal_variant_mix_shift_facts, build_time_series_facts,
        derive_temporal_variant_mix_shift_driver_facts, enrich_temporal_variant_contribution_facts,
        enrich_variant_contribution_facts,
    },
    insights::{
        summarize_metric_query_with_facts, summarize_temporal_variant_mix_shift_driver_facts,
        summarize_variant_contribution_facts,
    },
    plan::{AnalyticsDomain, AnalyticsPlan, Dimension, Metric, SortSpec, VisualizationIntent},
    product_resolver::{
        resolve_product, resolve_products, MultiProductResolution, ProductResolution,
        ResolvedProduct,
    },
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

fn product_comparison_metric_key(metric: Metric) -> Option<&'static str> {
    match metric {
        Metric::QuantitySold => Some("quantity_sold"),
        Metric::ProductOrderCount => Some("product_order_count"),
        Metric::ProductUniqueCustomers => Some("product_unique_customers"),
        Metric::ProductSalesValue => Some("product_sales_value"),
        _ => None,
    }
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

fn derive_multi_product_variant_expansion_plan(
    plan: &AnalyticsPlan,
    product_mention: &str,
) -> Option<(AnalyticsPlan, AnalyticsPlan)> {
    if plan.domain != AnalyticsDomain::Products {
        return None;
    }

    if plan.filters.products.len() < 2 {
        return None;
    }

    if !plan.dimensions.contains(&Dimension::Product) {
        return None;
    }

    if plan.dimensions.contains(&Dimension::Variant) {
        return None;
    }

    let product_mention = product_mention.trim();

    if product_mention.is_empty() {
        return None;
    }

    /*
     * Convert one member of the B5 selected-product comparison back into
     * the established B4 single-product shape.
     *
     * Example:
     *
     *   B5:
     *     dimensions=[month, product]
     *     products=[Sharbati, Khapli]
     *
     * becomes:
     *
     *   parent:
     *     dimensions=[month]
     *     product=Sharbati
     *
     * and then existing B4 derivation produces:
     *
     *   variant:
     *     dimensions=[month, variant]
     *     product=Sharbati
     */
    let mut parent_plan = plan.clone();

    parent_plan.filters.products.clear();
    parent_plan.filters.product = Some(product_mention.to_owned());

    parent_plan
        .dimensions
        .retain(|dimension| *dimension != Dimension::Product);

    parent_plan.limit = None;

    let variant_plan = derive_variant_expansion_plan(&parent_plan)?;

    Some((parent_plan, variant_plan))
}

fn product_result_slice(result: &QueryResult, product_id: &str) -> QueryResult {
    let rows = result
        .rows
        .iter()
        .filter(|row| row.get("product_id").and_then(Value::as_str) == Some(product_id))
        .cloned()
        .collect::<Vec<_>>();

    QueryResult {
        columns: result.columns.clone(),
        row_count: rows.len(),
        rows,
        truncated: result.truncated,
        duration_ms: 0,
        data_as_of: result.data_as_of.clone(),
    }
}

fn attach_parent_product_identity(result: &mut QueryResult, product_id: &str, product_name: &str) {
    for row in &mut result.rows {
        row.insert(
            "parent_product_id".to_owned(),
            Value::String(product_id.to_owned()),
        );

        row.insert(
            "parent_product_name".to_owned(),
            Value::String(product_name.to_owned()),
        );
    }

    result
        .columns
        .retain(|column| column != "parent_product_id" && column != "parent_product_name");

    /*
     * Keep the temporal dimension first when present.
     */
    let insertion_index = match result.columns.first().map(String::as_str) {
        Some("day" | "week" | "month" | "period") => 1,
        _ => 0,
    };

    result
        .columns
        .insert(insertion_index, "parent_product_name".to_owned());

    result
        .columns
        .insert(insertion_index + 1, "parent_product_id".to_owned());
}

fn append_query_result(
    target: &mut QueryResult,
    mut source: QueryResult,
) -> Result<(), AgentError> {
    if target.columns != source.columns {
        return Err(AgentError::InvalidPlan(
            "multi-product variant expansion produced incompatible result columns".to_owned(),
        ));
    }

    target.truncated |= source.truncated;
    target.duration_ms += source.duration_ms;

    if target.data_as_of.is_none() {
        target.data_as_of = source.data_as_of.take();
    }

    target.rows.append(&mut source.rows);
    target.row_count = target.rows.len();

    Ok(())
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

    let mut resolved_product = None;
    let mut resolved_product_ids = Vec::<String>::new();

    /*
     * B5 keeps enough canonical identity to run and later group each
     * product's derived variant analysis.
     *
     * tuple:
     *   (original mention, canonical product id, canonical product name)
     */
    let mut resolved_multi_products = Vec::<(String, String, String)>::new();

    if plan.domain == AnalyticsDomain::Products {
        /*
         * B5 explicit multi-product comparison.
         *
         * Resolve the complete comparison set in one catalogue lookup.
         * No analytical query executes unless every mention resolves
         * successfully to a distinct canonical product.
         */
        if !plan.filters.products.is_empty() {
            let lookup = resolve_products(database, &plan.filters.products).await?;

            resolver_db_duration_ms = lookup.db_duration_ms;

            match lookup.resolution {
                MultiProductResolution::Resolved(products) => {
                    #[cfg(debug_assertions)]
                    for resolved in &products {
                        eprintln!(
                        "[analytics product resolution]\nrequest_id={}\nmention={:?}\nproduct_id={}\nproduct_name={:?}\nscore={:.3}",
                        request_id,
                        resolved.mention,
                        resolved.product.product_id,
                        resolved.product.product_name,
                        resolved.product.score
                    );
                    }

                    /*
                     * Preserve mention order.
                     *
                     * compile_with_product_ids() accepts only canonical IDs;
                     * user-entered product strings never reach the analytical
                     * SQL filter.
                     */
                    for resolved in products {
                        let mention = resolved.mention;
                        let product_id = resolved.product.product_id;
                        let product_name = resolved.product.product_name;

                        resolved_product_ids.push(product_id.clone());

                        resolved_multi_products.push((mention, product_id, product_name));
                    }
                }

                MultiProductResolution::Ambiguous {
                    mention,
                    candidates,
                } => {
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

                    return Ok(SemanticExecution {
                    response: AgentResponse::text(format!(
                        "I found multiple published products matching \"{mention}\": {choices}. Please specify which one you mean."
                    )),
                    rows: 0,
                    db_duration_ms: resolver_db_duration_ms,
                    calculation_duration_ms: 0,
                    summary_duration_ms: 0,
                });
                }

                MultiProductResolution::NotFound { mention } => {
                    #[cfg(debug_assertions)]
                    eprintln!(
                    "[analytics product resolution]\nrequest_id={}\nmention={:?}\nresult=not_found",
                    request_id,
                    mention
                );

                    return Ok(SemanticExecution {
                        response: AgentResponse::text(format!(
                        "I couldn't confidently match \"{mention}\" to a current published product."
                    )),
                        rows: 0,
                        db_duration_ms: resolver_db_duration_ms,
                        calculation_duration_ms: 0,
                        summary_duration_ms: 0,
                    });
                }

                MultiProductResolution::DuplicateResolvedProduct {
                    first_mention,
                    duplicate_mention,
                    product,
                } => {
                    #[cfg(debug_assertions)]
                    eprintln!(
                    "[analytics product resolution]\nrequest_id={}\nfirst_mention={:?}\nduplicate_mention={:?}\nresult=duplicate\nproduct_id={}\nproduct_name={:?}",
                    request_id,
                    first_mention,
                    duplicate_mention,
                    product.product_id,
                    product.product_name
                );

                    return Ok(SemanticExecution {
                    response: AgentResponse::text(format!(
                        "\"{first_mention}\" and \"{duplicate_mention}\" both match the published product \"{}\". Please compare distinct products.",
                        product.product_name
                    )),
                    rows: 0,
                    db_duration_ms: resolver_db_duration_ms,
                    calculation_duration_ms: 0,
                    summary_duration_ms: 0,
                });
                }
            }
        } else if let Some(mention) = plan.filters.product.as_deref() {
            /*
             * Existing B3/B4 single-product path.
             */
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

                    resolved_product = Some(product);
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

                    return Ok(SemanticExecution {
                    response: AgentResponse::text(format!(
                        "I found multiple published products matching \"{mention}\": {choices}. Please specify which one you mean."
                    )),
                    rows: 0,
                    db_duration_ms: resolver_db_duration_ms,
                    calculation_duration_ms: 0,
                    summary_duration_ms: 0,
                });
                }

                ProductResolution::NotFound => {
                    #[cfg(debug_assertions)]
                    eprintln!(
                    "[analytics product resolution]\nrequest_id={}\nmention={:?}\nresult=not_found",
                    request_id,
                    mention
                );

                    return Ok(SemanticExecution {
                        response: AgentResponse::text(format!(
                        "I couldn't confidently match \"{mention}\" to a current published product."
                    )),
                        rows: 0,
                        db_duration_ms: resolver_db_duration_ms,
                        calculation_duration_ms: 0,
                        summary_duration_ms: 0,
                    });
                }
            }
        }
    }

    /*
     * Compile using canonical product identity.
     *
     * Single product:
     *     p.id = $N
     *
     * Multi-product B5:
     *     p.id IN ($N, $N+1, ...)
     */
    let compiled = if !plan.filters.products.is_empty() {
        let resolved_product_id_refs = resolved_product_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();

        compile_with_product_ids(plan, &resolved_product_id_refs)
    } else {
        compile_with_product_id(
            plan,
            resolved_product
                .as_ref()
                .map(|product| product.product_id.as_str()),
        )
    }
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
    let mut temporal_variant_driver_facts = None;

    /*
     * B5 variant comparison is deliberately separate from the established
     * B3/B4 automatic variant expansion.
     */
    let mut multi_product_variant_execution: Option<(
        AnalyticsPlan,
        CompiledAnalyticsQuery,
        QueryResult,
    )> = None;

    let mut multi_product_variant_summaries = Vec::<String>::new();

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

                let mix_shift_facts =
                    build_temporal_variant_mix_shift_facts(plan, &result, &variant_result)
                        .ok_or_else(|| {
                            AgentError::InvalidPlan(
                                "temporal variant mix shift could not determine its time axis"
                                    .to_owned(),
                            )
                        })?;

                #[cfg(debug_assertions)]
                {
                    let variant_fact_count = mix_shift_facts
                        .transitions
                        .iter()
                        .map(|transition| transition.variants.len())
                        .sum::<usize>();

                    eprintln!(
        "[analytics temporal variant mix shift]\nrequest_id={}\nbucket_transitions={}\nvariant_transition_facts={}",
        request_id,
        mix_shift_facts.transitions.len(),
        variant_fact_count,
    );
                }

                /*
                 * B4-B computes deterministic transition facts.
                 * B4-C will consume them for gain/loss summaries.
                 */
                let mix_shift_driver_facts =
                    derive_temporal_variant_mix_shift_driver_facts(&mix_shift_facts);

                #[cfg(debug_assertions)]
                if let Some(latest) = mix_shift_driver_facts.transitions.last() {
                    eprintln!(
        "[analytics temporal variant drivers]\nrequest_id={}\nfrom_bucket={}\nto_bucket={}\nquantity_gain={:?}\nquantity_loss={:?}\nsales_gain={:?}\nsales_loss={:?}\norder_penetration_gain={:?}\norder_penetration_loss={:?}\nbuyer_penetration_gain={:?}\nbuyer_penetration_loss={:?}",
        request_id,
        latest.from_bucket,
        latest.to_bucket,
        latest.largest_quantity_mix_gain,
        latest.largest_quantity_mix_loss,
        latest.largest_sales_mix_gain,
        latest.largest_sales_mix_loss,
        latest.largest_order_penetration_gain,
        latest.largest_order_penetration_loss,
        latest.largest_buyer_penetration_gain,
        latest.largest_buyer_penetration_loss,
    );
                }

                temporal_variant_driver_facts = Some(mix_shift_driver_facts);

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

    /*
     * B5 selected-product variant expansion.
     *
     * Every selected parent product is analyzed independently using the
     * already-proven B4 variant machinery. The enriched rows are merged
     * only after their within-parent percentages have been calculated.
     */
    if !plan.filters.products.is_empty() && plan.dimensions.contains(&Dimension::Product) {
        for (mention, product_id, product_name) in &resolved_multi_products {
            let Some((parent_plan, variant_plan)) =
                derive_multi_product_variant_expansion_plan(plan, mention)
            else {
                continue;
            };

            parent_plan
                .validate()
                .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

            variant_plan
                .validate()
                .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

            /*
             * Pull only this product's rows from the already-executed B5
             * parent query.
             */
            let parent_result = product_result_slice(&result, product_id);

            /*
             * No qualifying parent activity means there cannot be a useful
             * qualifying variant decomposition for this product.
             */
            if parent_result.row_count == 0 {
                #[cfg(debug_assertions)]
                eprintln!(
                "[analytics multi-product variant expansion]\nrequest_id={}\nproduct_id={}\nproduct_name={:?}\nresult=no_parent_rows",
                request_id,
                product_id,
                product_name,
            );

                continue;
            }

            let variant_compiled =
                compile_with_product_id(&variant_plan, Some(product_id.as_str()))
                    .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

            #[cfg(debug_assertions)]
            {
                eprintln!(
                "[analytics multi-product derived variant plan]\nrequest_id={}\nproduct_id={}\nproduct_name={:?}\nmetrics={:?}\ndimensions={}\nperiods={}",
                request_id,
                product_id,
                product_name,
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
                                period.end_exclusive,
                            )
                        })
                        .collect::<Vec<_>>()
                )
                .unwrap_or_else(|_| "[]".to_owned()),
            );

                eprintln!(
                "[analytics multi-product derived variant query]\nrequest_id={}\nproduct_id={}\nsql={}\nparams={}",
                request_id,
                product_id,
                variant_compiled.sql,
                safe_parameters_for_log(&variant_compiled.parameters),
            );
            }

            let variant_validated = validate_sql(&variant_compiled.sql)?;

            if variant_validated.parameter_count() != variant_compiled.parameters.len() {
                return Err(AgentError::InvalidPlan(
                "compiled multi-product variant query placeholder count did not match its parameters"
                    .to_owned(),
            ));
            }

            let mut variant_result = database
                .execute_deterministic(&variant_validated, &variant_compiled.parameters)
                .await?;

            db_duration_ms += variant_result.duration_ms;

            /*
             * Apply compiled period labels before results from different
             * products are merged.
             */
            apply_period_labels(&mut variant_result, &variant_compiled);

            if variant_result.row_count == 0 {
                continue;
            }

            let temporal_variant_expansion = temporal_dimension_key(&parent_plan).is_some();

            if temporal_variant_expansion {
                let temporal_facts =
                enrich_temporal_variant_contribution_facts(
                    &parent_plan,
                    &parent_result,
                    &mut variant_result,
                )
                .ok_or_else(|| {
                    AgentError::InvalidPlan(
                        "multi-product temporal variant contribution could not determine its time axis"
                            .to_owned(),
                    )
                })?;

                let mix_shift_facts = build_temporal_variant_mix_shift_facts(
                    &parent_plan,
                    &parent_result,
                    &variant_result,
                )
                .ok_or_else(|| {
                    AgentError::InvalidPlan(
                        "multi-product temporal variant mix shift could not determine its time axis"
                            .to_owned(),
                    )
                })?;

                let driver_facts = derive_temporal_variant_mix_shift_driver_facts(&mix_shift_facts);

                if let Some(driver_summary) =
                    summarize_temporal_variant_mix_shift_driver_facts(&driver_facts)
                {
                    multi_product_variant_summaries
                        .push(format!("{product_name}: {driver_summary}"));
                }

                #[cfg(debug_assertions)]
                {
                    let variant_fact_count = mix_shift_facts
                        .transitions
                        .iter()
                        .map(|transition| transition.variants.len())
                        .sum::<usize>();

                    eprintln!(
                    "[analytics multi-product temporal variant mix shift]\nrequest_id={}\nproduct_id={}\nproduct_name={:?}\nbucket_transitions={}\nvariant_transition_facts={}\nbuckets={:?}",
                    request_id,
                    product_id,
                    product_name,
                    mix_shift_facts.transitions.len(),
                    variant_fact_count,
                    temporal_facts.buckets,
                );

                    if let Some(latest) = driver_facts.transitions.last() {
                        eprintln!(
                        "[analytics multi-product temporal variant drivers]\nrequest_id={}\nproduct_id={}\nproduct_name={:?}\nfrom_bucket={}\nto_bucket={}\nquantity_gain={:?}\nquantity_loss={:?}\nsales_gain={:?}\nsales_loss={:?}\norder_penetration_gain={:?}\norder_penetration_loss={:?}\nbuyer_penetration_gain={:?}\nbuyer_penetration_loss={:?}",
                        request_id,
                        product_id,
                        product_name,
                        latest.from_bucket,
                        latest.to_bucket,
                        latest.largest_quantity_mix_gain,
                        latest.largest_quantity_mix_loss,
                        latest.largest_sales_mix_gain,
                        latest.largest_sales_mix_loss,
                        latest.largest_order_penetration_gain,
                        latest.largest_order_penetration_loss,
                        latest.largest_buyer_penetration_gain,
                        latest.largest_buyer_penetration_loss,
                    );
                    }
                }
            } else {
                /*
                 * Single-period B5 comparison.
                 *
                 * Use the existing B3 contribution calculation independently
                 * for this parent product.
                 */
                let contribution_facts =
                    enrich_variant_contribution_facts(&parent_result, &mut variant_result);

                if let Some(driver_summary) =
                    summarize_variant_contribution_facts(&contribution_facts)
                {
                    multi_product_variant_summaries
                        .push(format!("{product_name}: {driver_summary}"));
                }

                #[cfg(debug_assertions)]
                eprintln!(
                "[analytics multi-product variant contribution]\nrequest_id={}\nproduct_id={}\nproduct_name={:?}\nvariant_count={}\nquantity_share_total_pct={:?}\nsales_value_share_total_pct={:?}",
                request_id,
                product_id,
                product_name,
                contribution_facts.variant_count,
                contribution_facts.quantity_share_total_pct,
                contribution_facts.sales_value_share_total_pct,
            );
            }

            /*
             * Parent identity is metadata on variant grain.
             *
             * It is NOT a third analytical grouping dimension.
             */
            attach_parent_product_identity(&mut variant_result, product_id, product_name);

            if let Some((_, _, combined_variant_result)) = multi_product_variant_execution.as_mut()
            {
                append_query_result(combined_variant_result, variant_result)?;
            } else {
                multi_product_variant_execution =
                    Some((variant_plan, variant_compiled, variant_result));
            }
        }

        #[cfg(debug_assertions)]
        if let Some((_, _, variant_result)) = multi_product_variant_execution.as_ref() {
            eprintln!(
            "[analytics multi-product variant expansion]\nrequest_id={}\nproduct_count={}\nvariant_rows={}",
            request_id,
            resolved_multi_products.len(),
            variant_result.row_count,
        );
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

    if let Some(driver_facts) = temporal_variant_driver_facts.as_ref() {
        if let Some(driver_summary) =
            summarize_temporal_variant_mix_shift_driver_facts(driver_facts)
        {
            answer.push_str("\n\n");
            answer.push_str(&driver_summary);
        }
    }

    if !multi_product_variant_summaries.is_empty() {
        answer.push_str("\n\nVariant drivers by product:");

        for summary in &multi_product_variant_summaries {
            answer.push_str("\n\n");
            answer.push_str(summary);
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

    /*
     * B5 temporal selected-product comparison.
     *
     * The primary query is already Month/Week/Day × Product.
     * Reuse the same interactive multi-series chart contract used by
     * temporal variant performance:
     *
     *   series = product_id
     *   label  = product_name
     *   metric = user-switchable core product metric
     */
    if !plan.filters.products.is_empty() && plan.dimensions.contains(&Dimension::Product) {
        if let Some(x_key) = temporal_dimension_key(plan) {
            let metric_keys = plan
                .metrics
                .iter()
                .copied()
                .filter_map(product_comparison_metric_key)
                .collect::<Vec<_>>();

            if let Some(default_y_key) = metric_keys.first().copied() {
                /*
                 * Give this result a stable dataset identity before creating
                 * the multi-series chart.
                 *
                 * rename_primary_table() also retargets any chart already
                 * created by AgentResponse::from_query().
                 */
                response.rename_primary_table("product-comparison", "Product comparison");

                /*
                 * from_query() may already have created a generic line chart.
                 * Replace it with the richer multi-series version, but restore
                 * the generic chart if the richer chart cannot be constructed.
                 */
                let fallback_charts = std::mem::take(&mut response.charts);

                let chart_added = response.add_multi_series_line_chart(
                    "product-comparison-trend",
                    "product-comparison",
                    "Product performance over time",
                    x_key,
                    default_y_key,
                    "product_id",
                    "product_name",
                    &metric_keys,
                    true,
                );

                if !chart_added {
                    response.charts = fallback_charts;
                }

                #[cfg(debug_assertions)]
                eprintln!(
                "[analytics product comparison chart]\nrequest_id={}\nadded={}\ndataset=product-comparison\nx_key={}\ny_key={}\nseries_key=product_id\nseries_label_key=product_name\nmetrics={:?}\nzero_fill_missing_points=true",
                request_id,
                chart_added,
                x_key,
                default_y_key,
                metric_keys,
            );
            }
        }
    }

    let mut expanded_rows = 0usize;

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

    /*
     * B5 combined variant dataset.
     *
     * The dataset contains variants from every selected parent product,
     * with parent_product_id/name available to the frontend for the later
     * product → variant selector UX.
     */
    if let Some((variant_plan, variant_compiled, variant_result)) = multi_product_variant_execution
    {
        let variant_response = AgentResponse::from_query(
            String::new(),
            question,
            "selected product variant comparison",
            variant_compiled.expected_shape,
            &variant_result,
            None,
            None,
        );

        if let Some(mut variant_table) = variant_response.tables.into_iter().next() {
            variant_table.id = "variant-comparison".to_owned();

            variant_table.title = "Variant comparison".to_owned();

            response.tables.push(variant_table);

            /*
             * Temporal B5 gets one switchable multi-series variant chart.
             *
             * The frontend will later use parent_product_id/name to limit
             * visible variants rather than rendering every available line.
             */
            if let Some(x_key) = temporal_dimension_key(&variant_plan) {
                let default_y_key = plan
                    .metrics
                    .iter()
                    .copied()
                    .filter_map(product_comparison_metric_key)
                    .next()
                    .unwrap_or("quantity_sold");

                let chart_added = response.add_multi_series_line_chart(
                    "variant-comparison-trend",
                    "variant-comparison",
                    "Variant performance across selected products",
                    x_key,
                    default_y_key,
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
                "[analytics multi-product variant chart]\nrequest_id={}\nadded={}\ndataset=variant-comparison\nx_key={}\ny_key={}\nseries_key=variant_id\nseries_label_key=variant_title\nzero_fill_missing_points=true",
                request_id,
                chart_added,
                x_key,
                default_y_key,
            );
            }
        }

        response.warnings.extend(variant_response.warnings);

        expanded_rows += variant_result.row_count;

        #[cfg(debug_assertions)]
        eprintln!(
            "[analytics multi-product variant response]\nrequest_id={}\nvariant_rows={}",
            request_id, variant_result.row_count,
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

    #[test]
    fn monthly_multi_product_comparison_derives_single_parent_variant_plan() {
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

            dimensions: vec![Dimension::Month, Dimension::Product],

            periods: vec![TimePeriod {
                label: "January through July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],

            filters: AnalyticsFilters {
                products: vec!["Sharbati Atta".to_owned(), "Khapli Emmer".to_owned()],
                exclude_cancelled: true,
                ..AnalyticsFilters::default()
            },

            sort: Some(SortSpec::Chronological),

            limit: None,

            visualization: Some(VisualizationIntent::Line),
        };

        let (parent, variant) = derive_multi_product_variant_expansion_plan(&plan, "Sharbati Atta")
            .expect("selected B5 product should derive B4 variant analysis");

        assert_eq!(parent.filters.product.as_deref(), Some("Sharbati Atta"),);

        assert!(parent.filters.products.is_empty());

        assert_eq!(parent.dimensions, vec![Dimension::Month],);

        assert_eq!(variant.filters.product.as_deref(), Some("Sharbati Atta"),);

        assert!(variant.filters.products.is_empty());

        assert_eq!(
            variant.dimensions,
            vec![Dimension::Month, Dimension::Variant,],
        );

        assert_eq!(variant.metrics, plan.metrics);
        assert_eq!(variant.limit, None);

        assert!(parent.validate().is_ok());
        assert!(variant.validate().is_ok());
    }
}
