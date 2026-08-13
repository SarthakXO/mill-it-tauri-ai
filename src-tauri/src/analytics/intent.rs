use super::{
    dates::{
        complete_days_range, latest_complete_business_date, month_range, previous_complete_month,
    },
    semantic_parser::normalize_analytics_language,
    types::{DateRange, FastToolKind, PerformanceDirection, TimeBucket, ToolPlan},
};
use chrono::{Datelike, Duration, Months, NaiveDate};

pub(crate) fn detect_fast_tool(question: &str) -> Option<FastToolKind> {
    let raw = question.to_lowercase();
    let normalized = normalize_analytics_language(question);
    if normalized.contains("customer")
        && (normalized.contains("haven't ordered")
            || normalized.contains("have not ordered")
            || normalized.contains("inactive")
            || normalized.contains("frequent customers") && normalized.contains("recently"))
    {
        Some(FastToolKind::LapsedCustomers)
    } else if normalized.contains("reorder")
        || normalized.contains("repeat demand")
        || normalized.contains("repeat-order")
        || normalized.contains("usual reorder")
        || normalized.contains("due to order again")
        || normalized.contains("expected order date")
        || normalized.contains("overdue order")
    {
        Some(FastToolKind::ReorderDemand)
    } else if normalized.contains("busy day")
        || normalized.contains("busy days")
        || normalized.contains("forecast order")
        || normalized.contains("operations prepare")
    {
        Some(FastToolKind::BusyDayForecast)
    } else if normalized.contains("underperform")
        || normalized.contains("performing better")
        || normalized.contains("perform better")
        || normalized.contains("performed better")
        || normalized.contains("performed worse")
        || normalized.contains("doing worse")
        || normalized.contains("declined")
        || normalized.contains("overperform")
    {
        Some(FastToolKind::ProductPerformance)
    } else if (raw.contains("sku") || raw.contains("variant") || raw.contains("sellable item"))
        && (normalized.contains("top ")
            || normalized.contains("best selling")
            || normalized.contains("best-selling")
            || normalized.contains("most selling")
            || normalized.contains("sold the most"))
    {
        Some(FastToolKind::TopVariants)
    } else if normalized.contains("order") && normalized.contains("status") {
        Some(FastToolKind::StatusBreakdown)
    } else if normalized.contains("order")
        && (normalized.contains("daily")
            || normalized.contains("weekly")
            || normalized.contains("monthly")
            || normalized.contains("by day")
            || normalized.contains("by week")
            || normalized.contains("by month")
            || normalized.contains("plot")
            || normalized.contains("time series"))
    {
        Some(FastToolKind::OrderTimeseries)
    } else if normalized.contains("order")
        && (normalized.contains("how many")
            || normalized.contains("order count")
            || normalized.starts_with("count orders"))
    {
        Some(FastToolKind::OrderCount)
    } else {
        None
    }
}

pub(crate) fn detect_specialized_tool(question: &str) -> Option<FastToolKind> {
    match detect_fast_tool(question) {
        Some(
            tool @ (FastToolKind::LapsedCustomers
            | FastToolKind::ReorderDemand
            | FastToolKind::BusyDayForecast
            | FastToolKind::ProductPerformance),
        ) => Some(tool),
        _ => None,
    }
}

pub(crate) fn plan_fast_tool(question: &str, data_as_of: &str) -> Result<Option<ToolPlan>, String> {
    let Some(kind) = detect_fast_tool(question) else {
        return Ok(None);
    };
    let latest_complete = latest_complete_business_date(data_as_of)
        .ok_or_else(|| "data freshness timestamp was invalid".to_owned())?;
    let normalized = normalize_analytics_language(question);
    let plan = match kind {
        FastToolKind::OrderCount => ToolPlan::OrderCount {
            range: requested_range(&normalized, latest_complete)?,
        },
        FastToolKind::OrderTimeseries => ToolPlan::OrderTimeseries {
            range: requested_range(&normalized, latest_complete)?,
            bucket: if normalized.contains("by month") || normalized.contains("monthly") {
                TimeBucket::Month
            } else if normalized.contains("by week") || normalized.contains("weekly") {
                TimeBucket::Week
            } else {
                TimeBucket::Day
            },
        },
        FastToolKind::TopVariants => ToolPlan::TopVariants {
            range: requested_range(&normalized, latest_complete)?,
            limit: extract_limit(&normalized).clamp(1, 50),
        },
        FastToolKind::StatusBreakdown => ToolPlan::StatusBreakdown {
            range: requested_range(&normalized, latest_complete)?,
        },
        FastToolKind::LapsedCustomers => ToolPlan::LapsedCustomers {
            minimum_orders: minimum_orders(&normalized).clamp(1, 100),
            lookback_months: lookback_months(&normalized),
            inactive_days: number_before(&normalized, "days")
                .unwrap_or(30)
                .clamp(1, 730) as i64,
            product_filter: product_filter(question),
            return_records: !normalized.trim_start().starts_with("how many"),
        },
        FastToolKind::ReorderDemand => ToolPlan::ReorderDemand {
            horizon_days: horizon_days(&normalized).clamp(1, 60),
            limit: extract_limit(&normalized).clamp(1, 50),
        },
        FastToolKind::BusyDayForecast => ToolPlan::BusyDayForecast {
            horizon_days: horizon_days(&normalized).clamp(1, 30),
            lookback_months: number_before(&normalized, "months")
                .unwrap_or(6)
                .clamp(2, 24) as u32,
        },
        FastToolKind::ProductPerformance => {
            let direction = if normalized.contains("better") || normalized.contains("overperform") {
                PerformanceDirection::Overperforming
            } else {
                PerformanceDirection::Underperforming
            };
            let (current, baseline) = performance_ranges(&normalized, latest_complete)?;
            ToolPlan::ProductPerformance {
                current,
                baseline,
                direction,
                limit: extract_limit(&normalized).clamp(1, 100),
            }
        }
    };
    Ok(Some(plan))
}

fn requested_range(question: &str, latest_complete: NaiveDate) -> Result<DateRange, String> {
    if let Some(range) = explicit_day_range(question, latest_complete.year()) {
        return Ok(range);
    }
    let explicit = explicit_months(question, latest_complete.year());

    if explicit.len() >= 2
        && (question.contains(" through ")
            || question.contains("from ") && question.contains(" to "))
    {
        let first = explicit
            .first()
            .expect("explicit month range has a first period");

        let last = explicit
            .last()
            .expect("explicit month range has a last period");

        return Ok(DateRange {
            start: first.start,
            end_exclusive: last.end_exclusive,
        });
    }

    if let Some(range) = explicit.first() {
        return Ok(range.clone());
    }
    if question.contains("last month") {
        return previous_complete_month(latest_complete)
            .ok_or_else(|| "could not derive last month".to_owned());
    }
    if question.contains("yesterday") {
        return Ok(DateRange {
            start: latest_complete,
            end_exclusive: latest_complete + Duration::days(1),
        });
    }
    if question.contains("last week") {
        return Ok(complete_days_range(latest_complete, 7));
    }
    if let Some(days) = number_before(question, "days") {
        return Ok(complete_days_range(
            latest_complete,
            days.clamp(1, 366) as i64,
        ));
    }
    if let Some(months) = number_before(question, "months") {
        let end_exclusive = latest_complete + Duration::days(1);
        let start_month = shift_month_start(end_exclusive, months.clamp(1, 24) as u32)?;
        return Ok(DateRange {
            start: start_month,
            end_exclusive,
        });
    }
    Ok(complete_days_range(latest_complete, 30))
}

fn explicit_day_range(question: &str, fallback_year: i32) -> Option<DateRange> {
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
    .find_map(|(name, month)| question.contains(name).then_some(month))?;
    let numbers = digit_runs(question);
    let year = numbers
        .iter()
        .copied()
        .find(|number| (2000..=2100).contains(number))
        .map_or(fallback_year, |year| year as i32);
    let mut days = numbers
        .into_iter()
        .filter(|number| (1..=31).contains(number))
        .collect::<Vec<_>>();
    days.sort_unstable();
    days.dedup();
    let first = *days.first()?;
    let last = *days.last()?;
    Some(DateRange {
        start: NaiveDate::from_ymd_opt(year, month, first)?,
        end_exclusive: NaiveDate::from_ymd_opt(year, month, last)? + Duration::days(1),
    })
}

fn performance_ranges(
    question: &str,
    latest_complete: NaiveDate,
) -> Result<(DateRange, DateRange), String> {
    let explicit = explicit_months(question, latest_complete.year());
    if explicit.len() >= 2 {
        return Ok((explicit[0].clone(), explicit[1].clone()));
    }
    if let Some(months) = number_before(question, "months") {
        let months = months.clamp(1, 6) as u32;
        let end_exclusive = latest_complete + Duration::days(1);
        let current_start = end_exclusive
            .checked_sub_months(Months::new(months))
            .ok_or_else(|| "invalid current performance period".to_owned())?;
        let baseline_start = current_start
            .checked_sub_months(Months::new(months))
            .ok_or_else(|| "invalid baseline performance period".to_owned())?;
        return Ok((
            DateRange {
                start: current_start,
                end_exclusive,
            },
            DateRange {
                start: baseline_start,
                end_exclusive: current_start,
            },
        ));
    }
    let days = number_before(question, "days").unwrap_or(30).clamp(7, 180) as i64;
    let current = complete_days_range(latest_complete, days);
    let baseline = DateRange {
        start: current.start - Duration::days(days),
        end_exclusive: current.start,
    };
    Ok((current, baseline))
}

fn explicit_months(question: &str, fallback_year: i32) -> Vec<DateRange> {
    let year = digit_runs(question)
        .into_iter()
        .find(|number| (2000..=2100).contains(number))
        .map_or(fallback_year, |number| number as i32);
    let mut found = [
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
    .filter_map(|(name, month)| question.find(name).map(|position| (position, month)))
    .collect::<Vec<_>>();
    found.sort_by_key(|(position, _)| *position);
    found
        .into_iter()
        .filter_map(|(_, month)| month_range(year, month))
        .collect()
}

fn shift_month_start(date: NaiveDate, months: u32) -> Result<NaiveDate, String> {
    let absolute = date.year() * 12 + date.month0() as i32 - months as i32;
    NaiveDate::from_ymd_opt(
        absolute.div_euclid(12),
        absolute.rem_euclid(12) as u32 + 1,
        1,
    )
    .ok_or_else(|| "invalid month range".to_owned())
}

fn extract_limit(question: &str) -> usize {
    let tokens = question.split_whitespace().collect::<Vec<_>>();
    tokens
        .windows(2)
        .find_map(|window| {
            matches!(window[0], "top" | "first")
                .then(|| numeric_prefix(window[1]))
                .flatten()
        })
        .unwrap_or(10)
}

fn horizon_days(question: &str) -> i64 {
    number_before(question, "days").unwrap_or(7) as i64
}

fn number_before(question: &str, unit: &str) -> Option<usize> {
    let tokens = question.split_whitespace().collect::<Vec<_>>();
    tokens.windows(2).find_map(|window| {
        window[1]
            .trim_matches(|character: char| !character.is_ascii_alphabetic())
            .starts_with(unit)
            .then(|| numeric_prefix(window[0]))
            .flatten()
    })
}

fn minimum_orders(question: &str) -> usize {
    let tokens = question.split_whitespace().collect::<Vec<_>>();
    if let Some(value) = tokens.windows(3).find_map(|window| {
        (window[0] == "more" && window[1] == "than")
            .then(|| numeric_prefix(window[2]))
            .flatten()
    }) {
        return value.saturating_add(1);
    }
    tokens
        .windows(3)
        .find_map(|window| {
            (window[0] == "least"
                && (window[2].starts_with("order") || window[2].starts_with("time")))
            .then(|| numeric_prefix(window[1]))
            .flatten()
        })
        .or_else(|| number_before(question, "orders"))
        .unwrap_or(5)
}

fn lookback_months(question: &str) -> u32 {
    if let Some(months) = number_before(question, "months") {
        return months.clamp(1, 24) as u32;
    }
    if let Some(years) = number_before(question, "year") {
        return years.saturating_mul(12).clamp(1, 24) as u32;
    }
    if question.contains("past year") || question.contains("last year") {
        return 12;
    }
    6
}

fn numeric_prefix(value: &str) -> Option<usize> {
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn digit_runs(value: &str) -> Vec<u32> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn product_filter(question: &str) -> Option<String> {
    let normalized = question.to_lowercase();
    let bought = normalized.find("bought ")? + "bought ".len();
    let tail = question.get(bought..)?.trim();
    let end = tail.to_lowercase().find(" at least").unwrap_or(tail.len());
    let product = tail.get(..end)?.trim();
    (!product.is_empty()).then(|| product.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRESHNESS: &str = "2026-08-11 06:16:44.439091+00";

    #[test]
    fn detects_required_fast_tools() {
        assert_eq!(
            detect_fast_tool("Give me the most selling products in June 2026"),
            None
        );

        assert_eq!(
            detect_fast_tool("Give me the most selling variants in June 2026"),
            Some(FastToolKind::TopVariants)
        );

        assert_eq!(
            detect_fast_tool("Give me the top 10 SKUs in June 2026"),
            Some(FastToolKind::TopVariants)
        );

        assert_eq!(
            detect_fast_tool("Which customers haven't ordered in the last 30 days?"),
            Some(FastToolKind::LapsedCustomers)
        );

        assert_eq!(
            detect_fast_tool("When are the next busy days?"),
            Some(FastToolKind::BusyDayForecast)
        );
    }

    #[test]
    fn extracts_validated_lapse_parameters() {
        let plan = plan_fast_tool(
            "Which customers placed at least 5 orders in the last 6 months but haven't ordered in the last 30 days?",
            FRESHNESS,
        )
        .unwrap()
        .unwrap();
        let ToolPlan::LapsedCustomers {
            minimum_orders,
            lookback_months,
            inactive_days,
            ..
        } = plan
        else {
            panic!("wrong plan")
        };
        assert_eq!((minimum_orders, lookback_months, inactive_days), (5, 6, 30));

        let product = plan_fast_tool(
            "Which customers bought Khapli Atta at least 3 times in the past 6 months but haven't ordered it in the last 45 days?",
            FRESHNESS,
        )
        .unwrap()
        .unwrap();
        let ToolPlan::LapsedCustomers {
            minimum_orders,
            product_filter,
            ..
        } = product
        else {
            panic!("wrong plan")
        };
        assert_eq!(minimum_orders, 3);
        assert_eq!(product_filter.as_deref(), Some("Khapli Atta"));
    }

    #[test]
    fn more_than_and_past_year_have_exact_lapse_semantics() {
        let plan = plan_fast_tool(
            "How many customers placed more than 5 orders in the past year but haven't ordered in 100 days?",
            FRESHNESS,
        )
        .unwrap()
        .unwrap();
        let ToolPlan::LapsedCustomers {
            minimum_orders,
            lookback_months,
            inactive_days,
            return_records,
            ..
        } = plan
        else {
            panic!("wrong plan")
        };
        assert_eq!(minimum_orders, 6);
        assert_eq!(lookback_months, 12);
        assert_eq!(inactive_days, 100);
        assert!(!return_records);
    }

    #[test]
    fn uses_explicit_comparison_months_in_request_order() {
        let plan = plan_fast_tool(
            "Which SKUs performed worse in June compared with May?",
            FRESHNESS,
        )
        .unwrap()
        .unwrap();
        let ToolPlan::ProductPerformance {
            current, baseline, ..
        } = plan
        else {
            panic!("wrong plan")
        };
        assert_eq!(current.start, NaiveDate::from_ymd_opt(2026, 6, 1).unwrap());
        assert_eq!(baseline.start, NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
    }

    #[test]
    fn extracts_explicit_order_count_date_range() {
        let plan = plan_fast_tool(
            "How many orders did we get between May 1 and May 15, 2026?",
            FRESHNESS,
        )
        .unwrap()
        .unwrap();
        let ToolPlan::OrderCount { range } = plan else {
            panic!("wrong plan")
        };
        assert_eq!(range.start, NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
        assert_eq!(
            range.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 5, 16).unwrap()
        );
    }

    #[test]
    fn uses_equal_rolling_month_windows_for_performance() {
        let plan = plan_fast_tool(
            "Which SKUs performed better in the past 2 months versus the previous 2 months?",
            FRESHNESS,
        )
        .unwrap()
        .unwrap();
        let ToolPlan::ProductPerformance {
            current, baseline, ..
        } = plan
        else {
            panic!("wrong plan")
        };
        assert_eq!(current.start, NaiveDate::from_ymd_opt(2026, 6, 11).unwrap());
        assert_eq!(
            current.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()
        );
        assert_eq!(
            baseline.start,
            NaiveDate::from_ymd_opt(2026, 4, 11).unwrap()
        );
        assert_eq!(baseline.end_exclusive, current.start);
    }

    #[test]
    fn yesterday_order_count_uses_latest_complete_business_day() {
        let plan = plan_fast_tool(
            "How many orders were placed yesterday?",
            "2026-08-13 01:00:00+00",
        )
        .unwrap()
        .unwrap();

        let ToolPlan::OrderCount { range } = plan else {
            panic!("wrong plan")
        };

        assert_eq!(range.start, NaiveDate::from_ymd_opt(2026, 8, 12).unwrap());

        assert_eq!(
            range.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        );
    }

    #[test]
    fn contiguous_month_range_uses_first_start_and_last_end() {
        let plan = plan_fast_tool(
            "Compare orders placed from May to July 2026 week by week",
            "2026-08-13 01:00:00+00",
        )
        .unwrap()
        .unwrap();

        let ToolPlan::OrderTimeseries { range, bucket } = plan else {
            panic!("wrong plan")
        };

        assert_eq!(range.start, NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());

        assert_eq!(
            range.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );

        assert_eq!(bucket, TimeBucket::Week);
    }
}
