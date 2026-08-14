use super::plan::{AnalyticsPlan, Dimension, Metric};
use crate::database::QueryResult;
use chrono::{Datelike, Duration, NaiveDate};
use serde::Serialize;
use serde_json::{Map, Number, Value};

use std::collections::{BTreeMap, BTreeSet, HashMap};

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemporalVariantContributionFacts {
    pub(crate) buckets: Vec<TemporalVariantContributionBucketFacts>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PercentagePointShift {
    pub(crate) previous_pct: Option<f64>,
    pub(crate) current_pct: Option<f64>,
    pub(crate) change_pp: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VariantMixShiftFact {
    pub(crate) variant_id: String,
    pub(crate) variant_title: String,

    pub(crate) quantity_mix: PercentagePointShift,
    pub(crate) sales_value_mix: PercentagePointShift,

    pub(crate) order_penetration: PercentagePointShift,
    pub(crate) buyer_penetration: PercentagePointShift,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemporalVariantMixShiftTransition {
    pub(crate) from_bucket: String,
    pub(crate) to_bucket: String,
    pub(crate) variants: Vec<VariantMixShiftFact>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemporalVariantMixShiftFacts {
    pub(crate) transitions: Vec<TemporalVariantMixShiftTransition>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VariantMixShiftDriverFact {
    pub(crate) variant_id: String,
    pub(crate) variant_title: String,

    pub(crate) previous_pct: f64,
    pub(crate) current_pct: f64,
    pub(crate) change_pp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemporalVariantMixShiftTransitionDriverFacts {
    pub(crate) from_bucket: String,
    pub(crate) to_bucket: String,

    pub(crate) largest_quantity_mix_gain: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_quantity_mix_loss: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_sales_mix_gain: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_sales_mix_loss: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_order_penetration_gain: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_order_penetration_loss: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_buyer_penetration_gain: Option<VariantMixShiftDriverFact>,

    pub(crate) largest_buyer_penetration_loss: Option<VariantMixShiftDriverFact>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemporalVariantMixShiftDriverFacts {
    pub(crate) transitions: Vec<TemporalVariantMixShiftTransitionDriverFacts>,
}

#[derive(Clone, Debug)]
struct TemporalVariantContributionPoint {
    quantity_share_pct: Option<f64>,
    sales_value_share_pct: Option<f64>,
    order_penetration_pct: Option<f64>,
    buyer_penetration_pct: Option<f64>,
}

#[derive(Clone, Debug)]
struct TemporalParentDenominators {
    quantity_sold: Option<f64>,
    product_sales_value: Option<f64>,
    product_order_count: Option<f64>,
    product_unique_customers: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemporalVariantContributionBucketFacts {
    pub(crate) bucket: String,
    pub(crate) variant_row_count: usize,
    pub(crate) quantity_share_total_pct: Option<f64>,
    pub(crate) sales_value_share_total_pct: Option<f64>,
}

pub(crate) fn enrich_temporal_variant_contribution_facts(
    plan: &AnalyticsPlan,
    parent: &QueryResult,
    variants: &mut QueryResult,
) -> Option<TemporalVariantContributionFacts> {
    use std::collections::HashMap;

    let axis_key = if plan.dimensions.contains(&Dimension::Day) {
        "day"
    } else if plan.dimensions.contains(&Dimension::Week) {
        "week"
    } else if plan.dimensions.contains(&Dimension::Month) {
        "month"
    } else {
        return None;
    };

    let parent_by_bucket = parent
        .rows
        .iter()
        .filter_map(|row| {
            let bucket = row.get(axis_key)?.as_str()?.to_owned();

            Some((bucket, row))
        })
        .collect::<HashMap<_, _>>();

    for row in &mut variants.rows {
        let Some(bucket) = row.get(axis_key).and_then(Value::as_str) else {
            continue;
        };

        let Some(parent_row) = parent_by_bucket.get(bucket) else {
            continue;
        };

        insert_percentage(
            row,
            QUANTITY_SHARE_PCT,
            percentage(
                row_number(row, "quantity_sold"),
                row_number(parent_row, "quantity_sold"),
            ),
        );

        insert_percentage(
            row,
            ORDER_PENETRATION_PCT,
            percentage(
                row_number(row, "product_order_count"),
                row_number(parent_row, "product_order_count"),
            ),
        );

        insert_percentage(
            row,
            BUYER_PENETRATION_PCT,
            percentage(
                row_number(row, "product_unique_customers"),
                row_number(parent_row, "product_unique_customers"),
            ),
        );

        insert_percentage(
            row,
            SALES_VALUE_SHARE_PCT,
            percentage(
                row_number(row, "product_sales_value"),
                row_number(parent_row, "product_sales_value"),
            ),
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

    let buckets = parent
        .rows
        .iter()
        .filter_map(|parent_row| {
            let bucket = parent_row.get(axis_key)?.as_str()?.to_owned();

            let matching_rows = variants
                .rows
                .iter()
                .filter(|row| row.get(axis_key).and_then(Value::as_str) == Some(bucket.as_str()))
                .collect::<Vec<_>>();

            if matching_rows.is_empty() {
                return None;
            }

            Some(TemporalVariantContributionBucketFacts {
                bucket,
                variant_row_count: matching_rows.len(),

                quantity_share_total_pct: percentage_total_from_rows(
                    &matching_rows,
                    QUANTITY_SHARE_PCT,
                ),

                sales_value_share_total_pct: percentage_total_from_rows(
                    &matching_rows,
                    SALES_VALUE_SHARE_PCT,
                ),
            })
        })
        .collect::<Vec<_>>();

    Some(TemporalVariantContributionFacts { buckets })
}

pub(crate) fn build_temporal_variant_mix_shift_facts(
    plan: &AnalyticsPlan,
    parent: &QueryResult,
    variants: &QueryResult,
) -> Option<TemporalVariantMixShiftFacts> {
    let axis_key = temporal_axis_key(plan)?;

    /*
     * Parent result is already chronological because temporal
     * semantic queries are compiled with ORDER BY their date axis.
     *
     * Preserve that order here rather than re-ranking anything.
     */
    let mut buckets = parent
        .rows
        .iter()
        .filter_map(|row| row.get(axis_key).and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();

    buckets.dedup();

    if buckets.len() < 2 {
        return Some(TemporalVariantMixShiftFacts {
            transitions: Vec::new(),
        });
    }

    /*
     * Parent denominators let us distinguish:
     *
     * missing variant row + valid parent denominator
     *     => genuine 0%
     *
     * invalid/zero parent denominator
     *     => undefined percentage
     */
    let parent_denominators = parent
        .rows
        .iter()
        .filter_map(|row| {
            let bucket = row.get(axis_key)?.as_str()?.to_owned();

            Some((
                bucket,
                TemporalParentDenominators {
                    quantity_sold: row_number(row, "quantity_sold"),

                    product_sales_value: row_number(row, "product_sales_value"),

                    product_order_count: row_number(row, "product_order_count"),

                    product_unique_customers: row_number(row, "product_unique_customers"),
                },
            ))
        })
        .collect::<HashMap<_, _>>();

    /*
     * Contribution values are already deterministically calculated
     * by B4-A. B4-B consumes those facts; it does not recompute
     * shares from raw product metrics.
     */
    let mut points = HashMap::<(String, String), TemporalVariantContributionPoint>::new();

    let mut variant_ids_by_bucket = HashMap::<String, BTreeSet<String>>::new();

    /*
     * Use stable variant identity for matching.
     * Titles are display labels only.
     */
    let mut variant_titles = BTreeMap::<String, String>::new();

    for row in &variants.rows {
        let Some(bucket) = row.get(axis_key).and_then(Value::as_str) else {
            continue;
        };

        let Some(variant_id) = row.get("variant_id").and_then(Value::as_str) else {
            continue;
        };

        let variant_title = row
            .get("variant_title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(variant_id);

        variant_titles
            .entry(variant_id.to_owned())
            .or_insert_with(|| variant_title.to_owned());

        variant_ids_by_bucket
            .entry(bucket.to_owned())
            .or_default()
            .insert(variant_id.to_owned());

        points.insert(
            (bucket.to_owned(), variant_id.to_owned()),
            TemporalVariantContributionPoint {
                quantity_share_pct: row_number(row, QUANTITY_SHARE_PCT),

                sales_value_share_pct: row_number(row, SALES_VALUE_SHARE_PCT),

                order_penetration_pct: row_number(row, ORDER_PENETRATION_PCT),

                buyer_penetration_pct: row_number(row, BUYER_PENETRATION_PCT),
            },
        );
    }

    let mut transitions = Vec::new();

    for bucket_pair in buckets.windows(2) {
        let from_bucket = &bucket_pair[0];
        let to_bucket = &bucket_pair[1];

        /*
         * Never compare across a missing temporal bucket.
         *
         * Example:
         * Jan + Mar returned but February absent
         * must NOT silently become "Jan -> Mar monthly shift".
         */
        if !are_adjacent_temporal_buckets(plan, from_bucket, to_bucket) {
            continue;
        }

        let Some(from_denominators) = parent_denominators.get(from_bucket) else {
            continue;
        };

        let Some(to_denominators) = parent_denominators.get(to_bucket) else {
            continue;
        };

        /*
         * Only variants that participated in either side of this
         * transition belong in the transition.
         *
         * This avoids generating meaningless 0 -> 0 transitions
         * for variants that appear only much later.
         */
        let mut variant_ids = BTreeSet::new();

        if let Some(ids) = variant_ids_by_bucket.get(from_bucket) {
            variant_ids.extend(ids.iter().cloned());
        }

        if let Some(ids) = variant_ids_by_bucket.get(to_bucket) {
            variant_ids.extend(ids.iter().cloned());
        }

        let mut variant_shifts = Vec::new();

        for variant_id in variant_ids {
            let previous_point = points.get(&(from_bucket.clone(), variant_id.clone()));

            let current_point = points.get(&(to_bucket.clone(), variant_id.clone()));

            let previous_quantity = contribution_value_or_zero(
                previous_point,
                |point| point.quantity_share_pct,
                from_denominators.quantity_sold,
            );

            let current_quantity = contribution_value_or_zero(
                current_point,
                |point| point.quantity_share_pct,
                to_denominators.quantity_sold,
            );

            let previous_sales_value = contribution_value_or_zero(
                previous_point,
                |point| point.sales_value_share_pct,
                from_denominators.product_sales_value,
            );

            let current_sales_value = contribution_value_or_zero(
                current_point,
                |point| point.sales_value_share_pct,
                to_denominators.product_sales_value,
            );

            let previous_order_penetration = contribution_value_or_zero(
                previous_point,
                |point| point.order_penetration_pct,
                from_denominators.product_order_count,
            );

            let current_order_penetration = contribution_value_or_zero(
                current_point,
                |point| point.order_penetration_pct,
                to_denominators.product_order_count,
            );

            let previous_buyer_penetration = contribution_value_or_zero(
                previous_point,
                |point| point.buyer_penetration_pct,
                from_denominators.product_unique_customers,
            );

            let current_buyer_penetration = contribution_value_or_zero(
                current_point,
                |point| point.buyer_penetration_pct,
                to_denominators.product_unique_customers,
            );

            variant_shifts.push(VariantMixShiftFact {
                variant_id: variant_id.clone(),

                variant_title: variant_titles
                    .get(&variant_id)
                    .cloned()
                    .unwrap_or_else(|| variant_id.clone()),

                quantity_mix: percentage_point_shift(previous_quantity, current_quantity),

                sales_value_mix: percentage_point_shift(previous_sales_value, current_sales_value),

                order_penetration: percentage_point_shift(
                    previous_order_penetration,
                    current_order_penetration,
                ),

                buyer_penetration: percentage_point_shift(
                    previous_buyer_penetration,
                    current_buyer_penetration,
                ),
            });
        }

        transitions.push(TemporalVariantMixShiftTransition {
            from_bucket: from_bucket.clone(),
            to_bucket: to_bucket.clone(),
            variants: variant_shifts,
        });
    }

    Some(TemporalVariantMixShiftFacts { transitions })
}

#[derive(Clone, Copy)]
enum ShiftDirection {
    Gain,
    Loss,
}

pub(crate) fn derive_temporal_variant_mix_shift_driver_facts(
    facts: &TemporalVariantMixShiftFacts,
) -> TemporalVariantMixShiftDriverFacts {
    let transitions = facts
        .transitions
        .iter()
        .map(|transition| TemporalVariantMixShiftTransitionDriverFacts {
            from_bucket: transition.from_bucket.clone(),

            to_bucket: transition.to_bucket.clone(),

            largest_quantity_mix_gain: top_shift_driver(
                &transition.variants,
                |variant| &variant.quantity_mix,
                ShiftDirection::Gain,
            ),

            largest_quantity_mix_loss: top_shift_driver(
                &transition.variants,
                |variant| &variant.quantity_mix,
                ShiftDirection::Loss,
            ),

            largest_sales_mix_gain: top_shift_driver(
                &transition.variants,
                |variant| &variant.sales_value_mix,
                ShiftDirection::Gain,
            ),

            largest_sales_mix_loss: top_shift_driver(
                &transition.variants,
                |variant| &variant.sales_value_mix,
                ShiftDirection::Loss,
            ),

            largest_order_penetration_gain: top_shift_driver(
                &transition.variants,
                |variant| &variant.order_penetration,
                ShiftDirection::Gain,
            ),

            largest_order_penetration_loss: top_shift_driver(
                &transition.variants,
                |variant| &variant.order_penetration,
                ShiftDirection::Loss,
            ),

            largest_buyer_penetration_gain: top_shift_driver(
                &transition.variants,
                |variant| &variant.buyer_penetration,
                ShiftDirection::Gain,
            ),

            largest_buyer_penetration_loss: top_shift_driver(
                &transition.variants,
                |variant| &variant.buyer_penetration,
                ShiftDirection::Loss,
            ),
        })
        .collect();

    TemporalVariantMixShiftDriverFacts { transitions }
}

fn top_shift_driver(
    variants: &[VariantMixShiftFact],
    shift_for: fn(&VariantMixShiftFact) -> &PercentagePointShift,
    direction: ShiftDirection,
) -> Option<VariantMixShiftDriverFact> {
    let mut candidates = variants
        .iter()
        .filter_map(|variant| {
            let shift = shift_for(variant);

            let previous_pct = shift.previous_pct?;
            let current_pct = shift.current_pct?;
            let change_pp = shift.change_pp?;

            if !previous_pct.is_finite() || !current_pct.is_finite() || !change_pp.is_finite() {
                return None;
            }

            let qualifies = match direction {
                ShiftDirection::Gain => change_pp > 0.0,

                ShiftDirection::Loss => change_pp < 0.0,
            };

            if !qualifies {
                return None;
            }

            Some(VariantMixShiftDriverFact {
                variant_id: variant.variant_id.clone(),

                variant_title: variant.variant_title.clone(),

                previous_pct,
                current_pct,
                change_pp,
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        let change_order = match direction {
            /*
             * Largest positive change first.
             */
            ShiftDirection::Gain => right.change_pp.total_cmp(&left.change_pp),

            /*
             * Most negative change first.
             */
            ShiftDirection::Loss => left.change_pp.total_cmp(&right.change_pp),
        };

        change_order
            .then_with(|| left.variant_title.cmp(&right.variant_title))
            .then_with(|| left.variant_id.cmp(&right.variant_id))
    });

    candidates.into_iter().next()
}

fn contribution_value_or_zero<F>(
    point: Option<&TemporalVariantContributionPoint>,
    value: F,
    parent_denominator: Option<f64>,
) -> Option<f64>
where
    F: FnOnce(&TemporalVariantContributionPoint) -> Option<f64>,
{
    if let Some(point) = point {
        return value(point);
    }

    /*
     * A missing variant row under a valid positive parent
     * denominator means that variant contributed zero.
     */
    parent_denominator
        .filter(|denominator| denominator.is_finite() && *denominator > 0.0)
        .map(|_| 0.0)
}

fn percentage_point_shift(
    previous_pct: Option<f64>,
    current_pct: Option<f64>,
) -> PercentagePointShift {
    let change_pp = match (previous_pct, current_pct) {
        (Some(previous), Some(current)) if previous.is_finite() && current.is_finite() => {
            Some(current - previous)
        }

        _ => None,
    };

    PercentagePointShift {
        previous_pct,
        current_pct,
        change_pp,
    }
}

fn are_adjacent_temporal_buckets(plan: &AnalyticsPlan, from: &str, to: &str) -> bool {
    let Ok(from) = NaiveDate::parse_from_str(from, "%Y-%m-%d") else {
        return false;
    };

    let Ok(to) = NaiveDate::parse_from_str(to, "%Y-%m-%d") else {
        return false;
    };

    if plan.dimensions.contains(&Dimension::Day) {
        return from.checked_add_signed(Duration::days(1)) == Some(to);
    }

    if plan.dimensions.contains(&Dimension::Week) {
        return from.checked_add_signed(Duration::days(7)) == Some(to);
    }

    if plan.dimensions.contains(&Dimension::Month) {
        return next_month_start(from) == Some(to);
    }

    false
}

fn next_month_start(date: NaiveDate) -> Option<NaiveDate> {
    if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    }
}

fn percentage_total_from_rows(rows: &[&Map<String, Value>], key: &str) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }

    rows.iter()
        .try_fold(0.0, |total, row| Some(total + row.get(key)?.as_f64()?))
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

    fn monthly_product_plan(start: NaiveDate, end_exclusive: NaiveDate) -> AnalyticsPlan {
        AnalyticsPlan {
            domain: AnalyticsDomain::Products,
            metrics: vec![
                Metric::QuantitySold,
                Metric::ProductOrderCount,
                Metric::ProductUniqueCustomers,
                Metric::ProductSalesValue,
            ],
            dimensions: vec![Dimension::Month],
            periods: vec![TimePeriod {
                label: "Test period".to_owned(),
                start,
                end_exclusive,
            }],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Line),
        }
    }

    fn temporal_parent_row(
        month: &str,
        quantity_sold: i64,
        product_order_count: i64,
        product_unique_customers: i64,
        product_sales_value: f64,
    ) -> Map<String, Value> {
        Map::from_iter([
            ("month".to_owned(), Value::from(month)),
            ("quantity_sold".to_owned(), Value::from(quantity_sold)),
            (
                "product_order_count".to_owned(),
                Value::from(product_order_count),
            ),
            (
                "product_unique_customers".to_owned(),
                Value::from(product_unique_customers),
            ),
            (
                "product_sales_value".to_owned(),
                Value::from(product_sales_value),
            ),
        ])
    }

    fn temporal_variant_row(
        month: &str,
        variant_id: &str,
        variant_title: &str,
        quantity_sold: i64,
        product_order_count: i64,
        product_unique_customers: i64,
        product_sales_value: f64,
    ) -> Map<String, Value> {
        Map::from_iter([
            ("month".to_owned(), Value::from(month)),
            ("variant_id".to_owned(), Value::from(variant_id)),
            ("variant_title".to_owned(), Value::from(variant_title)),
            ("quantity_sold".to_owned(), Value::from(quantity_sold)),
            (
                "product_order_count".to_owned(),
                Value::from(product_order_count),
            ),
            (
                "product_unique_customers".to_owned(),
                Value::from(product_unique_customers),
            ),
            (
                "product_sales_value".to_owned(),
                Value::from(product_sales_value),
            ),
        ])
    }

    fn temporal_parent_result(rows: Vec<Map<String, Value>>) -> QueryResult {
        let row_count = rows.len();

        QueryResult {
            columns: vec![
                "month".to_owned(),
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows,
            row_count,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        }
    }

    fn temporal_variant_result(rows: Vec<Map<String, Value>>) -> QueryResult {
        let row_count = rows.len();

        QueryResult {
            columns: vec![
                "month".to_owned(),
                "variant_id".to_owned(),
                "variant_title".to_owned(),
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows,
            row_count,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        }
    }

    fn temporal_mix_shift_facts(
        plan: &AnalyticsPlan,
        parent: &QueryResult,
        mut variants: QueryResult,
    ) -> TemporalVariantMixShiftFacts {
        enrich_temporal_variant_contribution_facts(plan, parent, &mut variants)
            .expect("temporal contribution enrichment should succeed");

        build_temporal_variant_mix_shift_facts(plan, parent, &variants)
            .expect("temporal mix-shift facts should succeed")
    }

    #[test]
    fn temporal_variant_mix_shift_uses_percentage_points() {
        let plan = monthly_product_plan(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        );

        let parent = temporal_parent_result(vec![
            temporal_parent_row("2026-01-01", 100, 10, 10, 1000.0),
            temporal_parent_row("2026-02-01", 100, 10, 10, 1000.0),
        ]);

        let variants = temporal_variant_result(vec![
            temporal_variant_row("2026-01-01", "variant_a", "Variant A", 40, 5, 5, 400.0),
            temporal_variant_row("2026-02-01", "variant_a", "Variant A", 30, 4, 4, 300.0),
        ]);

        let facts = temporal_mix_shift_facts(&plan, &parent, variants);

        assert_eq!(facts.transitions.len(), 1);

        let transition = &facts.transitions[0];

        assert_eq!(transition.from_bucket, "2026-01-01");
        assert_eq!(transition.to_bucket, "2026-02-01");

        let variant = transition
            .variants
            .iter()
            .find(|variant| variant.variant_id == "variant_a")
            .unwrap();

        assert_close(variant.quantity_mix.previous_pct.unwrap(), 40.0);

        assert_close(variant.quantity_mix.current_pct.unwrap(), 30.0);

        assert_close(variant.quantity_mix.change_pp.unwrap(), -10.0);

        /*
         * 40% -> 30% is -10 percentage points.
         * It must NOT be reported as -25% relative change.
         */
        assert_ne!(variant.quantity_mix.change_pp, Some(-25.0),);
    }

    #[test]
    fn temporal_variant_mix_shift_zero_fills_previous_missing_variant() {
        let plan = monthly_product_plan(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        );

        let parent = temporal_parent_result(vec![
            temporal_parent_row("2026-01-01", 100, 10, 10, 1000.0),
            temporal_parent_row("2026-02-01", 100, 10, 10, 1000.0),
        ]);

        /*
         * Variant B has no January row and appears in February
         * with 20% quantity contribution.
         */
        let variants = temporal_variant_result(vec![temporal_variant_row(
            "2026-02-01",
            "variant_b",
            "Variant B",
            20,
            2,
            2,
            200.0,
        )]);

        let facts = temporal_mix_shift_facts(&plan, &parent, variants);

        assert_eq!(facts.transitions.len(), 1);

        let variant = facts.transitions[0]
            .variants
            .iter()
            .find(|variant| variant.variant_id == "variant_b")
            .unwrap();

        assert_eq!(variant.quantity_mix.previous_pct, Some(0.0),);

        assert_close(variant.quantity_mix.current_pct.unwrap(), 20.0);

        assert_close(variant.quantity_mix.change_pp.unwrap(), 20.0);
    }

    #[test]
    fn temporal_variant_mix_shift_captures_variant_dropping_to_zero() {
        let plan = monthly_product_plan(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        );

        let parent = temporal_parent_result(vec![
            temporal_parent_row("2026-01-01", 100, 10, 10, 1000.0),
            temporal_parent_row("2026-02-01", 100, 10, 10, 1000.0),
        ]);

        /*
         * Variant C contributes 10% in January and has no
         * physical SQL row in February.
         */
        let variants = temporal_variant_result(vec![temporal_variant_row(
            "2026-01-01",
            "variant_c",
            "Variant C",
            10,
            1,
            1,
            100.0,
        )]);

        let facts = temporal_mix_shift_facts(&plan, &parent, variants);

        assert_eq!(facts.transitions.len(), 1);

        let variant = facts.transitions[0]
            .variants
            .iter()
            .find(|variant| variant.variant_id == "variant_c")
            .unwrap();

        assert_close(variant.quantity_mix.previous_pct.unwrap(), 10.0);

        assert_eq!(variant.quantity_mix.current_pct, Some(0.0),);

        assert_close(variant.quantity_mix.change_pp.unwrap(), -10.0);
    }

    #[test]
    fn temporal_variant_mix_shift_handles_non_additive_penetration() {
        let plan = monthly_product_plan(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        );

        let parent = temporal_parent_result(vec![
            temporal_parent_row("2026-01-01", 100, 10, 10, 1000.0),
            temporal_parent_row("2026-02-01", 100, 10, 10, 1000.0),
        ]);

        let variants = temporal_variant_result(vec![
            temporal_variant_row("2026-01-01", "variant_a", "Variant A", 40, 5, 5, 400.0),
            temporal_variant_row("2026-02-01", "variant_a", "Variant A", 40, 4, 4, 400.0),
        ]);

        let facts = temporal_mix_shift_facts(&plan, &parent, variants);

        let variant = facts.transitions[0]
            .variants
            .iter()
            .find(|variant| variant.variant_id == "variant_a")
            .unwrap();

        assert_close(variant.order_penetration.previous_pct.unwrap(), 50.0);

        assert_close(variant.order_penetration.current_pct.unwrap(), 40.0);

        assert_close(variant.order_penetration.change_pp.unwrap(), -10.0);

        assert_close(variant.buyer_penetration.change_pp.unwrap(), -10.0);
    }

    #[test]
    fn temporal_variant_mix_shift_preserves_undefined_denominator() {
        let plan = monthly_product_plan(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        );

        let parent = temporal_parent_result(vec![
            temporal_parent_row("2026-01-01", 100, 10, 0, 1000.0),
            temporal_parent_row("2026-02-01", 100, 10, 0, 1000.0),
        ]);

        let variants = temporal_variant_result(vec![
            temporal_variant_row("2026-01-01", "variant_a", "Variant A", 40, 5, 0, 400.0),
            temporal_variant_row("2026-02-01", "variant_a", "Variant A", 30, 4, 0, 300.0),
        ]);

        let facts = temporal_mix_shift_facts(&plan, &parent, variants);

        let variant = facts.transitions[0]
            .variants
            .iter()
            .find(|variant| variant.variant_id == "variant_a")
            .unwrap();

        assert_eq!(variant.buyer_penetration.previous_pct, None,);

        assert_eq!(variant.buyer_penetration.current_pct, None,);

        assert_eq!(variant.buyer_penetration.change_pp, None,);
    }

    #[test]
    fn temporal_variant_mix_shift_does_not_compare_non_adjacent_months() {
        let plan = monthly_product_plan(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        );

        /*
         * February is intentionally missing.
         */
        let parent = temporal_parent_result(vec![
            temporal_parent_row("2026-01-01", 100, 10, 10, 1000.0),
            temporal_parent_row("2026-03-01", 100, 10, 10, 1000.0),
        ]);

        let variants = temporal_variant_result(vec![
            temporal_variant_row("2026-01-01", "variant_a", "Variant A", 40, 5, 5, 400.0),
            temporal_variant_row("2026-03-01", "variant_a", "Variant A", 30, 4, 4, 300.0),
        ]);

        let facts = temporal_mix_shift_facts(&plan, &parent, variants);

        assert!(
            facts.transitions.is_empty(),
            "January must not be compared directly with March"
        );
    }
    fn quantity_shift_variant(
        variant_id: &str,
        variant_title: &str,
        previous_pct: f64,
        current_pct: f64,
    ) -> VariantMixShiftFact {
        let undefined = PercentagePointShift {
            previous_pct: None,
            current_pct: None,
            change_pp: None,
        };

        VariantMixShiftFact {
            variant_id: variant_id.to_owned(),
            variant_title: variant_title.to_owned(),

            quantity_mix: percentage_point_shift(Some(previous_pct), Some(current_pct)),

            sales_value_mix: undefined.clone(),
            order_penetration: undefined.clone(),
            buyer_penetration: undefined,
        }
    }
    #[test]
    fn temporal_variant_driver_facts_select_largest_gain_and_loss() {
        let facts = TemporalVariantMixShiftFacts {
            transitions: vec![TemporalVariantMixShiftTransition {
                from_bucket: "2026-06-01".to_owned(),

                to_bucket: "2026-07-01".to_owned(),

                variants: vec![
                    quantity_shift_variant("variant_a", "Variant A", 10.0, 16.0),
                    quantity_shift_variant("variant_b", "Variant B", 20.0, 12.0),
                    quantity_shift_variant("variant_c", "Variant C", 10.0, 13.0),
                    quantity_shift_variant("variant_d", "Variant D", 5.0, 1.0),
                ],
            }],
        };

        let drivers = derive_temporal_variant_mix_shift_driver_facts(&facts);

        assert_eq!(drivers.transitions.len(), 1);

        let transition = &drivers.transitions[0];

        let gain = transition.largest_quantity_mix_gain.as_ref().unwrap();

        assert_eq!(gain.variant_id, "variant_a");
        assert_eq!(gain.variant_title, "Variant A");

        assert_close(gain.previous_pct, 10.0);

        assert_close(gain.current_pct, 16.0);

        assert_close(gain.change_pp, 6.0);

        let loss = transition.largest_quantity_mix_loss.as_ref().unwrap();

        assert_eq!(loss.variant_id, "variant_b");
        assert_eq!(loss.variant_title, "Variant B");

        assert_close(loss.previous_pct, 20.0);

        assert_close(loss.current_pct, 12.0);

        assert_close(loss.change_pp, -8.0);
    }
    #[test]
    fn temporal_variant_driver_facts_include_drop_to_zero() {
        let facts = TemporalVariantMixShiftFacts {
            transitions: vec![TemporalVariantMixShiftTransition {
                from_bucket: "2026-06-01".to_owned(),

                to_bucket: "2026-07-01".to_owned(),

                variants: vec![
                    quantity_shift_variant("variant_active", "Active Variant", 20.0, 16.0),
                    quantity_shift_variant("variant_disappeared", "Disappeared Variant", 9.0, 0.0),
                ],
            }],
        };

        let drivers = derive_temporal_variant_mix_shift_driver_facts(&facts);

        let loss = drivers.transitions[0]
            .largest_quantity_mix_loss
            .as_ref()
            .unwrap();

        assert_eq!(loss.variant_id, "variant_disappeared");

        assert_close(loss.previous_pct, 9.0);

        assert_close(loss.current_pct, 0.0);

        assert_close(loss.change_pp, -9.0);
    }
    #[test]
    fn temporal_variant_driver_facts_break_ties_deterministically() {
        let facts = TemporalVariantMixShiftFacts {
            transitions: vec![TemporalVariantMixShiftTransition {
                from_bucket: "2026-06-01".to_owned(),

                to_bucket: "2026-07-01".to_owned(),

                variants: vec![
                    quantity_shift_variant("variant_z", "Zulu Variant", 10.0, 15.0),
                    quantity_shift_variant("variant_a", "Alpha Variant", 20.0, 25.0),
                ],
            }],
        };

        let drivers = derive_temporal_variant_mix_shift_driver_facts(&facts);

        let gain = drivers.transitions[0]
            .largest_quantity_mix_gain
            .as_ref()
            .unwrap();

        /*
         * Both gained exactly +5pp.
         * Alphabetical title wins the deterministic tie.
         */
        assert_eq!(gain.variant_id, "variant_a");

        assert_eq!(gain.variant_title, "Alpha Variant");
    }
    #[test]
    fn temporal_variant_driver_facts_ignore_zero_and_undefined_changes() {
        let undefined = PercentagePointShift {
            previous_pct: None,
            current_pct: None,
            change_pp: None,
        };

        let facts = TemporalVariantMixShiftFacts {
            transitions: vec![TemporalVariantMixShiftTransition {
                from_bucket: "2026-06-01".to_owned(),

                to_bucket: "2026-07-01".to_owned(),

                variants: vec![VariantMixShiftFact {
                    variant_id: "variant_flat".to_owned(),

                    variant_title: "Flat Variant".to_owned(),

                    quantity_mix: percentage_point_shift(Some(10.0), Some(10.0)),

                    sales_value_mix: undefined.clone(),

                    order_penetration: undefined.clone(),

                    buyer_penetration: undefined,
                }],
            }],
        };

        let drivers = derive_temporal_variant_mix_shift_driver_facts(&facts);

        let transition = &drivers.transitions[0];

        assert!(transition.largest_quantity_mix_gain.is_none());

        assert!(transition.largest_quantity_mix_loss.is_none());
    }
}
