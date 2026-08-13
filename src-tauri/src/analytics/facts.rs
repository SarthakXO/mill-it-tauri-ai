use super::plan::{AnalyticsPlan, Dimension, Metric};
use crate::database::QueryResult;
use serde::Serialize;
use serde_json::{Map, Value};

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
}
