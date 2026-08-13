use super::analytics::AnalyticalShape;
use crate::database::QueryResult;
use serde::Serialize;
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SummaryAnalysis {
    pub(crate) answer: String,
    pub(crate) facts: Value,
}

pub(crate) fn analyze(
    question: &str,
    shape: AnalyticalShape,
    result: &QueryResult,
) -> SummaryAnalysis {
    if result.rows.is_empty() {
        return SummaryAnalysis {
            answer: "No matching data was found for that request.".to_owned(),
            facts: json!({"row_count": 0}),
        };
    }
    match shape {
        AnalyticalShape::Scalar => scalar_summary(question, result),
        AnalyticalShape::Comparison => comparison_summary(result),
        AnalyticalShape::TimeSeries => time_series_summary(result),
        AnalyticalShape::CategoryBreakdown => category_summary(result),
        AnalyticalShape::Ranking => ranking_summary(result),
        AnalyticalShape::RecordList => record_summary(question, result),
    }
}

pub(crate) fn analyze_metrics(
    question: &str,
    shape: AnalyticalShape,
    metrics: &[String],
    result: &QueryResult,
) -> SummaryAnalysis {
    if metrics.len() <= 1 {
        return analyze(question, shape, result);
    }
    multi_metric_summary(metrics, result)
}

fn multi_metric_summary(metrics: &[String], result: &QueryResult) -> SummaryAnalysis {
    if result.rows.is_empty() {
        return SummaryAnalysis {
            answer: "No matching data was found for that request.".to_owned(),
            facts: json!({"row_count": 0}),
        };
    }
    let labels = result
        .rows
        .iter()
        .filter_map(first_dimension)
        .collect::<Vec<_>>();
    let mut metric_facts = Map::new();
    let mut sentences = Vec::new();
    for metric in metrics {
        let points = result
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                Some((
                    labels
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| (index + 1).to_string()),
                    row.get(metric)?.as_f64()?,
                ))
            })
            .collect::<Vec<_>>();
        if points.is_empty() {
            continue;
        }
        let total = points.iter().map(|(_, value)| value).sum::<f64>();
        let average = total / points.len() as f64;
        let minimum = points
            .iter()
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .expect("points are non-empty");
        let maximum = points
            .iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .expect("points are non-empty");
        let changes = points
            .windows(2)
            .map(|pair| {
                let difference = pair[1].1 - pair[0].1;
                json!({
                    "from": pair[0].0,
                    "to": pair[1].0,
                    "absolute_change": difference,
                    "percentage_change": (pair[0].1.abs() > f64::EPSILON)
                        .then_some(difference / pair[0].1 * 100.0)
                })
            })
            .collect::<Vec<_>>();
        metric_facts.insert(
            metric.clone(),
            json!({
                "total": total,
                "average": average,
                "minimum": {"label": minimum.0, "value": minimum.1},
                "maximum": {"label": maximum.0, "value": maximum.1},
                "sequential_changes": changes
            }),
        );
        match metric.as_str() {
            "order_count" => sentences.push(format!(
                "{} qualifying orders were recorded across {} periods.",
                format_number(total),
                points.len()
            )),
            "order_value" => sentences.push(format!(
                "Qualifying order value totaled ₹{} (using the documented computed fallback where no active payment was available).",
                format_decimal(total, 2)
            )),
            "average_order_value" => {}
            _ => sentences.push(format!(
                "{} totaled {} with a period average of {}.",
                metric.replace('_', " "),
                format_number(total),
                format_decimal(average, 2)
            )),
        }
    }
    let order_total = metric_facts
        .get("order_count")
        .and_then(|facts| facts["total"].as_f64());
    let value_total = metric_facts
        .get("order_value")
        .and_then(|facts| facts["total"].as_f64());
    if let (Some(orders), Some(value)) = (order_total, value_total) {
        if orders.abs() > f64::EPSILON {
            let weighted_aov = value / orders;
            metric_facts.insert(
                "weighted_average_order_value".to_owned(),
                json!(weighted_aov),
            );
            sentences.push(format!(
                "Weighted qualifying average order value was ₹{} (using the documented computed fallback where needed).",
                format_decimal(weighted_aov, 2)
            ));
        }
    }
    SummaryAnalysis {
        answer: sentences.join("\n\n"),
        facts: json!({
            "bucket_count": result.row_count,
            "metrics": metric_facts
        }),
    }
}

fn scalar_summary(question: &str, result: &QueryResult) -> SummaryAnalysis {
    let (key, value) = first_number(&result.rows[0]).unwrap_or(("value", 0.0));
    let normalized = question.to_lowercase();
    let noun = if key.contains("average") && normalized.contains("line item") {
        "average line items per order".to_owned()
    } else if key.contains("average") {
        key.replace('_', " ")
    } else if normalized.contains("sku") || normalized.contains("variant") {
        "distinct variants sold".to_owned()
    } else if normalized.contains("customer") {
        "customers matched".to_owned()
    } else if normalized.contains("order") {
        "orders matched".to_owned()
    } else {
        key.replace('_', " ")
    };
    SummaryAnalysis {
        answer: format!("{} {}.", format_number(value), noun),
        facts: json!({"metric": key, "value": value}),
    }
}

fn comparison_summary(result: &QueryResult) -> SummaryAnalysis {
    let points = dimension_metric_points(result);
    if points.len() < 2 {
        return generic_summary(result);
    }
    let (first_label, first) = &points[0];
    let (second_label, second) = &points[1];
    let difference = second - first;
    let percentage_change = if first.abs() > f64::EPSILON {
        Some(difference / first * 100.0)
    } else {
        None
    };
    let first_sentence = format!(
        "{} recorded {} versus {} on {}.",
        first_label,
        format_number(*first),
        format_number(*second),
        second_label
    );
    let second_sentence = if let Some(percentage) = percentage_change {
        let (article, direction) = if difference >= 0.0 {
            ("an", "increase")
        } else {
            ("a", "decrease")
        };
        format!(
            "That is {article} {} of {} ({}%).",
            direction,
            format_number(difference.abs()),
            format_decimal(percentage.abs(), 1)
        )
    } else {
        format!(
            "The absolute difference was {}; percentage change is undefined from a zero baseline.",
            format_number(difference.abs())
        )
    };
    SummaryAnalysis {
        answer: format!("{first_sentence}\n\n{second_sentence}"),
        facts: json!({
            "first": {"label": first_label, "value": first},
            "second": {"label": second_label, "value": second},
            "absolute_difference": difference.abs(),
            "percentage_change": percentage_change
        }),
    }
}

fn time_series_summary(result: &QueryResult) -> SummaryAnalysis {
    let points = dimension_metric_points(result);
    if points.is_empty() {
        return generic_summary(result);
    }
    let total = points.iter().map(|(_, value)| value).sum::<f64>();
    let minimum = points
        .iter()
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .expect("points is non-empty");
    let maximum = points
        .iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .expect("points is non-empty");
    let average = total / points.len() as f64;
    let mut answer = format!(
            "The period recorded {} orders across {} returned time buckets.\n\nThe highest volume was {} on {}, while the lowest was {} on {}.",
            format_number(total),
            points.len(),
            format_number(maximum.1),
            maximum.0,
            format_number(minimum.1),
            minimum.0
        );
    let pattern = period_pattern(&points);
    if let Some(narrative) = pattern["narrative"].as_str() {
        answer.push_str("\n\n");
        answer.push_str(narrative);
    }
    SummaryAnalysis {
        answer,
        facts: json!({
            "total": total,
            "average": average,
            "bucket_count": points.len(),
            "minimum": {"label": minimum.0, "value": minimum.1},
            "maximum": {"label": maximum.0, "value": maximum.1},
            "period_pattern": pattern
        }),
    }
}

fn period_pattern(points: &[(String, f64)]) -> Value {
    if !(4..=7).contains(&points.len()) {
        return Value::Null;
    }
    let changes = points
        .windows(2)
        .map(|pair| {
            let absolute = pair[1].1 - pair[0].1;
            let percentage =
                (pair[0].1.abs() > f64::EPSILON).then_some(absolute / pair[0].1 * 100.0);
            (pair[0].0.clone(), pair[1].0.clone(), absolute, percentage)
        })
        .collect::<Vec<_>>();
    let directions = changes
        .iter()
        .map(|change| change.2.total_cmp(&0.0))
        .collect::<Vec<_>>();
    let reversals = directions
        .windows(2)
        .filter(|pair| {
            !matches!(pair[0], std::cmp::Ordering::Equal)
                && !matches!(pair[1], std::cmp::Ordering::Equal)
                && pair[0] != pair[1]
        })
        .count();
    let largest = changes
        .iter()
        .max_by(|left, right| left.2.abs().total_cmp(&right.2.abs()))
        .expect("four points produce changes");
    let overall = points.last().expect("four points exist").1 - points[0].1;
    let overall_percentage =
        (points[0].1.abs() > f64::EPSILON).then_some(overall / points[0].1 * 100.0);
    let up_intervals = directions
        .iter()
        .filter(|direction| matches!(direction, std::cmp::Ordering::Greater))
        .count();
    let down_intervals = directions
        .iter()
        .filter(|direction| matches!(direction, std::cmp::Ordering::Less))
        .count();
    let direction_sentence = match (up_intervals, down_intervals) {
        (up, 0) => format!("Volume rose in all {up} sequential intervals."),
        (0, down) => format!("Volume fell in all {down} sequential intervals."),
        (up, down) => format!(
            "Volume rose in {up} sequential intervals and fell in {down}, with {reversals} direction reversal{}.",
            if reversals == 1 { "" } else { "s" }
        ),
    };
    let overall_sentence = overall_percentage.map_or_else(
        || {
            format!(
                "The final period differed from the first by {} orders.",
                format_number(overall.abs())
            )
        },
        |percentage| {
            format!(
                "The final period was {} orders ({}%) {} the first.",
                format_number(overall.abs()),
                format_decimal(percentage.abs(), 1),
                if overall >= 0.0 { "above" } else { "below" }
            )
        },
    );
    let largest_sentence = largest.3.map_or_else(
        || {
            format!(
                "The largest sequential movement was {} orders from {} to {}; that transition is worth investigating.",
                format_number(largest.2.abs()), largest.0, largest.1
            )
        },
        |percentage| {
            format!(
                "The largest sequential movement was {} orders ({}%) from {} to {}; that transition is worth investigating.",
                format_number(largest.2.abs()),
                format_decimal(percentage.abs(), 1),
                largest.0,
                largest.1
            )
        },
    );
    json!({
        "sequential_changes": changes.iter().map(|change| json!({
            "from": change.0,
            "to": change.1,
            "absolute_change": change.2,
            "percentage_change": change.3
        })).collect::<Vec<_>>(),
        "up_intervals": up_intervals,
        "down_intervals": down_intervals,
        "reversal_count": reversals,
        "overall_absolute_change": overall,
        "overall_percentage_change": overall_percentage,
        "largest_movement": {
            "from": largest.0,
            "to": largest.1,
            "absolute_change": largest.2,
            "percentage_change": largest.3
        },
        "narrative": format!("{direction_sentence} {overall_sentence} {largest_sentence}")
    })
}

fn category_summary(result: &QueryResult) -> SummaryAnalysis {
    let mut points = dimension_metric_points(result);
    if points.is_empty() {
        return generic_summary(result);
    }
    points.sort_by(|left, right| right.1.total_cmp(&left.1));
    let (leader, leader_value) = &points[0];
    let follow_up = points.get(1).map(|(label, value)| {
        format!(
            "The next-largest group was {} with {}.",
            label,
            format_number(*value)
        )
    });
    SummaryAnalysis {
        answer: follow_up.map_or_else(
            || {
                format!(
                    "{} was the largest group with {} orders.",
                    leader,
                    format_number(*leader_value)
                )
            },
            |sentence| {
                format!(
                    "{} was the largest group with {} orders.\n\n{}",
                    leader,
                    format_number(*leader_value),
                    sentence
                )
            },
        ),
        facts: json!({
            "largest": {"label": leader, "value": leader_value},
            "second_largest": points.get(1).map(|(label, value)| json!({"label": label, "value": value}))
        }),
    }
}

fn ranking_summary(result: &QueryResult) -> SummaryAnalysis {
    let points = dimension_metric_points(result);
    if points.is_empty() {
        return generic_summary(result);
    }
    let top_five_total = points.iter().take(5).map(|(_, value)| value).sum::<f64>();
    SummaryAnalysis {
        answer: format!(
            "{} ranked first with {}.\n\nThe top {} entries accounted for {} combined.",
            points[0].0,
            format_number(points[0].1),
            points.len().min(5),
            format_number(top_five_total)
        ),
        facts: json!({
            "leader": {"label": points[0].0, "value": points[0].1},
            "top_five_total": top_five_total,
            "ranked_row_count": points.len()
        }),
    }
}

fn record_summary(question: &str, result: &QueryResult) -> SummaryAnalysis {
    let noun = if question.to_lowercase().contains("customer") {
        "customer records"
    } else {
        "order records"
    };
    let qualifier = if result.truncated {
        " The table is truncated at the read-only analytics limit."
    } else {
        ""
    };
    SummaryAnalysis {
        answer: format!("Found {} {noun}.{qualifier}", result.row_count),
        facts: json!({"matched_record_count": result.row_count, "truncated": result.truncated}),
    }
}

fn generic_summary(result: &QueryResult) -> SummaryAnalysis {
    SummaryAnalysis {
        answer: format!(
            "Found {} matching rows. The results are shown below.",
            result.row_count
        ),
        facts: json!({"row_count": result.row_count}),
    }
}

fn dimension_metric_points(result: &QueryResult) -> Vec<(String, f64)> {
    result
        .rows
        .iter()
        .filter_map(|row| {
            let label = first_dimension(row)?;
            let (_, value) = first_number(row)?;
            Some((label, value))
        })
        .collect()
}

fn first_dimension(row: &Map<String, Value>) -> Option<String> {
    if let Some(product) = row.get("product_name").and_then(Value::as_str) {
        let variant = row.get("variant_name").and_then(Value::as_str);
        return Some(match variant.filter(|value| !value.trim().is_empty()) {
            Some(variant) if !variant.eq_ignore_ascii_case(product) => {
                format!("{product} — {variant}")
            }
            _ => product.to_owned(),
        });
    }
    row.iter()
        .find(|(_, value)| value.is_string() || value.is_boolean())
        .and_then(|(_, value)| match value {
            Value::String(value) => Some(value.clone()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        })
}

fn first_number(row: &Map<String, Value>) -> Option<(&str, f64)> {
    row.iter()
        .filter(|(key, _)| is_business_metric_key(key))
        .find_map(|(key, value)| value.as_f64().map(|value| (key.as_str(), value)))
}

fn is_business_metric_key(key: &str) -> bool {
    let normalized = key.to_lowercase();
    !normalized.ends_with("_id")
        && !normalized.ends_with("_rank")
        && !normalized.starts_with("rank")
        && !normalized.contains("display_id")
}

fn format_number(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        let digits = format!("{:.0}", value.abs());
        let grouped = digits
            .chars()
            .rev()
            .enumerate()
            .fold(String::new(), |mut output, (index, character)| {
                if index > 0 && index % 3 == 0 {
                    output.push(',');
                }
                output.push(character);
                output
            })
            .chars()
            .rev()
            .collect::<String>();
        if value.is_sign_negative() {
            format!("-{grouped}")
        } else {
            grouped
        }
    } else {
        format_decimal(value, 2)
    }
}

fn format_decimal(value: f64, precision: usize) -> String {
    format!("{value:.precision$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(rows: Vec<Map<String, Value>>) -> QueryResult {
        QueryResult {
            columns: vec!["order_date".to_owned(), "order_count".to_owned()],
            row_count: rows.len(),
            rows,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        }
    }

    #[test]
    fn calculates_comparison_difference_and_percentage() {
        let rows = [("2026-05-01", 124), ("2026-05-02", 151)]
            .into_iter()
            .map(|(date, count)| {
                let mut row = Map::new();
                row.insert("order_date".to_owned(), Value::String(date.to_owned()));
                row.insert("order_count".to_owned(), Value::from(count));
                row
            })
            .collect();
        let summary = analyze("Compare orders", AnalyticalShape::Comparison, &result(rows));
        assert!(summary.answer.contains("27"));
        assert!(summary.answer.contains("21.8%"));
    }

    #[test]
    fn ranking_summary_ignores_numeric_rank_and_identifier_helpers() {
        let rows = [("First", 1, 473), ("Second", 2, 320)]
            .into_iter()
            .map(|(name, rank, quantity)| {
                Map::from_iter([
                    ("product_name".to_owned(), Value::from(name)),
                    ("product_rank".to_owned(), Value::from(rank)),
                    ("quantity_sold".to_owned(), Value::from(quantity)),
                ])
            })
            .collect::<Vec<_>>();
        let result = QueryResult {
            columns: vec![
                "product_name".to_owned(),
                "product_rank".to_owned(),
                "quantity_sold".to_owned(),
            ],
            row_count: rows.len(),
            rows,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };
        let analysis = analyze("top products", AnalyticalShape::Ranking, &result);
        assert!(analysis.answer.contains("473"));
        assert!(!analysis.answer.contains("with 1."));
    }

    #[test]
    fn calculates_time_series_extrema_from_database_rows() {
        let rows = [("2026-06-01", 10), ("2026-06-02", 18), ("2026-06-03", 7)]
            .into_iter()
            .map(|(date, count)| {
                let mut row = Map::new();
                row.insert("order_date".to_owned(), Value::String(date.to_owned()));
                row.insert("order_count".to_owned(), Value::from(count));
                row
            })
            .collect();
        let summary = analyze("Daily orders", AnalyticalShape::TimeSeries, &result(rows));
        assert!(summary.answer.contains("35 orders"));
        assert!(summary.answer.contains("18 on 2026-06-02"));
        assert!(summary.answer.contains("7 on 2026-06-03"));
    }

    #[test]
    fn labels_generated_average_metrics_from_the_result_key() {
        let mut row = Map::new();
        row.insert("average_items".to_owned(), Value::from(3.0));
        let summary = analyze(
            "What is the average number of line items per order?",
            AnalyticalShape::Scalar,
            &result(vec![row]),
        );
        assert_eq!(summary.answer, "3 average line items per order.");
    }
}
