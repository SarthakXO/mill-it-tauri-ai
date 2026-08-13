use super::{
    plan::{AnalyticsPlan, Dimension, Metric, SortSpec, StatusDimension},
    query::{PARTIAL_CANCEL_QUANTITY_JOIN, QUALIFYING_SALE_FILTER},
};
use crate::database::QueryParameter;
use std::{error::Error, fmt};

const BUSINESS_DATE: &str = "timezone('Asia/Kolkata', o.created_at)";

#[derive(Clone, Debug)]
pub(crate) struct CompiledAnalyticsQuery {
    pub(crate) sql: String,
    pub(crate) parameters: Vec<QueryParameter>,
    pub(crate) expected_shape: &'static str,
    pub(crate) period_labels: Vec<String>,
}

pub(crate) fn compile(plan: &AnalyticsPlan) -> Result<CompiledAnalyticsQuery, CompileError> {
    plan.validate()
        .map_err(|error| CompileError(error.to_string()))?;
    let mut parameters = period_parameters(plan);
    let period_case = period_case(plan);
    let range_filter = range_filter(plan);
    let date_dimension = date_dimension(plan);
    let period_dimension = plan.dimensions.contains(&Dimension::Period);
    let status_dimension = plan
        .dimensions
        .iter()
        .copied()
        .find_map(status_for_dimension);
    let store_dimension = plan.dimensions.contains(&Dimension::Store);
    let variant_dimension = plan.dimensions.contains(&Dimension::Variant);
    let product_dimension = plan.dimensions.contains(&Dimension::Product);

    let needs_order_items = plan.metrics.iter().any(|metric| {
        matches!(
            metric,
            Metric::OrderValue | Metric::AverageOrderValue | Metric::AverageItemQuantityPerOrder
        )
    });
    let product_metric = plan
        .metrics
        .iter()
        .any(|metric| matches!(metric, Metric::QuantitySold | Metric::UniqueVariants));
    let mut joins = if product_metric {
        format!(" JOIN line_item AS li ON li.order_id = o.id{PARTIAL_CANCEL_QUANTITY_JOIN}")
    } else if needs_order_items {
        " LEFT JOIN (SELECT items.order_id, COALESCE(GREATEST(payments.placed_order_value - COALESCE(partial_refunds.processed_partial_refund, 0), 0), items.computed_item_value) AS qualifying_order_value, items.item_quantity FROM (SELECT li.order_id, SUM(li.unit_price::bigint * GREATEST(li.quantity::bigint - COALESCE(pcq.partial_cancelled_qty, 0), 0)) AS computed_item_value, SUM(GREATEST(li.quantity::bigint - COALESCE(pcq.partial_cancelled_qty, 0), 0)) AS item_quantity FROM line_item AS li LEFT JOIN (SELECT line_item_id, SUM(cancelled_quantity)::bigint AS partial_cancelled_qty FROM partial_order_cancellation_item GROUP BY line_item_id) AS pcq ON pcq.line_item_id = li.id WHERE li.order_id IS NOT NULL AND li.is_return = false GROUP BY li.order_id) AS items LEFT JOIN (SELECT DISTINCT ON (order_id) order_id, amount::bigint AS placed_order_value FROM payment WHERE order_id IS NOT NULL AND canceled_at IS NULL AND lower(COALESCE(status, '')) NOT IN ('canceled', 'cancelled') ORDER BY order_id, updated_at DESC, created_at DESC, id DESC) AS payments ON payments.order_id = items.order_id LEFT JOIN (SELECT order_id, SUM(refund_amount)::bigint AS processed_partial_refund FROM refund_request WHERE lower(status) = 'processed' AND partial_cancellation_id IS NOT NULL GROUP BY order_id) AS partial_refunds ON partial_refunds.order_id = items.order_id) AS oi ON oi.order_id = o.id".to_owned()
    } else {
        String::new()
    };
    let needs_variant_join = variant_dimension
        || product_dimension
        || plan.filters.product.is_some()
        || plan.filters.variant_id.is_some();
    if needs_variant_join {
        joins.push_str(" LEFT JOIN product_variant AS pv ON pv.id = li.variant_id LEFT JOIN product AS p ON p.id = pv.product_id");
    }
    if plan.dimensions.contains(&Dimension::DeliveryStatus)
        || plan
            .filters
            .status_filters
            .iter()
            .any(|filter| filter.dimension == StatusDimension::Delivery)
    {
        joins.push_str(" LEFT JOIN (SELECT order_id, MAX(delivery_status) AS delivery_status FROM fulfillment WHERE order_id IS NOT NULL GROUP BY order_id) AS f ON f.order_id = o.id");
    }
    if store_dimension {
        joins.push_str(" LEFT JOIN store AS s ON s.id = o.store_id");
    }

    let mut where_parts = vec![format!("({range_filter})")];
    if plan.filters.exclude_cancelled
        || plan.metrics.iter().any(|metric| {
            matches!(
                metric,
                Metric::UniqueCustomers
                    | Metric::QuantitySold
                    | Metric::UniqueVariants
                    | Metric::OrderValue
                    | Metric::AverageOrderValue
                    | Metric::AverageItemQuantityPerOrder
            )
        })
    {
        where_parts.push(QUALIFYING_SALE_FILTER.to_owned());
    }
    if product_metric {
        where_parts.push("li.variant_id IS NOT NULL".to_owned());
    }
    if product_dimension {
        where_parts.push("pv.product_id IS NOT NULL".to_owned());
    }
    add_status_filters(
        &mut where_parts,
        &mut parameters,
        &plan.filters.status_filters,
    );
    add_string_filter(
        &mut where_parts,
        &mut parameters,
        plan.filters.store_id.as_deref(),
        "o.store_id =",
    );
    if let Some(product) = plan.filters.product.as_deref() {
        parameters.push(QueryParameter::String(product.to_owned()));
        where_parts.push(format!(
            "(lower(li.title) LIKE '%' || lower((${}::text)) || '%' OR lower(COALESCE(p.title, '')) LIKE '%' || lower((${}::text)) || '%')",
            parameters.len(),
            parameters.len()
        ));
    }
    add_string_filter(
        &mut where_parts,
        &mut parameters,
        plan.filters.variant_id.as_deref(),
        "li.variant_id =",
    );

    let metric_expressions = plan
        .metrics
        .iter()
        .map(|metric| metric_expression(*metric).to_owned())
        .collect::<Vec<_>>();
    let mut select = Vec::new();
    let mut group = Vec::new();
    if period_dimension {
        select.push(format!("{period_case} AS period_index"));
        group.push("period_index".to_owned());
    }
    if let Some((expression, alias)) = date_dimension {
        select.push(format!("{expression} AS {alias}"));
        group.push(alias.to_owned());
    }
    if let Some((expression, alias)) = status_dimension {
        select.push(format!("COALESCE({expression}, 'UNSPECIFIED') AS {alias}"));
        group.push(expression.to_owned());
    }
    if store_dimension {
        select.push("o.store_id AS store_id".to_owned());
        select.push("COALESCE(s.name, 'Unknown store') AS store_name".to_owned());
        group.extend(["o.store_id".to_owned(), "s.name".to_owned()]);
    }
    if variant_dimension {
        select.extend([
            "li.variant_id".to_owned(),
            "COALESCE(MAX(NULLIF(li.title, '')), li.variant_id) AS historical_product_label"
                .to_owned(),
        ]);
        group.push("li.variant_id".to_owned());
    } else if product_dimension {
        select.extend([
        "pv.product_id AS product_id".to_owned(),
        "COALESCE(MAX(NULLIF(p.title, '')), MAX(NULLIF(li.title, '')), pv.product_id) AS product_name"
            .to_owned(),
    ]);
        group.push("pv.product_id".to_owned());
    }
    select.extend(metric_expressions);

    let where_clause = where_parts.join(" AND ");
    let group_clause = (!group.is_empty()).then(|| format!(" GROUP BY {}", group.join(", ")));
    let ranking = variant_dimension || product_dimension;
    let per_period_ranking = ranking && period_dimension && plan.limit.is_some();
    let sql = if per_period_ranking {
        let partition = period_case.clone();
        let rank_alias = "semantic_rank";
        let mut ranked_select = select.clone();
        ranked_select.push(format!(
            "ROW_NUMBER() OVER (PARTITION BY {partition} ORDER BY {} DESC) AS {rank_alias}",
            metric_aggregate(plan.metrics[0])
        ));
        parameters.push(QueryParameter::Integer(plan.limit.unwrap_or(10) as i64));
        let limit_index = parameters.len();
        let outer_columns = select
            .iter()
            .map(|projection| projection_alias(projection))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "WITH semantic_ranked AS (SELECT {} FROM \"order\" AS o{} WHERE {}{} ) SELECT {} FROM semantic_ranked WHERE {rank_alias} <= ${limit_index} ORDER BY period_index, {rank_alias}",
            ranked_select.join(", "),
            joins,
            where_clause,
            group_clause.unwrap_or_default(),
            outer_columns,
        )
    } else {
        let order = order_clause(plan, date_dimension.map(|(_, alias)| alias), ranking);
        let mut sql = format!(
            "SELECT {} FROM \"order\" AS o{} WHERE {}{}{}",
            select.join(", "),
            joins,
            where_clause,
            group_clause.unwrap_or_default(),
            order
        );
        if let Some(limit) = plan.limit.filter(|_| ranking) {
            parameters.push(QueryParameter::Integer(limit as i64));
            sql.push_str(&format!(" LIMIT ${}", parameters.len()));
        }
        sql
    };

    Ok(CompiledAnalyticsQuery {
        sql,
        parameters,
        expected_shape: plan.expected_shape(),
        period_labels: plan
            .periods
            .iter()
            .map(|period| period.label.clone())
            .collect(),
    })
}

fn period_parameters(plan: &AnalyticsPlan) -> Vec<QueryParameter> {
    plan.periods
        .iter()
        .flat_map(|period| {
            [
                QueryParameter::String(period.start.to_string()),
                QueryParameter::String(period.end_exclusive.to_string()),
            ]
        })
        .collect()
}

fn range_filter(plan: &AnalyticsPlan) -> String {
    plan.periods
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let start = index * 2 + 1;
            let end = start + 1;
            format!(
                "({BUSINESS_DATE} >= ((${}::text)::date) AND {BUSINESS_DATE} < ((${}::text)::date))",
                start, end
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn period_case(plan: &AnalyticsPlan) -> String {
    let branches = plan
        .periods
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let start = index * 2 + 1;
            let end = start + 1;
            format!(
                "WHEN {BUSINESS_DATE} >= ((${}::text)::date) AND {BUSINESS_DATE} < ((${}::text)::date) THEN {}",
                start,
                end,
                index + 1
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("CASE {branches} END")
}

fn date_dimension(plan: &AnalyticsPlan) -> Option<(&'static str, &'static str)> {
    if plan.dimensions.contains(&Dimension::Day) {
        Some(("timezone('Asia/Kolkata', o.created_at)::date", "day"))
    } else if plan.dimensions.contains(&Dimension::Week) {
        Some((
            "date_trunc('week', timezone('Asia/Kolkata', o.created_at))::date",
            "week",
        ))
    } else if plan.dimensions.contains(&Dimension::Month) {
        Some((
            "date_trunc('month', timezone('Asia/Kolkata', o.created_at))::date",
            "month",
        ))
    } else {
        None
    }
}

fn add_string_filter(
    where_parts: &mut Vec<String>,
    parameters: &mut Vec<QueryParameter>,
    value: Option<&str>,
    sql_prefix: &str,
) {
    if let Some(value) = value {
        parameters.push(QueryParameter::String(value.to_owned()));
        where_parts.push(format!("{sql_prefix} (${}::text)", parameters.len()));
    }
}

fn order_clause(plan: &AnalyticsPlan, date_alias: Option<&str>, ranking: bool) -> String {
    if let Some(alias) = date_alias {
        return format!(" ORDER BY {alias}");
    }
    if plan.dimensions.contains(&Dimension::Period) && !ranking {
        if plan.dimensions.iter().any(Dimension::is_status) {
            return format!(
                " ORDER BY period_index, {} DESC",
                plan.metrics[0].value_key()
            );
        }
        return " ORDER BY period_index".to_owned();
    }
    if plan.dimensions.iter().any(Dimension::is_status) {
        return format!(" ORDER BY {} DESC", plan.metrics[0].value_key());
    }
    if ranking {
        let direction = match plan.sort.unwrap_or(SortSpec::Descending) {
            SortSpec::Ascending => "ASC",
            SortSpec::Chronological | SortSpec::Descending => "DESC",
        };
        return format!(" ORDER BY {} {direction}", plan.metrics[0].value_key());
    }
    String::new()
}

fn projection_alias(projection: &str) -> String {
    projection.rsplit_once(" AS ").map_or_else(
        || {
            projection
                .rsplit_once('.')
                .map_or(projection, |(_, column)| column)
                .to_owned()
        },
        |(_, alias)| alias.to_owned(),
    )
}

fn metric_aggregate(metric: Metric) -> &'static str {
    match metric {
        Metric::OrderCount => "COUNT(*)",
        Metric::UniqueCustomers => "COUNT(DISTINCT o.customer_id)",
        Metric::QuantitySold => "SUM(GREATEST(li.quantity::bigint - COALESCE(pcq.partial_cancelled_qty, 0), 0))",
        Metric::UniqueVariants => "COUNT(DISTINCT li.variant_id)",
        Metric::OrderValue => "SUM(COALESCE(oi.qualifying_order_value, 0)) / 100.0",
        Metric::AverageOrderValue => "AVG(COALESCE(oi.qualifying_order_value, 0)) / 100.0",
        Metric::AverageItemQuantityPerOrder => "AVG(COALESCE(oi.item_quantity, 0))",
        Metric::CancelledOrderCount => "COUNT(*) FILTER (WHERE (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED')))",
        Metric::CancellationRate => {
            "100.0 * COUNT(*) FILTER (WHERE (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED'))) / NULLIF(COUNT(*), 0)"
        }
    }
}

fn metric_expression(metric: Metric) -> &'static str {
    match metric {
        Metric::OrderCount => "COUNT(*) AS order_count",
        Metric::UniqueCustomers => "COUNT(DISTINCT o.customer_id) AS customer_count",
        Metric::QuantitySold => "SUM(GREATEST(li.quantity::bigint - COALESCE(pcq.partial_cancelled_qty, 0), 0)) AS quantity_sold",
        Metric::UniqueVariants => "COUNT(DISTINCT li.variant_id) AS variant_count",
        Metric::OrderValue => "SUM(COALESCE(oi.qualifying_order_value, 0)) / 100.0 AS order_value",
        Metric::AverageOrderValue => "AVG(COALESCE(oi.qualifying_order_value, 0)) / 100.0 AS average_order_value",
        Metric::AverageItemQuantityPerOrder => "AVG(COALESCE(oi.item_quantity, 0)) AS average_item_quantity_per_order",
        Metric::CancelledOrderCount => "COUNT(*) FILTER (WHERE (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED'))) AS cancelled_order_count",
        Metric::CancellationRate => "100.0 * COUNT(*) FILTER (WHERE (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED'))) / NULLIF(COUNT(*), 0) AS cancellation_rate",
    }
}

fn status_for_dimension(dimension: Dimension) -> Option<(&'static str, &'static str)> {
    match dimension {
        Dimension::OperationalStatus => Some(("o.zoho_order_status::text", "operational_status")),
        Dimension::OrderStatus => Some(("o.status::text", "order_status")),
        Dimension::FulfillmentStatus => Some(("o.fulfillment_status::text", "fulfillment_status")),
        Dimension::PaymentStatus => Some(("o.payment_status::text", "payment_status")),
        Dimension::DeliveryStatus => Some(("f.delivery_status", "delivery_status")),
        _ => None,
    }
}

fn status_column(dimension: StatusDimension) -> &'static str {
    match dimension {
        StatusDimension::Operational => "o.zoho_order_status::text",
        StatusDimension::Order => "o.status::text",
        StatusDimension::Fulfillment => "o.fulfillment_status::text",
        StatusDimension::Payment => "o.payment_status::text",
        StatusDimension::Delivery => "f.delivery_status",
    }
}

fn add_status_filters(
    where_parts: &mut Vec<String>,
    parameters: &mut Vec<QueryParameter>,
    filters: &[super::plan::StatusFilter],
) {
    for filter in filters {
        let placeholders = filter
            .values
            .iter()
            .map(|value| {
                parameters.push(QueryParameter::String(value.clone()));
                format!("(${}::text)", parameters.len())
            })
            .collect::<Vec<_>>()
            .join(", ");
        where_parts.push(format!(
            "{} {}IN ({placeholders})",
            status_column(filter.dimension),
            if filter.exclude { "NOT " } else { "" }
        ));
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CompileError(String);

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CompileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analytics::plan::{AnalyticsDomain, AnalyticsFilters, TimePeriod, VisualizationIntent},
        database::validate_sql,
    };
    use chrono::NaiveDate;

    fn period(label: &str, year: i32, month: u32) -> TimePeriod {
        let next = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        TimePeriod {
            label: label.to_owned(),
            start: NaiveDate::from_ymd_opt(year, month, 1).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(next.0, next.1, 1).unwrap(),
        }
    }

    fn base_plan(metric: Metric, domain: AnalyticsDomain) -> AnalyticsPlan {
        AnalyticsPlan {
            domain,
            metrics: vec![metric],
            dimensions: Vec::new(),
            periods: vec![period("May 2026", 2026, 5)],
            filters: AnalyticsFilters::default(),
            sort: None,
            limit: None,
            visualization: Some(VisualizationIntent::Bar),
        }
    }

    #[test]
    fn four_period_order_plan_compiles_to_one_valid_parameterized_select() {
        let mut plan = base_plan(Metric::OrderCount, AnalyticsDomain::Operations);
        plan.dimensions = vec![Dimension::Period];
        plan.periods = vec![
            period("April 2026", 2026, 4),
            period("May 2026", 2026, 5),
            period("June 2026", 2026, 6),
            period("July 2026", 2026, 7),
        ];
        let compiled = compile(&plan).unwrap();
        let validated = validate_sql(&compiled.sql).expect("compiler SQL must pass AST safety");
        assert_eq!(compiled.parameters.len(), 8);
        assert_eq!(validated.parameter_count(), 8);
        assert!(compiled.sql.contains("CASE WHEN"));
        assert!(!compiled.sql.contains("line_item"));
    }

    #[test]
    fn weekly_query_uses_ist_calendar_weeks_and_only_the_order_table() {
        let mut plan = base_plan(Metric::OrderCount, AnalyticsDomain::Operations);
        plan.dimensions = vec![Dimension::Week];
        plan.periods = vec![TimePeriod {
            label: "May through June 2026".to_owned(),
            start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
        }];
        let compiled = compile(&plan).unwrap();
        validate_sql(&compiled.sql).expect("weekly compiler SQL must pass AST safety");
        assert!(compiled.sql.contains("date_trunc('week'"));
        assert!(!compiled.sql.contains("line_item"));
        assert_eq!(compiled.parameters.len(), 2);
    }

    #[test]
    fn multi_period_variant_ranking_is_partitioned_per_period() {
        let mut plan = base_plan(Metric::QuantitySold, AnalyticsDomain::Products);
        plan.dimensions = vec![Dimension::Period, Dimension::Variant];
        plan.periods = vec![
            period("April 2026", 2026, 4),
            period("May 2026", 2026, 5),
            period("June 2026", 2026, 6),
        ];
        plan.limit = Some(10);
        plan.sort = Some(SortSpec::Descending);
        let compiled = compile(&plan).unwrap();
        validate_sql(&compiled.sql).expect("ranking compiler SQL must pass AST safety");
        assert!(compiled
            .sql
            .contains("ROW_NUMBER() OVER (PARTITION BY CASE"));
        assert!(compiled.sql.contains(QUALIFYING_SALE_FILTER));
        assert_eq!(compiled.parameters.len(), 7);
    }

    #[test]
    fn multi_metric_order_values_preaggregate_line_items_at_order_grain() {
        let mut plan = base_plan(Metric::OrderCount, AnalyticsDomain::Operations);
        plan.metrics = vec![
            Metric::OrderCount,
            Metric::OrderValue,
            Metric::AverageOrderValue,
        ];
        plan.dimensions = vec![Dimension::Month];
        let compiled = compile(&plan).unwrap();
        validate_sql(&compiled.sql).expect("multi-metric SQL must pass AST safety");
        assert!(compiled.sql.contains("GROUP BY li.order_id"));
        assert!(compiled.sql.contains("qualifying_order_value"));
        assert!(compiled.sql.contains("DISTINCT ON (order_id)"));
        assert!(compiled.sql.contains("lower(status) = 'processed'"));
        assert!(compiled
            .sql
            .contains("AVG(COALESCE(oi.qualifying_order_value"));
        assert!(compiled.sql.contains(QUALIFYING_SALE_FILTER));
    }

    #[test]
    fn payment_filter_uses_payment_status_not_operational_status() {
        let mut plan = base_plan(Metric::OrderCount, AnalyticsDomain::Operations);
        plan.filters
            .status_filters
            .push(super::super::plan::StatusFilter {
                dimension: super::super::plan::StatusDimension::Payment,
                values: vec!["awaiting".to_owned()],
                exclude: false,
            });
        let compiled = compile(&plan).unwrap();
        validate_sql(&compiled.sql).expect("status filter SQL must pass AST safety");
        assert!(compiled.sql.contains("o.payment_status::text IN"));
        assert!(!compiled.sql.contains("zoho_order_status"));
    }

    #[test]
    fn delivery_breakdown_uses_fulfillment_delivery_status_at_order_grain() {
        let mut plan = base_plan(Metric::OrderCount, AnalyticsDomain::Operations);
        plan.dimensions = vec![Dimension::DeliveryStatus];
        let compiled = compile(&plan).unwrap();
        validate_sql(&compiled.sql).expect("delivery SQL must pass AST safety");

        assert!(compiled
            .sql
            .contains("SELECT order_id, MAX(delivery_status) AS delivery_status FROM fulfillment"));
        assert!(compiled.sql.contains("f.delivery_status"));
        assert!(!compiled.sql.contains("o.delivery_status"));
    }
    #[test]
    fn product_ranking_aggregates_at_product_identity_grain() {
        let mut plan = base_plan(Metric::QuantitySold, AnalyticsDomain::Products);
        plan.dimensions = vec![Dimension::Product];
        plan.limit = Some(10);
        plan.sort = Some(SortSpec::Descending);

        let compiled = compile(&plan).unwrap();

        validate_sql(&compiled.sql).expect("product ranking SQL must pass AST safety");

        assert!(compiled.sql.contains("pv.product_id AS product_id"));
        assert!(compiled.sql.contains("pv.product_id IS NOT NULL"));

        let (_, grouping_tail) = compiled
            .sql
            .rsplit_once(" GROUP BY ")
            .expect("product ranking must group");

        let grouping = grouping_tail
            .split(" ORDER BY ")
            .next()
            .expect("grouping must precede ordering");

        assert_eq!(grouping.trim(), "pv.product_id");
        assert!(!grouping.contains("li.variant_id"));
        assert!(!grouping.contains("li.title"));
    }
    #[test]
    fn variant_ranking_aggregates_at_variant_identity_grain() {
        let mut plan = base_plan(Metric::QuantitySold, AnalyticsDomain::Products);
        plan.dimensions = vec![Dimension::Variant];
        plan.limit = Some(10);
        plan.sort = Some(SortSpec::Descending);

        let compiled = compile(&plan).unwrap();

        validate_sql(&compiled.sql).expect("variant ranking SQL must pass AST safety");

        assert!(compiled.sql.contains("li.variant_id"));
        assert!(compiled.sql.contains("MAX(NULLIF(li.title, ''))"));

        let (_, grouping_tail) = compiled
            .sql
            .rsplit_once(" GROUP BY ")
            .expect("variant ranking must group");

        let grouping = grouping_tail
            .split(" ORDER BY ")
            .next()
            .expect("grouping must precede ordering");

        assert_eq!(grouping.trim(), "li.variant_id");
        assert!(!grouping.contains("li.title"));
    }
    #[test]
    fn unique_customers_uses_qualifying_order_population() {
        let plan = base_plan(Metric::UniqueCustomers, AnalyticsDomain::Customers);

        let compiled = compile(&plan).unwrap();

        validate_sql(&compiled.sql).expect("unique customer SQL must pass AST safety");

        assert!(compiled
            .sql
            .contains("COUNT(DISTINCT o.customer_id) AS customer_count"));
        assert!(compiled.sql.contains(QUALIFYING_SALE_FILTER));
    }
    #[test]
    fn plain_order_count_does_not_implicitly_exclude_cancelled_orders() {
        let plan = base_plan(Metric::OrderCount, AnalyticsDomain::Operations);

        let compiled = compile(&plan).unwrap();

        validate_sql(&compiled.sql).expect("order count SQL must pass AST safety");

        assert!(compiled.sql.contains("COUNT(*) AS order_count"));
        assert!(!compiled.sql.contains(QUALIFYING_SALE_FILTER));
    }
}
