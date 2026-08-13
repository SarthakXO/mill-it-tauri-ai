use super::{
    facts::{AnalyticalFacts, MetricFacts},
    plan::{AnalyticsPlan, Dimension, Metric},
};
use crate::database::QueryResult;
use chrono::{Datelike, Duration};
use serde_json::Value;

pub(crate) fn summarize_metric_query(plan: &AnalyticsPlan, result: &QueryResult) -> String {
    if result.rows.is_empty() {
        return "No matching analytics data was found for the requested periods.".to_owned();
    }
    if plan.dimensions.iter().any(Dimension::is_status) {
        return status_summary(plan, result);
    }
    if plan.dimensions.contains(&Dimension::Variant)
        || plan.dimensions.contains(&Dimension::Product)
    {
        return if plan.metrics.len() > 1 {
            multi_metric_ranking_summary(plan, result)
        } else {
            ranking_summary(plan, result)
        };
    }

    if plan.metrics.len() > 1 {
        return multi_metric_summary(plan, result);
    }
    comparison_summary(plan, result, plan.metrics[0])
}

pub(crate) fn summarize_metric_query_with_facts(
    plan: &AnalyticsPlan,
    result: &QueryResult,
    analytical_facts: Option<&AnalyticalFacts>,
) -> String {
    let is_time_series = plan.dimensions.iter().any(|dimension| {
        matches!(
            dimension,
            Dimension::Day | Dimension::Week | Dimension::Month
        )
    });

    if !is_time_series {
        return summarize_metric_query(plan, result);
    }

    let Some(facts) = analytical_facts else {
        return summarize_metric_query(plan, result);
    };

    if facts.complete_bucket_count == 0 {
        return format!(
            "The returned series contains no complete buckets suitable for analytical comparison.{}",
            partial_bucket_note(facts)
        );
    }

    if plan.metrics.len() > 1 {
        return multi_metric_fact_summary(plan, facts);
    }

    let Some(metric) = plan.metrics.first().copied() else {
        return summarize_metric_query(plan, result);
    };

    let Some(metric_facts) = facts
        .metrics
        .iter()
        .find(|facts| facts.metric == metric.value_key())
    else {
        return summarize_metric_query(plan, result);
    };

    single_metric_fact_summary(metric, metric_facts, facts)
}

fn single_metric_fact_summary(
    metric: Metric,
    metric_facts: &MetricFacts,
    facts: &AnalyticalFacts,
) -> String {
    let note = partial_bucket_note(facts);

    let Some(first) = metric_facts.first_complete.as_ref() else {
        return format!(
            "No complete {} buckets were available for analysis.{}",
            facts.complete_bucket_count, note
        );
    };

    let Some(last) = metric_facts.last_complete.as_ref() else {
        return format!(
            "{} was {} for the only complete bucket, {}.{}",
            metric_label(metric),
            format_value(metric, first.value),
            first.label,
            note
        );
    };

    if facts.complete_bucket_count == 1 {
        return format!(
            "{} was {} for the only complete bucket, {}.{}",
            metric_label(metric),
            format_value(metric, first.value),
            first.label,
            note
        );
    }

    let overall = metric_facts
        .overall_change
        .as_ref()
        .map(|change| {
            change.percent_change.map_or_else(
                || {
                    if change.to.value == change.from.value {
                        "had no meaningful percentage change".to_owned()
                    } else {
                        "changed from a zero baseline; percentage change is not meaningful"
                            .to_owned()
                    }
                },
                |percent| format!("changed {percent:+.1}%"),
            )
        })
        .unwrap_or_else(|| "had no comparable overall change".to_owned());

    let peak = metric_facts
        .peak_complete
        .as_ref()
        .map(|point| {
            format!(
                "{} was highest at {}",
                point.label,
                format_value(metric, point.value)
            )
        })
        .unwrap_or_else(|| "No complete-bucket maximum was available".to_owned());

    let lowest = metric_facts
        .lowest_complete
        .as_ref()
        .map(|point| {
            format!(
                "{} was lowest at {}",
                point.label,
                format_value(metric, point.value)
            )
        })
        .unwrap_or_else(|| "no complete-bucket minimum was available".to_owned());

    let increase = metric_facts
        .largest_increase
        .as_ref()
        .map(|change| {
            format!(
                "Largest increase: {} to {} ({:+.0}).",
                change.from.label, change.to.label, change.absolute_change
            )
        })
        .unwrap_or_else(|| "There was no increase across complete buckets.".to_owned());

    let decline = metric_facts
        .largest_decline
        .as_ref()
        .map(|change| {
            format!(
                "Largest decline: {} to {} ({:+.0}).",
                change.from.label, change.to.label, change.absolute_change
            )
        })
        .unwrap_or_else(|| "There was no decline across complete buckets.".to_owned());

    format!(
        "Across {} complete buckets, {} to {} {}. {}, while {}.\n\n{} {}{}",
        facts.complete_bucket_count,
        first.label,
        last.label,
        overall,
        peak,
        lowest,
        increase,
        decline,
        note
    )
}

fn multi_metric_integrity_note(plan: &AnalyticsPlan) -> &'static str {
    if plan.metrics.iter().all(|metric| metric.is_order_grain()) {
        " These calculations use independent order-grain aggregates, so line items do not multiply order value or AOV."
    } else {
        ""
    }
}

fn multi_metric_fact_summary(plan: &AnalyticsPlan, facts: &AnalyticalFacts) -> String {
    let mut summaries = Vec::new();
    let mut first_label = None::<String>;
    let mut last_label = None::<String>;

    for metric in &plan.metrics {
        let Some(metric_facts) = facts
            .metrics
            .iter()
            .find(|facts| facts.metric == metric.value_key())
        else {
            continue;
        };

        let (Some(first), Some(last)) = (
            metric_facts.first_complete.as_ref(),
            metric_facts.last_complete.as_ref(),
        ) else {
            continue;
        };

        first_label.get_or_insert_with(|| first.label.clone());

        last_label.get_or_insert_with(|| last.label.clone());

        let change = metric_facts
            .overall_change
            .as_ref()
            .and_then(|change| change.percent_change)
            .map_or_else(
                || {
                    if first.value == last.value {
                        format!("{} had no percentage change", metric_label(*metric))
                    } else {
                        format!(
                            "{} changed from a zero baseline; percentage change is not meaningful",
                            metric_label(*metric)
                        )
                    }
                },
                |percent| format!("{} changed {percent:+.1}%", metric_label(*metric)),
            );

        summaries.push(change);
    }

    if summaries.is_empty() {
        return format!(
            "No complete metric series were available for analytical comparison.{}",
            partial_bucket_note(facts)
        );
    }

    let first_label = first_label.unwrap_or_else(|| "first complete bucket".to_owned());

    let last_label = last_label.unwrap_or_else(|| "last complete bucket".to_owned());

    format!(
        "Across {} complete buckets from {} through {}, {}.{}{}",
        facts.complete_bucket_count,
        first_label,
        last_label,
        summaries.join(", while "),
        multi_metric_integrity_note(plan),
        partial_bucket_note(facts)
    )
}

fn partial_bucket_note(facts: &AnalyticalFacts) -> String {
    match (facts.partial_start_bucket, facts.partial_end_bucket) {
        (true, true) => {
            " The first and last buckets are partial and are excluded from analytical comparisons."
                .to_owned()
        }
        (true, false) => {
            " The first bucket is partial and is excluded from analytical comparisons.".to_owned()
        }
        (false, true) => {
            " The last bucket is partial and is excluded from analytical comparisons.".to_owned()
        }
        (false, false) => String::new(),
    }
}

fn comparison_summary(plan: &AnalyticsPlan, result: &QueryResult, metric: Metric) -> String {
    let key = metric.value_key();
    if plan.dimensions.is_empty() && result.row_count == 1 {
        let value = result.rows[0]
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if matches!(
            metric,
            Metric::OrderCount
                | Metric::UniqueCustomers
                | Metric::ProductOrderCount
                | Metric::ProductUniqueCustomers
                | Metric::CancelledOrderCount
        ) {
            return format!(
                "There were {} {} for {}.",
                format_value(metric, value),
                metric_label(metric),
                plan.periods[0].label
            );
        }
        return format!(
            "{} was {} for {}.",
            metric_label(metric),
            format_value(metric, value),
            plan.periods[0].label
        );
    }
    let points = result
        .rows
        .iter()
        .filter_map(|row| Some((row_label(row)?, row.get(key)?.as_f64()?)))
        .collect::<Vec<_>>();
    if points.is_empty() {
        return "No matching analytics data was found for the requested periods.".to_owned();
    }
    if points.len() == 1 {
        if matches!(
            metric,
            Metric::OrderCount
                | Metric::UniqueCustomers
                | Metric::ProductOrderCount
                | Metric::ProductUniqueCustomers
                | Metric::CancelledOrderCount
        ) {
            return format!(
                "There were {} {} for {}.",
                format_value(metric, points[0].1),
                metric_label(metric),
                points[0].0
            );
        }
        return format!(
            "{} was {} for {}.",
            metric_label(metric),
            format_value(metric, points[0].1),
            points[0].0
        );
    }
    let comparison = comparison_type(plan);
    let transitions = points
        .windows(2)
        .map(|pair| change_sentence(&pair[0], &pair[1], comparison))
        .collect::<Vec<_>>();
    let highest = points
        .iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .unwrap();
    let lowest = points
        .iter()
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .unwrap();
    let changes = points
        .windows(2)
        .map(|pair| (pair[1].1 - pair[0].1, &pair[0], &pair[1]))
        .collect::<Vec<_>>();
    let largest_increase = changes
        .iter()
        .filter(|change| change.0 > 0.0)
        .max_by(|left, right| left.0.total_cmp(&right.0));
    let largest_decline = changes
        .iter()
        .filter(|change| change.0 < 0.0)
        .min_by(|left, right| left.0.total_cmp(&right.0));
    let first = &points[0];
    let last = points.last().unwrap();
    let overall = percent_change(first.1, last.1).map_or_else(
        || {
            if last.1 > first.1 {
                "increased from zero; percentage change is not meaningful".to_owned()
            } else {
                "had no meaningful percentage change".to_owned()
            }
        },
        |change| format!("changed {change:+.1}%"),
    );
    let increase = largest_increase
        .map(|change| {
            format!(
                "Largest increase: {} to {} ({:+.0}).",
                change.1 .0, change.2 .0, change.0
            )
        })
        .unwrap_or_else(|| "There was no period increase.".to_owned());
    let decline = largest_decline
        .map(|change| {
            format!(
                "Largest decline: {} to {} ({:+.0}).",
                change.1 .0, change.2 .0, change.0
            )
        })
        .unwrap_or_else(|| "There was no period decline.".to_owned());
    format!(
        "{}\n\nOverall, {} to {} {}. {} was highest at {}, while {} was lowest at {}.\n\n{} {}",
        transitions.join(" "),
        first.0,
        last.0,
        overall,
        highest.0,
        format_value(metric, highest.1),
        lowest.0,
        format_value(metric, lowest.1),
        increase,
        decline
    )
}

fn multi_metric_summary(plan: &AnalyticsPlan, result: &QueryResult) -> String {
    let first = result.rows.first().unwrap();
    let last = result.rows.last().unwrap();
    let first_label = row_label(first).unwrap_or_else(|| "first period".to_owned());
    let last_label = row_label(last).unwrap_or_else(|| "last period".to_owned());
    let facts = plan
        .metrics
        .iter()
        .filter_map(|metric| {
            let from = first.get(metric.value_key())?.as_f64()?;
            let to = last.get(metric.value_key())?.as_f64()?;
            Some(percent_change(from, to).map_or_else(
                || {
                    if to == 0.0 {
                        format!("{} remained at zero", metric_label(*metric))
                    } else {
                        format!(
                            "{} increased from zero; percentage change is not meaningful",
                            metric_label(*metric)
                        )
                    }
                },
                |change| format!("{} changed {change:+.1}%", metric_label(*metric)),
            ))
        })
        .collect::<Vec<_>>();
    format!(
        "Across {} through {}, {}.{}",
        first_label,
        last_label,
        facts.join(", while "),
        multi_metric_integrity_note(plan)
    )
}

fn ranking_item_label(plan: &AnalyticsPlan, row: &serde_json::Map<String, Value>) -> String {
    let value = if plan.dimensions.contains(&Dimension::Product) {
        row.get("product_name")
            .or_else(|| row.get("historical_product_label"))
    } else {
        row.get("variant_title")
            .or_else(|| row.get("historical_product_label"))
            .or_else(|| row.get("variant_name"))
            .or_else(|| row.get("product_name"))
    };

    value
        .and_then(Value::as_str)
        .unwrap_or("Unnamed item")
        .to_owned()
}

fn multi_metric_ranking_summary(plan: &AnalyticsPlan, result: &QueryResult) -> String {
    let mut periods = Vec::<(String, Vec<&serde_json::Map<String, Value>>)>::new();

    for row in &result.rows {
        let period = row
            .get("period")
            .and_then(Value::as_str)
            .or_else(|| plan.periods.first().map(|period| period.label.as_str()))
            .unwrap_or("Requested period")
            .to_owned();

        if let Some(existing) = periods.iter_mut().find(|entry| entry.0 == period) {
            existing.1.push(row);
        } else {
            periods.push((period, vec![row]));
        }
    }

    periods
        .into_iter()
        .filter_map(|(period, rows)| {
            let leaders = plan
                .metrics
                .iter()
                .filter_map(|metric| {
                    let key = metric.value_key();

                    let (row, value) = rows
                        .iter()
                        .filter_map(|row| {
                            let value = row.get(key)?.as_f64()?;

                            Some((*row, value))
                        })
                        .max_by(|left, right| left.1.total_cmp(&right.1))?;

                    Some(format!(
                        "{} leads {} with {}",
                        ranking_item_label(plan, row),
                        metric_label(*metric),
                        format_value(*metric, value)
                    ))
                })
                .collect::<Vec<_>>();

            if leaders.is_empty() {
                None
            } else {
                Some(format!("For {period}, {}.", leaders.join("; ")))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn ranking_summary(plan: &AnalyticsPlan, result: &QueryResult) -> String {
    let metric = plan.metrics[0];
    let key = metric.value_key();

    let mut rankings = Vec::<(String, Vec<(String, f64)>)>::new();

    for row in &result.rows {
        let period = row
            .get("period")
            .and_then(Value::as_str)
            .or_else(|| plan.periods.first().map(|period| period.label.as_str()))
            .unwrap_or("Requested period")
            .to_owned();

        let item = ranking_item_label(plan, row);

        let value = row.get(key).and_then(Value::as_f64).unwrap_or(0.0);

        if let Some(existing) = rankings.iter_mut().find(|entry| entry.0 == period) {
            existing.1.push((item, value));
        } else {
            rankings.push((period, vec![(item, value)]));
        }
    }

    rankings
        .into_iter()
        .filter_map(|(period, points)| {
            let leader = points.first()?;

            let gap = points.get(1).map(|runner| leader.1 - runner.1);

            let mut summary = format!(
                "For {period}, {} leads with {}",
                leader.0,
                format_value(metric, leader.1)
            );

            if let Some(gap) = gap {
                summary.push_str(&format!(
                    ", ahead of the runner-up by {}",
                    format_value(metric, gap)
                ));
            }

            if ranking_metric_is_additive(metric) {
                let top3 = points.iter().take(3).map(|point| point.1).sum::<f64>();

                let top5 = points.iter().take(5).map(|point| point.1).sum::<f64>();

                let returned_total = points.iter().map(|point| point.1).sum::<f64>();

                summary.push_str(&format!(
                    "; top 3 total {}, top 5 total {}, and returned top-N total {}",
                    format_value(metric, top3),
                    format_value(metric, top5),
                    format_value(metric, returned_total)
                ));
            }

            summary.push('.');
            Some(summary)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn ranking_metric_is_additive(metric: Metric) -> bool {
    matches!(metric, Metric::QuantitySold | Metric::ProductSalesValue)
}

fn status_summary(plan: &AnalyticsPlan, result: &QueryResult) -> String {
    let key = plan.metrics[0].value_key();
    let status_key = [
        "operational_status",
        "order_status",
        "fulfillment_status",
        "payment_status",
        "delivery_status",
    ]
    .iter()
    .find(|key| result.columns.iter().any(|column| column == **key))
    .copied()
    .unwrap_or("operational_status");
    let mut periods = Vec::<(String, Vec<(String, f64)>)>::new();
    for row in &result.rows {
        let period = row
            .get("period")
            .and_then(Value::as_str)
            .unwrap_or("Requested period")
            .to_owned();
        let status = row
            .get(status_key)
            .and_then(Value::as_str)
            .unwrap_or("UNSPECIFIED")
            .to_owned();
        let value = row.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(existing) = periods.iter_mut().find(|entry| entry.0 == period) {
            existing.1.push((status, value));
        } else {
            periods.push((period, vec![(status, value)]));
        }
    }
    let descriptions = periods
        .iter()
        .filter_map(|(period, statuses)| {
            let total = statuses.iter().map(|status| status.1).sum::<f64>();
            let dominant = statuses
                .iter()
                .max_by(|left, right| left.1.total_cmp(&right.1))?;
            Some(format!(
                "{} was dominated by {} at {:.1}% ({} of {}).",
                period,
                dominant.0,
                if total == 0.0 {
                    0.0
                } else {
                    dominant.1 / total * 100.0
                },
                dominant.1,
                total
            ))
        })
        .collect::<Vec<_>>();
    let share_change = if periods.len() >= 2 {
        let first = &periods[0];
        let last = periods.last().unwrap();
        let first_total = first.1.iter().map(|row| row.1).sum::<f64>();
        let last_total = last.1.iter().map(|row| row.1).sum::<f64>();
        last.1
            .iter()
            .filter_map(|(status, value)| {
                let prior = first.1.iter().find(|row| row.0 == *status)?.1;
                let from = if first_total == 0.0 {
                    0.0
                } else {
                    prior / first_total * 100.0
                };
                let to = if last_total == 0.0 {
                    0.0
                } else {
                    *value / last_total * 100.0
                };
                Some((to - from, status))
            })
            .max_by(|left, right| left.0.abs().total_cmp(&right.0.abs()))
            .map(|change| {
                format!(
                    "The largest composition shift was {} at {:+.1} percentage points.",
                    change.1, change.0
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    format!("{} {}", descriptions.join(" "), share_change)
}

fn comparison_type(plan: &AnalyticsPlan) -> &'static str {
    if plan.dimensions.contains(&Dimension::Week)
        && (plan.periods.len() == 1
            || plan.periods.windows(2).all(|pair| {
                pair[0].end_exclusive == pair[1].start
                    && (pair[0].end_exclusive - pair[0].start) == Duration::days(7)
                    && (pair[1].end_exclusive - pair[1].start) == Duration::days(7)
            }))
    {
        return "WoW";
    }
    if plan.dimensions.contains(&Dimension::Month)
        && (plan.periods.len() == 1 || sequential_calendar_months(&plan.periods))
    {
        return "MoM";
    }
    if plan.periods.len() == 2 {
        let first = &plan.periods[0];
        let second = &plan.periods[1];
        if first.start.month() == second.start.month()
            && first.start.day() == second.start.day()
            && second.start.year() == first.start.year() + 1
            && (first.end_exclusive - first.start) == (second.end_exclusive - second.start)
        {
            return "YoY";
        }
    }
    if sequential_calendar_months(&plan.periods) {
        "MoM"
    } else {
        "period-over-period"
    }
}

fn sequential_calendar_months(periods: &[super::plan::TimePeriod]) -> bool {
    periods.len() > 1
        && periods.windows(2).all(|pair| {
            pair[0].end_exclusive == pair[1].start
                && pair[0].start.day() == 1
                && pair[1].start.day() == 1
        })
}

fn change_sentence(first: &(String, f64), second: &(String, f64), label: &str) -> String {
    percent_change(first.1, second.1).map_or_else(
        || {
            if second.1 == 0.0 {
                format!("{} to {} remained at zero.", first.0, second.0)
            } else {
                format!(
                    "{} increased from zero to {} in {}; percentage change is not meaningful.",
                    first.0, second.1, second.0
                )
            }
        },
        |change| {
            format!(
                "{} to {} changed {:+.1}% {}.",
                first.0, second.0, change, label
            )
        },
    )
}

fn percent_change(from: f64, to: f64) -> Option<f64> {
    (from != 0.0).then_some((to - from) / from * 100.0)
}

fn row_label(row: &serde_json::Map<String, Value>) -> Option<String> {
    [
        "period",
        "day",
        "week",
        "month",
        "variant_title",
        "variant_name",
        "product_name",
        "historical_product_label",
        "operational_status",
        "order_status",
        "fulfillment_status",
        "payment_status",
        "delivery_status",
        "store_name",
    ]
    .iter()
    .find_map(|key| row.get(*key).and_then(Value::as_str).map(str::to_owned))
}

fn metric_label(metric: Metric) -> &'static str {
    match metric {
        Metric::OrderCount => "orders",
        Metric::UniqueCustomers => "unique customers",
        Metric::QuantitySold => "quantity sold",
        Metric::UniqueVariants => "unique variants",
        Metric::OrderValue => "order value",
        Metric::AverageOrderValue => "AOV",
        Metric::AverageItemQuantityPerOrder => "average item quantity per order",
        Metric::CancelledOrderCount => "cancelled orders",
        Metric::CancellationRate => "cancellation rate",
        Metric::ProductOrderCount => "product orders",
        Metric::ProductUniqueCustomers => "unique product buyers",
        Metric::ProductSalesValue => "product sales value",
    }
}

fn format_value(metric: Metric, value: f64) -> String {
    match metric {
        Metric::OrderValue | Metric::AverageOrderValue | Metric::ProductSalesValue => {
            format_currency(value)
        }
        Metric::CancellationRate => format!("{value:.1}%"),
        _ => format!("{value:.0}"),
    }
}

fn format_currency(value: f64) -> String {
    let raw = format!("{:.2}", value.abs());
    let (integer, fraction) = raw.split_once('.').unwrap_or((&raw, "00"));
    let mut reversed = String::with_capacity(integer.len() + integer.len() / 3);
    for (index, digit) in integer.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(digit);
    }
    let grouped = reversed.chars().rev().collect::<String>();
    let sign = if value.is_sign_negative() { "-" } else { "" };
    format!("{sign}₹{grouped}.{fraction}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::plan::{
        AnalyticsDomain, AnalyticsFilters, SortSpec, TimePeriod, VisualizationIntent,
    };
    use chrono::NaiveDate;
    use serde_json::{Map, Value};

    #[test]
    fn monthly_insight_reports_all_required_comparison_facts() {
        let periods = [("April", 4), ("May", 5), ("June", 6), ("July", 7)]
            .map(|(label, month)| TimePeriod {
                label: label.to_owned(),
                start: NaiveDate::from_ymd_opt(2026, month, 1).unwrap(),
                end_exclusive: NaiveDate::from_ymd_opt(2026, month + 1, 1).unwrap(),
            })
            .to_vec();
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Period],
            periods,
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Bar),
        };
        let rows = [
            ("April", 1076),
            ("May", 1126),
            ("June", 1173),
            ("July", 1120),
        ]
        .map(|(label, value)| {
            let mut row = Map::new();
            row.insert("period".to_owned(), Value::from(label));
            row.insert("order_count".to_owned(), Value::from(value));
            row
        })
        .to_vec();
        let result = QueryResult {
            columns: vec!["period".to_owned(), "order_count".to_owned()],
            row_count: 4,
            rows,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        let summary = summarize_metric_query(&plan, &result);
        assert!(summary.contains("MoM"));
        assert!(summary.contains("Overall"));
        assert!(summary.contains("highest"));
        assert!(summary.contains("Largest increase"));
        assert!(summary.contains("Largest decline"));
    }

    #[test]
    fn yoy_requires_the_same_calendar_alignment() {
        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Period],
            periods: vec![
                TimePeriod {
                    label: "First".to_owned(),
                    start: NaiveDate::from_ymd_opt(2025, 5, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2025, 5, 8).unwrap(),
                },
                TimePeriod {
                    label: "Second".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 5, 22).unwrap(),
                },
            ],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Bar),
        };
        assert_eq!(comparison_type(&plan), "period-over-period");
        plan.periods[1].start = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        plan.periods[1].end_exclusive = NaiveDate::from_ymd_opt(2026, 5, 8).unwrap();
        assert_eq!(comparison_type(&plan), "YoY");
    }
    #[test]
    fn variant_ranking_uses_historical_label_instead_of_unnamed_item() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Products,
            metrics: vec![Metric::QuantitySold],
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

        let mut row = Map::new();
        row.insert("variant_id".to_owned(), Value::from("variant_1"));
        row.insert(
            "historical_product_label".to_owned(),
            Value::from("Sharbati Wheat Atta 5kg"),
        );
        row.insert("quantity_sold".to_owned(), Value::from(120));

        let result = QueryResult {
            columns: vec![
                "variant_id".to_owned(),
                "historical_product_label".to_owned(),
                "quantity_sold".to_owned(),
            ],
            rows: vec![row],
            row_count: 1,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let summary = summarize_metric_query(&plan, &result);

        assert!(summary.contains("Sharbati Wheat Atta 5kg"));
        assert!(!summary.contains("Unnamed item"));
    }

    #[test]
    fn weekly_summary_uses_complete_bucket_facts() {
        use crate::analytics::facts::build_time_series_facts;
        use crate::analytics::plan::{
            AnalyticsDomain, AnalyticsFilters, SortSpec, TimePeriod, VisualizationIntent,
        };
        use chrono::NaiveDate;
        use serde_json::{Map, Value};

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

        let rows = [
            ("2026-04-27", 120),
            ("2026-05-04", 258),
            ("2026-05-11", 259),
            ("2026-05-18", 254),
            ("2026-05-25", 235),
            ("2026-06-01", 237),
            ("2026-06-08", 234),
            ("2026-06-15", 273),
            ("2026-06-22", 331),
            ("2026-06-29", 338),
            ("2026-07-06", 363),
            ("2026-07-13", 375),
            ("2026-07-20", 382),
            ("2026-07-27", 291),
        ]
        .map(|(week, count)| {
            let mut row = Map::new();
            row.insert("week".to_owned(), Value::String(week.to_owned()));
            row.insert("order_count".to_owned(), Value::from(count));
            row
        })
        .to_vec();

        let result = QueryResult {
            columns: vec!["week".to_owned(), "order_count".to_owned()],
            row_count: rows.len(),
            rows,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let facts = build_time_series_facts(&plan, &result, true, true).unwrap();

        let summary = summarize_metric_query_with_facts(&plan, &result, Some(&facts));

        assert!(summary.contains("12 complete buckets"));
        assert!(summary.contains("2026-05-04"));
        assert!(summary.contains("2026-07-20"));
        assert!(summary.contains("Largest increase: 2026-06-15 to 2026-06-22 (+58)"));
        assert!(summary.contains("Largest decline: 2026-05-18 to 2026-05-25 (-19)"));
        assert!(summary.contains("The first and last buckets are partial"));

        // Critical regression:
        // the partial ending week must not become the decline claim.
        assert!(!summary.contains("Largest decline: 2026-07-20 to 2026-07-27"));
    }
    #[test]
    fn product_multi_metric_summary_does_not_claim_order_grain_aggregation() {
        assert_eq!(
            multi_metric_integrity_note(&AnalyticsPlan {
                domain: AnalyticsDomain::Products,
                metrics: vec![
                    Metric::QuantitySold,
                    Metric::ProductOrderCount,
                    Metric::ProductUniqueCustomers,
                    Metric::ProductSalesValue,
                ],
                dimensions: vec![Dimension::Month],
                periods: vec![TimePeriod {
                    label: "May through July".to_owned(),
                    start: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
                    end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
                }],
                filters: AnalyticsFilters::default(),
                sort: Some(SortSpec::Chronological),
                limit: None,
                visualization: Some(VisualizationIntent::Line),
            }),
            ""
        );
    }
    #[test]
    fn multi_metric_variant_ranking_reports_each_metric_independently() {
        let plan = AnalyticsPlan {
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
                product: Some("MP Sharbati Atta".to_owned()),
                ..AnalyticsFilters::default()
            },
            sort: Some(SortSpec::Descending),
            limit: None,
            visualization: Some(VisualizationIntent::Table),
        };

        let mut regular = Map::new();

        regular.insert("variant_id".to_owned(), Value::from("variant_regular"));

        regular.insert(
            "variant_title".to_owned(),
            Value::from("Sharbati Regular - 5 kg"),
        );

        regular.insert("quantity_sold".to_owned(), Value::from(100));

        regular.insert("product_order_count".to_owned(), Value::from(50));

        regular.insert("product_unique_customers".to_owned(), Value::from(40));

        regular.insert("product_sales_value".to_owned(), Value::from(10_000.0));

        let mut fine = Map::new();

        fine.insert("variant_id".to_owned(), Value::from("variant_fine"));

        fine.insert(
            "variant_title".to_owned(),
            Value::from("Sharbati Fine - 5 kg"),
        );

        fine.insert("quantity_sold".to_owned(), Value::from(80));

        fine.insert("product_order_count".to_owned(), Value::from(60));

        fine.insert("product_unique_customers".to_owned(), Value::from(45));

        fine.insert("product_sales_value".to_owned(), Value::from(12_000.0));

        let result = QueryResult {
            columns: vec![
                "variant_id".to_owned(),
                "variant_title".to_owned(),
                "quantity_sold".to_owned(),
                "product_order_count".to_owned(),
                "product_unique_customers".to_owned(),
                "product_sales_value".to_owned(),
            ],
            rows: vec![regular, fine],
            row_count: 2,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let summary = summarize_metric_query(&plan, &result);

        assert!(summary.contains("Sharbati Regular - 5 kg leads quantity sold with 100"));

        assert!(summary.contains("Sharbati Fine - 5 kg leads product orders with 60"));

        assert!(summary.contains("Sharbati Fine - 5 kg leads unique product buyers with 45"));

        assert!(summary.contains("Sharbati Fine - 5 kg leads product sales value with ₹12,000.00"));

        // Product-order and buyer counts are non-additive.
        assert!(!summary.contains("top 3 total"));
        assert!(!summary.contains("top 5 total"));
    }
}
