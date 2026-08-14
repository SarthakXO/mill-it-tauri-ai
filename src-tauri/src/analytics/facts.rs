use super::plan::{AnalyticsPlan, Dimension, Metric};
use crate::database::QueryResult;
use serde::Serialize;
use serde_json::{Map, Number, Value};

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct AnalyticalFacts {
    pub(crate) partial_start_bucket: bool,
    pub(crate) partial_end_bucket: bool,
    pub(crate) complete_bucket_count: usize,
    pub(crate) metrics: Vec<MetricFacts>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct MetricFacts {
    pub(crate) metric: String,
    pub(crate) first_complete: Option<LabeledValue>,
    pub(crate) last_complete: Option<LabeledValue>,
    pub(crate) peak_complete: Option<LabeledValue>,
    pub(crate) lowest_complete: Option<LabeledValue>,
    pub(crate) overall_change: Option<TransitionFact>,
    pub(crate) largest_increase: Option<TransitionFact>,
    pub(crate) largest_decline: Option<TransitionFact>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct LabeledValue {
    pub(crate) label: String,
    pub(crate) value: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct TransitionFact {
    pub(crate) from: LabeledValue,
    pub(crate) to: LabeledValue,
    pub(crate) absolute_change: f64,
    pub(crate) percent_change: Option<f64>,
}

pub(crate) fn build_time_series_facts(
    plan: &AnalyticsPlan,
    result: &QueryResult,
    partial_start_bucket: bool,
    partial_end_bucket: bool,
) -> Option<AnalyticalFacts> {
    let axis_key = temporal_axis_key(plan)?;

    if result.rows.is_empty() {
        return Some(AnalyticalFacts {
            partial_start_bucket,
            partial_end_bucket,
            complete_bucket_count: 0,
            metrics: Vec::new(),
        });
    }

    let start_index = usize::from(partial_start_bucket).min(result.rows.len());

    let mut end_index = result.rows.len();

    if partial_end_bucket && end_index > start_index {
        end_index -= 1;
    }

    let complete_rows = &result.rows[start_index..end_index];

    let metrics = plan
        .metrics
        .iter()
        .filter_map(|metric| metric_facts(*metric, axis_key, complete_rows))
        .collect();

    Some(AnalyticalFacts {
        partial_start_bucket,
        partial_end_bucket,
        complete_bucket_count: complete_rows.len(),
        metrics,
    })
}

const QUANTITY_SHARE_PCT: &str = "quantity_share_pct";
const SALES_VALUE_SHARE_PCT: &str = "sales_value_share_pct";
const ORDER_PENETRATION_PCT: &str = "order_penetration_pct";
const BUYER_PENETRATION_PCT: &str = "buyer_penetration_pct";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VariantContributionFacts {
    pub(crate) variant_count: usize,

    pub(crate) quantity_share_total_pct: Option<f64>,
    pub(crate) sales_value_share_total_pct: Option<f64>,

    pub(crate) top_quantity_variant: Option<VariantDriverFact>,
    pub(crate) top_sales_value_variant: Option<VariantDriverFact>,
    pub(crate) top_order_penetration_variant: Option<VariantDriverFact>,
    pub(crate) top_buyer_penetration_variant: Option<VariantDriverFact>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VariantDriverFact {
    pub(crate) variant_id: String,
    pub(crate) variant_title: String,
    pub(crate) percentage: f64,
}

pub(crate) fn enrich_variant_contribution_facts(
    parent: &QueryResult,
    variants: &mut QueryResult,
) -> VariantContributionFacts {
    let parent_row = parent.rows.first();

    let parent_quantity = parent_row.and_then(|row| row_number(row, "quantity_sold"));

    let parent_order_count = parent_row.and_then(|row| row_number(row, "product_order_count"));

    let parent_unique_customers =
        parent_row.and_then(|row| row_number(row, "product_unique_customers"));

    let parent_sales_value = parent_row.and_then(|row| row_number(row, "product_sales_value"));

    for row in &mut variants.rows {
        let quantity = row_number(row, "quantity_sold");
        let order_count = row_number(row, "product_order_count");
        let unique_customers = row_number(row, "product_unique_customers");
        let sales_value = row_number(row, "product_sales_value");

        insert_percentage(
            row,
            QUANTITY_SHARE_PCT,
            percentage(quantity, parent_quantity),
        );

        insert_percentage(
            row,
            ORDER_PENETRATION_PCT,
            percentage(order_count, parent_order_count),
        );

        insert_percentage(
            row,
            BUYER_PENETRATION_PCT,
            percentage(unique_customers, parent_unique_customers),
        );

        insert_percentage(
            row,
            SALES_VALUE_SHARE_PCT,
            percentage(sales_value, parent_sales_value),
        );
    }

    for column in [
        QUANTITY_SHARE_PCT,
        ORDER_PENETRATION_PCT,
        BUYER_PENETRATION_PCT,
        SALES_VALUE_SHARE_PCT,
    ] {
        if !variants.columns.iter().any(|existing| existing == column) {
            variants.columns.push(column.to_owned());
        }
    }

    VariantContributionFacts {
        variant_count: variants.row_count,

        quantity_share_total_pct: complete_percentage_total(&variants.rows, QUANTITY_SHARE_PCT),

        sales_value_share_total_pct: complete_percentage_total(
            &variants.rows,
            SALES_VALUE_SHARE_PCT,
        ),

        top_quantity_variant: top_variant_driver(&variants.rows, QUANTITY_SHARE_PCT),

        top_sales_value_variant: top_variant_driver(&variants.rows, SALES_VALUE_SHARE_PCT),

        top_order_penetration_variant: top_variant_driver(&variants.rows, ORDER_PENETRATION_PCT),

        top_buyer_penetration_variant: top_variant_driver(&variants.rows, BUYER_PENETRATION_PCT),
    }
}

fn row_number(row: &Map<String, Value>, key: &str) -> Option<f64> {
    row.get(key).and_then(Value::as_f64)
}

fn percentage(numerator: Option<f64>, denominator: Option<f64>) -> Option<f64> {
    let numerator = numerator?;
    let denominator = denominator?;

    if denominator <= f64::EPSILON {
        return None;
    }

    Some(numerator / denominator * 100.0)
}

fn insert_percentage(row: &mut Map<String, Value>, key: &str, percentage: Option<f64>) {
    let value = percentage
        .and_then(Number::from_f64)
        .map(Value::Number)
        .unwrap_or(Value::Null);

    row.insert(key.to_owned(), value);
}

fn complete_percentage_total(rows: &[Map<String, Value>], key: &str) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }

    rows.iter()
        .try_fold(0.0, |total, row| Some(total + row.get(key)?.as_f64()?))
}

fn top_variant_driver(
    rows: &[Map<String, Value>],
    percentage_key: &str,
) -> Option<VariantDriverFact> {
    let mut drivers = rows
        .iter()
        .filter_map(|row| {
            let percentage = row.get(percentage_key)?.as_f64()?;

            if !percentage.is_finite() {
                return None;
            }

            let variant_id = row.get("variant_id")?.as_str()?.trim();

            if variant_id.is_empty() {
                return None;
            }

            let variant_title = row
                .get("variant_title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(variant_id);

            Some(VariantDriverFact {
                variant_id: variant_id.to_owned(),
                variant_title: variant_title.to_owned(),
                percentage,
            })
        })
        .collect::<Vec<_>>();

    /*
     * Deterministic ordering:
     *
     * 1. highest percentage
     * 2. title alphabetically
     * 3. variant ID alphabetically
     *
     * The latter two only matter for exact ties.
     */
    drivers.sort_by(|left, right| {
        right
            .percentage
            .total_cmp(&left.percentage)
            .then_with(|| left.variant_title.cmp(&right.variant_title))
            .then_with(|| left.variant_id.cmp(&right.variant_id))
    });

    drivers.into_iter().next()
}

fn metric_facts(
    metric: Metric,
    axis_key: &str,
    rows: &[Map<String, Value>],
) -> Option<MetricFacts> {
    let points = rows
        .iter()
        .filter_map(|row| {
            let label = row.get(axis_key)?.as_str()?.to_owned();
            let value = row.get(metric.value_key())?.as_f64()?;

            Some(LabeledValue { label, value })
        })
        .collect::<Vec<_>>();

    if points.is_empty() {
        return None;
    }

    let first_complete = points.first().cloned();
    let last_complete = points.last().cloned();

    let peak_complete = points
        .iter()
        .max_by(|left, right| left.value.total_cmp(&right.value))
        .cloned();

    let lowest_complete = points
        .iter()
        .min_by(|left, right| left.value.total_cmp(&right.value))
        .cloned();

    let transitions = points
        .windows(2)
        .map(|window| transition(&window[0], &window[1]))
        .collect::<Vec<_>>();

    let largest_increase = transitions
        .iter()
        .filter(|fact| fact.absolute_change > 0.0)
        .max_by(|left, right| left.absolute_change.total_cmp(&right.absolute_change))
        .cloned();

    let largest_decline = transitions
        .iter()
        .filter(|fact| fact.absolute_change < 0.0)
        .min_by(|left, right| left.absolute_change.total_cmp(&right.absolute_change))
        .cloned();

    let overall_change = match (&first_complete, &last_complete) {
        (Some(first), Some(last)) if points.len() >= 2 => Some(transition(first, last)),
        _ => None,
    };

    Some(MetricFacts {
        metric: metric.value_key().to_owned(),
        first_complete,
        last_complete,
        peak_complete,
        lowest_complete,
        overall_change,
        largest_increase,
        largest_decline,
    })
}

fn transition(from: &LabeledValue, to: &LabeledValue) -> TransitionFact {
    let absolute_change = to.value - from.value;

    let percent_change = if from.value == 0.0 {
        None
    } else {
        Some(absolute_change / from.value * 100.0)
    };

    TransitionFact {
        from: from.clone(),
        to: to.clone(),
        absolute_change,
        percent_change,
    }
}

fn temporal_axis_key(plan: &AnalyticsPlan) -> Option<&'static str> {
    if plan.dimensions.contains(&Dimension::Day) {
        Some("day")
    } else if plan.dimensions.contains(&Dimension::Week) {
        Some("week")
    } else if plan.dimensions.contains(&Dimension::Month) {
        Some("month")
    } else if plan.dimensions.contains(&Dimension::Period) {
        Some("period")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{
        AnalyticsDomain, AnalyticsFilters, SortSpec, TimePeriod, VisualizationIntent,
    };
    use chrono::NaiveDate;
    use serde_json::Map;

    fn weekly_row(label: &str, order_count: i64) -> Map<String, Value> {
        let mut row = Map::new();

        row.insert("week".to_owned(), Value::String(label.to_owned()));

        row.insert("order_count".to_owned(), Value::from(order_count));

        row
    }

    #[test]
    fn facts_exclude_partial_weekly_edges() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Week],
            periods: vec![TimePeriod {
                label: "May through July 2026".to_owned(),
                start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            }],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Line),
        };

        let rows = vec![
            weekly_row("2026-04-27", 120), // partial
            weekly_row("2026-05-04", 258),
            weekly_row("2026-05-11", 259),
            weekly_row("2026-05-18", 254),
            weekly_row("2026-05-25", 235),
            weekly_row("2026-06-01", 237),
            weekly_row("2026-06-08", 234),
            weekly_row("2026-06-15", 273),
            weekly_row("2026-06-22", 331),
            weekly_row("2026-06-29", 338),
            weekly_row("2026-07-06", 363),
            weekly_row("2026-07-13", 375),
            weekly_row("2026-07-20", 382),
            weekly_row("2026-07-27", 291), // partial
        ];

        let result = QueryResult {
            columns: vec!["week".to_owned(), "order_count".to_owned()],
            row_count: rows.len(),
            rows,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let facts = build_time_series_facts(&plan, &result, true, true)
            .expect("weekly plan should produce facts");

        assert!(facts.partial_start_bucket);
        assert!(facts.partial_end_bucket);
        assert_eq!(facts.complete_bucket_count, 12);

        let order_facts = &facts.metrics[0];

        assert_eq!(
            order_facts.first_complete.as_ref().unwrap().label,
            "2026-05-04"
        );

        assert_eq!(
            order_facts.last_complete.as_ref().unwrap().label,
            "2026-07-20"
        );

        assert_eq!(
            order_facts.peak_complete.as_ref().unwrap(),
            &LabeledValue {
                label: "2026-07-20".to_owned(),
                value: 382.0,
            }
        );

        assert_eq!(
            order_facts.lowest_complete.as_ref().unwrap(),
            &LabeledValue {
                label: "2026-06-08".to_owned(),
                value: 234.0,
            }
        );

        let increase = order_facts.largest_increase.as_ref().unwrap();

        assert_eq!(increase.from.label, "2026-06-15");
        assert_eq!(increase.to.label, "2026-06-22");
        assert_eq!(increase.absolute_change, 58.0);

        let decline = order_facts.largest_decline.as_ref().unwrap();

        assert_eq!(decline.from.label, "2026-05-18");
        assert_eq!(decline.to.label, "2026-05-25");
        assert_eq!(decline.absolute_change, -19.0);

        let overall = order_facts.overall_change.as_ref().unwrap();

        assert_eq!(overall.from.value, 258.0);
        assert_eq!(overall.to.value, 382.0);
        assert_eq!(overall.absolute_change, 124.0);

        // Most important regression:
        // the partial 2026-07-27 bucket must never appear
        // as the largest decline.
        assert_ne!(decline.to.label, "2026-07-27");
    }

    #[test]
    fn zero_baseline_has_no_percentage_change() {
        let from = LabeledValue {
            label: "first".to_owned(),
            value: 0.0,
        };

        let to = LabeledValue {
            label: "second".to_owned(),
            value: 10.0,
        };

        let fact = transition(&from, &to);

        assert_eq!(fact.absolute_change, 10.0);
        assert_eq!(fact.percent_change, None);
    }
    #[test]
    fn enriches_variant_contribution_and_penetration() {
        let parent_row = Map::from_iter([
            ("quantity_sold".to_owned(), Value::from(100)),
            ("product_order_count".to_owned(), Value::from(10)),
            ("product_unique_customers".to_owned(), Value::from(8)),
            ("product_sales_value".to_owned(), Value::from(1000.0)),
        ]);

        let parent = QueryResult {
            columns: vec![
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows: vec![parent_row.clone()],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let first = Map::from_iter([
            ("variant_id".to_owned(), Value::from("variant_a")),
            ("variant_title".to_owned(), Value::from("Variant A")),
            ("quantity_sold".to_owned(), Value::from(60)),
            ("product_order_count".to_owned(), Value::from(7)),
            ("product_unique_customers".to_owned(), Value::from(6)),
            ("product_sales_value".to_owned(), Value::from(700.0)),
        ]);

        let second = Map::from_iter([
            ("variant_id".to_owned(), Value::from("variant_b")),
            ("variant_title".to_owned(), Value::from("Variant B")),
            ("quantity_sold".to_owned(), Value::from(40)),
            ("product_order_count".to_owned(), Value::from(6)),
            ("product_unique_customers".to_owned(), Value::from(5)),
            ("product_sales_value".to_owned(), Value::from(300.0)),
        ]);

        let mut variants = QueryResult {
            columns: vec![
                "variant_id".to_owned(),
                "variant_title".to_owned(),
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows: vec![first, second],
            row_count: 2,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let facts = enrich_variant_contribution_facts(&parent, &mut variants);

        let first = &variants.rows[0];
        let second = &variants.rows[1];
        let top_quantity = facts.top_quantity_variant.as_ref().unwrap();

        assert_eq!(top_quantity.variant_id, "variant_a");

        assert_eq!(top_quantity.variant_title, "Variant A");

        assert_close(top_quantity.percentage, 60.0);

        let top_sales = facts.top_sales_value_variant.as_ref().unwrap();

        assert_eq!(top_sales.variant_id, "variant_a");

        assert_close(top_sales.percentage, 70.0);

        let top_orders = facts.top_order_penetration_variant.as_ref().unwrap();

        assert_eq!(top_orders.variant_id, "variant_a");

        assert_close(top_orders.percentage, 70.0);

        let top_buyers = facts.top_buyer_penetration_variant.as_ref().unwrap();

        assert_eq!(top_buyers.variant_id, "variant_a");

        assert_close(top_buyers.percentage, 75.0);

        assert_close(first["quantity_share_pct"].as_f64().unwrap(), 60.0);

        assert_close(second["quantity_share_pct"].as_f64().unwrap(), 40.0);

        assert_close(first["sales_value_share_pct"].as_f64().unwrap(), 70.0);

        assert_close(second["sales_value_share_pct"].as_f64().unwrap(), 30.0);

        /*
         * Penetration is intentionally non-additive.
         *
         * A parent order/customer may contain multiple variants.
         */
        assert_close(first["order_penetration_pct"].as_f64().unwrap(), 70.0);

        assert_close(second["order_penetration_pct"].as_f64().unwrap(), 60.0);

        assert_close(first["buyer_penetration_pct"].as_f64().unwrap(), 75.0);

        assert_close(second["buyer_penetration_pct"].as_f64().unwrap(), 62.5);

        assert_close(facts.quantity_share_total_pct.unwrap(), 100.0);

        assert_close(facts.sales_value_share_total_pct.unwrap(), 100.0);

        /*
         * B3 must never mutate/recalculate the parent result.
         */
        assert_eq!(parent.rows[0], parent_row);
    }

    #[test]
    fn zero_parent_denominators_produce_null_percentages() {
        let parent = QueryResult {
            columns: vec![
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows: vec![Map::from_iter([
                ("quantity_sold".to_owned(), Value::from(0)),
                ("product_order_count".to_owned(), Value::from(0)),
                ("product_unique_customers".to_owned(), Value::from(0)),
                ("product_sales_value".to_owned(), Value::from(0)),
            ])],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let mut variants = QueryResult {
            columns: vec![
                "variant_id".to_owned(),
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows: vec![Map::from_iter([
                ("variant_id".to_owned(), Value::from("variant_a")),
                ("quantity_sold".to_owned(), Value::from(0)),
                ("product_order_count".to_owned(), Value::from(0)),
                ("product_unique_customers".to_owned(), Value::from(0)),
                ("product_sales_value".to_owned(), Value::from(0)),
            ])],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        enrich_variant_contribution_facts(&parent, &mut variants);

        let row = &variants.rows[0];

        assert!(row["quantity_share_pct"].is_null());
        assert!(row["sales_value_share_pct"].is_null());
        assert!(row["order_penetration_pct"].is_null());
        assert!(row["buyer_penetration_pct"].is_null());
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_001,
            "expected {expected}, got {actual}"
        );
    }
}
