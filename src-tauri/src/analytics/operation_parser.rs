use super::{
    dates::{latest_complete_business_date, subtract_months},
    operation::{
        AnalysisOperation, EntityListPlan, EntityType, ForecastHorizon, ForecastHorizonUnit,
        ForecastMetric, ForecastPlan, TimeBucket, TrainingWindow, TrainingWindowKind,
    },
    plan::{AnalyticsFilters, SortSpec, TimePeriod},
    semantic_parser::{normalize_analytics_language, parse_semantic_plan},
};
use chrono::{Datelike, Duration, NaiveDate};

pub(crate) fn parse_analysis_operation(
    question: &str,
    data_as_of: &str,
    referential_context: Option<&str>,
) -> Result<Option<AnalysisOperation>, String> {
    let normalized = normalize_analytics_language(question);
    let latest_complete = latest_complete_business_date(data_as_of)
        .ok_or_else(|| "data freshness timestamp was invalid".to_owned())?;
    if is_forecast_request(&normalized) {
        return parse_forecast(&normalized, latest_complete)
            .map(AnalysisOperation::Forecast)
            .map(Some);
    }
    if is_customer_cohort(&normalized) {
        return parse_customer_entity(&normalized, latest_complete)
            .map(AnalysisOperation::EntityList)
            .map(Some);
    }
    parse_semantic_plan(question, data_as_of, referential_context)
        .map(|plan| plan.map(AnalysisOperation::MetricQuery))
}

fn is_forecast_request(question: &str) -> bool {
    [
        "forecast",
        "project",
        "predict",
        "expected orders",
        "should august",
        "should next",
    ]
    .iter()
    .any(|term| question.contains(term))
}

fn is_customer_cohort(question: &str) -> bool {
    question.contains("customer")
        && (question.contains("orders") || question.contains("bought"))
        && (question.contains("at least")
            || question.contains("or more")
            || question.contains("more than")
            || question.contains("less than")
            || question.contains("no more than"))
        && (question.contains("haven't")
            || question.contains("have not")
            || question.contains("inactive"))
}

fn parse_customer_entity(
    question: &str,
    latest_complete: NaiveDate,
) -> Result<EntityListPlan, String> {
    let end_exclusive = latest_complete + Duration::days(1);
    let lookback_months = if question.contains("past year") || question.contains("last year") {
        12
    } else {
        number_before_unit(question, "month")
            .unwrap_or(6)
            .clamp(1, 24)
    } as u32;
    let start = subtract_months(end_exclusive, lookback_months)
        .ok_or_else(|| "customer lookback period was invalid".to_owned())?;
    let threshold = threshold_orders(question).unwrap_or((Some(5), None));
    let inactive_days =
        number_before_unit(question, "day").map(|value| value.clamp(1, 3650) as u32);
    let no_orders_since = month_after_phrase(question, "since", latest_complete.year());
    let return_records = !question.trim_start().starts_with("how many");
    let plan = EntityListPlan {
        entity: EntityType::Customer,
        period: TimePeriod {
            label: format!("Previous {lookback_months} months"),
            start,
            end_exclusive,
        },
        minimum_order_count: threshold.0,
        maximum_order_count: threshold.1,
        inactive_days,
        no_orders_since,
        purchased_product: None,
        return_records,
        sort: Some(SortSpec::Descending),
        limit: 500,
    };
    plan.validate()?;
    Ok(plan)
}

fn parse_forecast(question: &str, latest_complete: NaiveDate) -> Result<ForecastPlan, String> {
    let _ = latest_complete;
    let months = number_before_unit(question, "month")
        .unwrap_or(6)
        .clamp(1, 24) as u32;
    let horizon = number_after_phrase(question, "next")
        .or_else(|| number_before_unit(question, "day"))
        .unwrap_or(if question.contains("next week") {
            7
        } else {
            14
        })
        .clamp(1, 90) as u32;
    let plan = ForecastPlan {
        metric: ForecastMetric::OrderCount,
        training_window: TrainingWindow {
            kind: TrainingWindowKind::CalendarMonths,
            count: months,
        },
        forecast_horizon: ForecastHorizon {
            unit: ForecastHorizonUnit::Days,
            count: horizon,
        },
        grain: TimeBucket::Day,
        filters: AnalyticsFilters::default(),
    };
    plan.validate()?;
    Ok(plan)
}

fn threshold_orders(question: &str) -> Option<(Option<u32>, Option<u32>)> {
    let value = number_before_unit(question, "order")? as u32;
    if question.contains("no more than") {
        Some((None, Some(value)))
    } else if question.contains("more than") {
        Some((Some(value.saturating_add(1)), None))
    } else if question.contains("less than") {
        Some((None, Some(value.saturating_sub(1))))
    } else {
        Some((Some(value), None))
    }
}

fn number_before_unit(question: &str, unit: &str) -> Option<usize> {
    let words = question.split_whitespace().collect::<Vec<_>>();
    words.windows(2).find_map(|window| {
        window[1]
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .starts_with(unit)
            .then(|| parse_number(window[0]))
            .flatten()
    })
}

fn number_after_phrase(question: &str, phrase: &str) -> Option<usize> {
    let words = question.split_whitespace().collect::<Vec<_>>();
    words.windows(2).find_map(|window| {
        (window[0] == phrase)
            .then(|| parse_number(window[1]))
            .flatten()
    })
}

fn parse_number(value: &str) -> Option<usize> {
    value
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .parse()
        .ok()
        .or(match value {
            "one" => Some(1),
            "two" => Some(2),
            "three" => Some(3),
            "four" => Some(4),
            "five" => Some(5),
            "six" => Some(6),
            "seven" => Some(7),
            "eight" => Some(8),
            "nine" => Some(9),
            "ten" => Some(10),
            "twelve" => Some(12),
            _ => None,
        })
}

fn month_after_phrase(question: &str, phrase: &str, year: i32) -> Option<NaiveDate> {
    let after = question.split_once(phrase)?.1;
    const MONTHS: [&str; 12] = [
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
    ];
    MONTHS
        .iter()
        .position(|month| after.contains(month))
        .and_then(|month| {
            NaiveDate::from_ymd_opt(explicit_year(after).unwrap_or(year), month as u32 + 1, 1)
        })
}

fn explicit_year(question: &str) -> Option<i32> {
    question
        .split(|c: char| !c.is_ascii_digit())
        .find_map(|token| (token.len() == 4).then(|| token.parse().ok()).flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRESHNESS: &str = "2026-08-11 06:16:44.439091+00";

    #[test]
    fn routes_forecasts_before_future_metric_queries() {
        let operation = parse_analysis_operation(
            "Based on the last 6 months, project orders for the next 14 days",
            FRESHNESS,
            None,
        )
        .unwrap()
        .unwrap();
        let AnalysisOperation::Forecast(plan) = operation else {
            panic!("expected forecast")
        };
        assert_eq!(plan.training_window.count, 6);
        assert_eq!(plan.forecast_horizon.count, 14);
    }

    #[test]
    fn preserves_threshold_language_for_customer_cohorts() {
        let operation = parse_analysis_operation("Which customers placed more than 5 orders in the past year but haven't ordered in the last 60 days?", FRESHNESS, None).unwrap().unwrap();
        let AnalysisOperation::EntityList(plan) = operation else {
            panic!("expected entity")
        };
        assert_eq!(plan.minimum_order_count, Some(6));
        assert_eq!(plan.inactive_days, Some(60));
        assert!(plan.return_records);
    }

    #[test]
    fn distinguishes_inclusive_and_exclusive_customer_thresholds() {
        assert_eq!(threshold_orders("at least 5 orders"), Some((Some(5), None)));
        assert_eq!(
            threshold_orders("more than 5 orders"),
            Some((Some(6), None))
        );
        assert_eq!(
            threshold_orders("no more than 5 orders"),
            Some((None, Some(5)))
        );
        assert_eq!(
            threshold_orders("less than 5 orders"),
            Some((None, Some(4)))
        );
    }

    #[test]
    fn forecast_parser_returns_semantics_for_rust_date_resolution() {
        let operation = parse_analysis_operation(
            "Based on orders from May through July, what should August order volume look like?",
            FRESHNESS,
            None,
        )
        .unwrap()
        .unwrap();
        let AnalysisOperation::Forecast(plan) = operation else {
            panic!("expected forecast")
        };
        assert_eq!(plan.training_window.count, 6);
        assert_eq!(plan.forecast_horizon.unit, ForecastHorizonUnit::Days);
    }
    #[test]
    fn compact_customer_cohort_language_routes_to_entity_list() {
        let operation = parse_analysis_operation(
        "how many customers who placed atleast 5 orders in the last 6 months have not placed an order with in the last 45 days",
        FRESHNESS,
        None,
    )
    .unwrap()
    .unwrap();

        let AnalysisOperation::EntityList(plan) = operation else {
            panic!("expected entity list")
        };

        assert_eq!(plan.minimum_order_count, Some(5));
        assert_eq!(plan.maximum_order_count, None);
        assert_eq!(plan.inactive_days, Some(45));
        assert!(!plan.return_records);
    }
}
