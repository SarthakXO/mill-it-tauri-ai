use super::{
    dates::{complete_days_range, latest_complete_business_date, month_range},
    plan::{
        AnalyticsDomain, AnalyticsFilters, AnalyticsPlan, Dimension, Metric, SortSpec,
        StatusDimension, StatusFilter, TimePeriod, VisualizationIntent,
    },
};
use chrono::{Datelike, NaiveDate};

const MONTHS: [(&str, u32); 12] = [
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
];

pub(crate) fn normalize_analytics_language(question: &str) -> String {
    let mut normalized = question.to_lowercase();

    for typo in ["comapre", "comapare"] {
        normalized = normalized.replace(typo, "compare");
    }

    for (from, to) in [
        ("atleast", "at least"),
        ("with in", "within"),
        ("fulfiled", "fulfilled"),
        ("fullfilled", "fulfilled"),
    ] {
        normalized = normalized.replace(from, to);
    }

    normalized
}

pub(crate) fn parse_semantic_plan(
    question: &str,
    data_as_of: &str,
    referential_context: Option<&str>,
) -> Result<Option<AnalyticsPlan>, String> {
    let latest_complete = latest_complete_business_date(data_as_of)
        .ok_or_else(|| "data freshness timestamp was invalid".to_owned())?;
    let normalized = normalize_analytics_language(question);
    let metrics = match_metrics(&normalized).or_else(|| {
        is_referential(&normalized)
            .then(|| {
                referential_context
                    .and_then(|context| match_metrics(&normalize_analytics_language(context)))
            })
            .flatten()
    });
    let Some(metrics) = metrics else {
        return Ok(None);
    };
    let primary_metric = metrics[0];
    let mut dimensions = dimensions(&normalized, primary_metric);
    let mut periods = parse_periods(&normalized, latest_complete)?;
    if is_referential(&normalized) && references_prior_periods(&normalized, periods.is_empty()) {
        if let Some(context) = referential_context {
            let prior = parse_periods(&normalize_analytics_language(context), latest_complete)?;
            let current = std::mem::take(&mut periods);
            for period in prior.into_iter().chain(current) {
                if !periods_equal_any(&period, &periods) {
                    periods.push(period);
                }
            }
            periods.sort_by_key(|period| period.start);
        }
    }
    if periods.is_empty() {
        return Ok(None);
    }
    let time_bucketed = dimensions.iter().any(|dimension| {
        matches!(
            dimension,
            Dimension::Day | Dimension::Week | Dimension::Month
        )
    });
    if time_bucketed && periods.len() > 1 && periods_are_contiguous(&periods) {
        periods = vec![combined_period(&periods)];
        dimensions.retain(|dimension| *dimension != Dimension::Period);
    } else if periods.len() > 1 && !dimensions.contains(&Dimension::Period) {
        dimensions.insert(0, Dimension::Period);
    }
    if primary_metric == Metric::QuantitySold
        && !dimensions.contains(&Dimension::Variant)
        && !dimensions.contains(&Dimension::Product)
        && contains_any(&normalized, &["top", "selling", "sold most", "best"])
    {
        dimensions.push(Dimension::Variant);
    }

    let domain = match primary_metric {
        Metric::OrderCount
        | Metric::OrderValue
        | Metric::AverageOrderValue
        | Metric::AverageItemQuantityPerOrder
        | Metric::CancelledOrderCount
        | Metric::CancellationRate => AnalyticsDomain::Operations,
        Metric::UniqueCustomers => AnalyticsDomain::Customers,
        Metric::QuantitySold
        | Metric::ProductOrderCount
        | Metric::ProductUniqueCustomers
        | Metric::ProductSalesValue
        | Metric::UniqueVariants => AnalyticsDomain::Products,
    };
    let status_filters = parse_status_filters(&normalized);
    let filters = AnalyticsFilters {
        exclude_cancelled: contains_any(
            &normalized,
            &[
                "non-cancelled",
                "non cancelled",
                "not cancelled",
                "not canceled",
                "exclude cancelled",
                "exclude canceled",
            ],
        ) || metrics.iter().any(|metric| {
            matches!(
                metric,
                Metric::QuantitySold
                    | Metric::UniqueVariants
                    | Metric::OrderValue
                    | Metric::AverageOrderValue
            )
        }),
        status_filters,
        store_id: None,
        product: None,
        variant_id: None,
    };
    let ranking =
        dimensions.contains(&Dimension::Variant) || dimensions.contains(&Dimension::Product);
    let plan = AnalyticsPlan {
        domain,
        metrics,
        dimensions,
        periods,
        filters,
        sort: Some(if ranking {
            SortSpec::Descending
        } else {
            SortSpec::Chronological
        }),
        limit: ranking.then(|| extract_limit(&normalized).unwrap_or(10).clamp(1, 100) as u32),
        visualization: Some(if ranking {
            VisualizationIntent::HorizontalBar
        } else if contains_any(&normalized, &["week", "daily", "by day", "every month"]) {
            VisualizationIntent::Line
        } else {
            VisualizationIntent::Bar
        }),
    };
    plan.validate().map_err(|error| error.to_string())?;
    Ok(Some(plan))
}

fn match_metrics(question: &str) -> Option<Vec<Metric>> {
    let customer_subject = contains_any(question, &["customer", "customers", "buyers"]);
    let variant_subject = contains_any(question, &["sku", "skus", "variant", "variants"]);
    let product_subject = contains_any(question, &["product", "products", "item", "items"]);
    if customer_subject
        && contains_any(
            question,
            &[
                "how many",
                "count",
                "unique",
                "different",
                "every month",
                "monthly",
            ],
        )
        && !contains_any(
            question,
            &["lapsed", "inactive", "haven't", "have not", "reorder"],
        )
    {
        Some(vec![Metric::UniqueCustomers])
    } else if variant_subject
        && contains_any(question, &["how many", "count", "unique", "different"])
    {
        Some(vec![Metric::UniqueVariants])
    } else if (variant_subject || product_subject)
        && contains_any(
            question,
            &["top", "selling", "sold", "quantity", "best", "volume"],
        )
    {
        Some(vec![Metric::QuantitySold])
    } else if contains_any(
        question,
        &[
            "average number of items per order",
            "average items per order",
            "average item quantity",
        ],
    ) {
        Some(vec![Metric::AverageItemQuantityPerOrder])
    } else if contains_any(question, &["aov", "average order value"])
        || (question.contains("average") && question.contains("order value"))
    {
        let mut metrics = Vec::new();
        if contains_any(
            question,
            &["compare orders,", "orders, order value", "order count"],
        ) {
            metrics.push(Metric::OrderCount);
        }
        if question.contains("order value") && contains_any(question, &["orders,", "value and"]) {
            metrics.push(Metric::OrderValue);
        }
        metrics.push(Metric::AverageOrderValue);
        Some(metrics)
    } else if contains_any(question, &["order value", "gross order value"])
        || (question.contains("revenue") && question.contains("order"))
    {
        let mut metrics = vec![Metric::OrderValue];
        if contains_any(question, &["compare orders,", "orders and order value"]) {
            metrics.insert(0, Metric::OrderCount);
        }
        Some(metrics)
    } else if question.contains("cancellation rate") {
        Some(vec![Metric::CancellationRate])
    } else if contains_any(question, &["cancelled order count", "canceled order count"])
        && !contains_any(
            question,
            &[
                "non-cancelled",
                "non cancelled",
                "not cancelled",
                "not canceled",
                "exclude cancelled",
                "exclude canceled",
            ],
        )
    {
        Some(vec![Metric::CancelledOrderCount])
    } else if contains_any(question, &["order", "orders", "order volume"])
        && !contains_any(
            question,
            &[
                "reorder",
                "lapsed",
                "inactive",
                "haven't",
                "have not",
                "average number of line items",
                "average items per order",
            ],
        )
    {
        Some(vec![Metric::OrderCount])
    } else {
        None
    }
}

fn parse_status_filters(question: &str) -> Vec<StatusFilter> {
    if contains_any(question, &["payment-pending", "payment pending"]) {
        return vec![StatusFilter {
            dimension: StatusDimension::Payment,
            values: vec!["awaiting".to_owned()],
            exclude: false,
        }];
    }
    if contains_any(question, &["unfulfilled", "not yet fulfilled"]) {
        return vec![StatusFilter {
            dimension: StatusDimension::Fulfillment,
            values: vec!["not_fulfilled".to_owned()],
            exclude: false,
        }];
    }
    if question.contains("delivered orders") {
        return vec![StatusFilter {
            dimension: StatusDimension::Delivery,
            values: vec!["delivered".to_owned()],
            exclude: false,
        }];
    }
    if question.contains("confirmed and fulfilled") {
        return vec![StatusFilter {
            dimension: StatusDimension::Operational,
            values: vec!["CONFIRMED".to_owned(), "FULFILLED".to_owned()],
            exclude: false,
        }];
    }

    if question.contains("fulfilled")
        && !question.contains("unfulfilled")
        && !question.contains("status breakdown")
    {
        let dimension = if question.contains("fulfillment status") {
            StatusDimension::Fulfillment
        } else {
            StatusDimension::Operational
        };

        let value = if dimension == StatusDimension::Operational {
            "FULFILLED"
        } else {
            "fulfilled"
        };

        return vec![StatusFilter {
            dimension,
            values: vec![value.to_owned()],
            exclude: false,
        }];
    }
    if question.contains("confirmed and fulfilled") {
        return vec![StatusFilter {
            dimension: StatusDimension::Operational,
            values: vec!["CONFIRMED".to_owned(), "FULFILLED".to_owned()],
            exclude: false,
        }];
    }
    Vec::new()
}

fn dimensions(question: &str, metric: Metric) -> Vec<Dimension> {
    let mut dimensions = Vec::new();
    if contains_any(question, &["day by day", "by day", "daily", "each day"]) {
        dimensions.push(Dimension::Day);
    } else if contains_any(
        question,
        &["week by week", "by week", "weekly", "each week"],
    ) {
        dimensions.push(Dimension::Week);
    } else if contains_any(
        question,
        &[
            "month by month",
            "by month",
            "monthly",
            "every month",
            "each month",
        ],
    ) {
        dimensions.push(Dimension::Month);
    }
    if metric.is_order_grain()
        && contains_any(
            question,
            &[
                "operational status",
                "order status",
                "status breakdown",
                "by status",
            ],
        )
    {
        dimensions.push(if question.contains("payment status") {
            Dimension::PaymentStatus
        } else if question.contains("fulfillment status") {
            Dimension::FulfillmentStatus
        } else if question.contains("delivery status") {
            Dimension::DeliveryStatus
        } else if question.contains("order lifecycle status") {
            Dimension::OrderStatus
        } else {
            Dimension::OperationalStatus
        });
    }
    if contains_any(question, &["by store", "store-level", "each store"]) {
        dimensions.push(Dimension::Store);
    }
    if metric == Metric::QuantitySold {
        if contains_any(
            question,
            &[
                "variant",
                "variants",
                "sku",
                "skus",
                "sellable item",
                "sellable items",
            ],
        ) {
            dimensions.push(Dimension::Variant);
        } else if contains_any(question, &["product", "products"]) {
            dimensions.push(Dimension::Product);
        }
    }
    dimensions
}

fn parse_periods(question: &str, latest_complete: NaiveDate) -> Result<Vec<TimePeriod>, String> {
    if question.contains("every month this year") {
        return Ok(vec![TimePeriod {
            label: format!("{} year to date", latest_complete.year()),
            start: NaiveDate::from_ymd_opt(latest_complete.year(), 1, 1)
                .ok_or_else(|| "invalid year-to-date period".to_owned())?,
            end_exclusive: latest_complete + chrono::Duration::days(1),
        }]);
    }
    if let Some(period) = explicit_day_period(question, latest_complete.year())? {
        return Ok(vec![period]);
    }
    let mentions = month_mentions(question, latest_complete.year());
    if question.contains("every month") && mentions.is_empty() {
        if let Some((_, year)) = year_mentions(question).first() {
            return Ok(vec![TimePeriod {
                label: year.to_string(),
                start: NaiveDate::from_ymd_opt(*year, 1, 1)
                    .ok_or_else(|| "invalid annual period".to_owned())?,
                end_exclusive: NaiveDate::from_ymd_opt(*year + 1, 1, 1)
                    .ok_or_else(|| "invalid annual period".to_owned())?,
            }]);
        }
    }
    if let Some(periods) = first_months_of_year(question) {
        return Ok(periods);
    }
    if !mentions.is_empty() {
        if mentions.len() == 2 && range_connector(question, &mentions) {
            return expand_month_range(&mentions);
        }
        let mut periods = mentions
            .into_iter()
            .map(|mention| month_period(mention.year, mention.month))
            .collect::<Result<Vec<_>, _>>()?;
        periods.sort_by_key(|period| period.start);
        periods.dedup_by_key(|period| period.start);
        return Ok(periods);
    }
    if question.contains("last 7 complete days") {
        return Ok(vec![rolling_period(
            latest_complete,
            7,
            "Last 7 complete days",
        )]);
    }
    if let Some(days) =
        number_before(question, "days").or_else(|| number_before(question, "complete"))
    {
        return Ok(vec![rolling_period(
            latest_complete,
            days.clamp(1, 366) as i64,
            &format!("Previous {} days", days.clamp(1, 366)),
        )]);
    }
    Ok(Vec::new())
}

#[derive(Clone, Copy, Debug)]
struct MonthMention {
    position: usize,
    month: u32,
    year: i32,
}

fn explicit_day_period(question: &str, fallback_year: i32) -> Result<Option<TimePeriod>, String> {
    let words = question.split_whitespace().collect::<Vec<_>>();

    let year = year_mentions(question)
        .first()
        .map(|(_, year)| *year)
        .unwrap_or(fallback_year);

    for (index, word) in words.iter().enumerate() {
        let Some(month) = month_number_from_token(word) else {
            continue;
        };

        let day = index
            .checked_sub(1)
            .and_then(|previous| day_number_from_token(words[previous]))
            .or_else(|| {
                words
                    .get(index + 1)
                    .and_then(|next| day_number_from_token(next))
            });

        let Some(day) = day else {
            continue;
        };

        let start = NaiveDate::from_ymd_opt(year, month, day)
            .ok_or_else(|| "explicit calendar date was invalid".to_owned())?;

        return Ok(Some(TimePeriod {
            label: format!(
                "{} {}, {}",
                MONTHS[(month - 1) as usize].0.to_ascii_titlecase(),
                day,
                year
            ),
            start,
            end_exclusive: start + chrono::Duration::days(1),
        }));
    }

    Ok(None)
}

fn month_number_from_token(token: &str) -> Option<u32> {
    let token = token
        .trim_matches(|character: char| !character.is_ascii_alphabetic())
        .to_lowercase();

    match token.as_str() {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

fn day_number_from_token(token: &str) -> Option<u32> {
    let token = token.trim_matches(|character: char| !character.is_ascii_alphanumeric());

    let digits = token
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();

    let day = digits.parse::<u32>().ok()?;

    (1..=31).contains(&day).then_some(day)
}

fn month_mentions(question: &str, fallback_year: i32) -> Vec<MonthMention> {
    let years = year_mentions(question);
    let mut mentions = Vec::new();
    for (name, month) in MONTHS {
        for (position, _) in question.match_indices(name) {
            let year = if years.len() == 1 {
                years[0].1
            } else {
                years
                    .iter()
                    .min_by_key(|(year_position, _)| position.abs_diff(*year_position))
                    .map_or(fallback_year, |(_, year)| *year)
            };
            mentions.push(MonthMention {
                position,
                month,
                year,
            });
        }
    }
    mentions.sort_by_key(|mention| mention.position);
    mentions
}

fn year_mentions(question: &str) -> Vec<(usize, i32)> {
    question
        .match_indices(|character: char| character.is_ascii_digit())
        .filter_map(|(position, _)| {
            let digits = question[position..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            (digits.len() == 4)
                .then(|| digits.parse::<i32>().ok().map(|year| (position, year)))
                .flatten()
        })
        .filter(|(_, year)| (2000..=2100).contains(year))
        .collect()
}

fn range_connector(question: &str, mentions: &[MonthMention]) -> bool {
    let start = mentions[0].position;
    let end = mentions[1].position;
    let between = &question[start..end];
    contains_any(between, &[" to ", " through ", " thru ", " until "])
}

fn expand_month_range(mentions: &[MonthMention]) -> Result<Vec<TimePeriod>, String> {
    let start = mentions[0].year * 12 + mentions[0].month as i32 - 1;
    let end = mentions[1].year * 12 + mentions[1].month as i32 - 1;
    if end < start || end - start >= 24 {
        return Err("month range is invalid or too large".to_owned());
    }
    (start..=end)
        .map(|absolute| month_period(absolute.div_euclid(12), absolute.rem_euclid(12) as u32 + 1))
        .collect()
}

fn first_months_of_year(question: &str) -> Option<Vec<TimePeriod>> {
    let year = year_mentions(question).first()?.1;
    let count = if question.contains("first six months") {
        6
    } else if question.contains("first quarter") {
        3
    } else {
        return None;
    };
    (1..=count)
        .map(|month| month_period(year, month))
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn month_period(year: i32, month: u32) -> Result<TimePeriod, String> {
    let range = month_range(year, month).ok_or_else(|| "invalid month".to_owned())?;
    Ok(TimePeriod {
        label: format!(
            "{} {year}",
            MONTHS[(month - 1) as usize].0.to_ascii_titlecase()
        ),
        start: range.start,
        end_exclusive: range.end_exclusive,
    })
}

fn combined_period(periods: &[TimePeriod]) -> TimePeriod {
    TimePeriod {
        label: format!(
            "{} through {}",
            periods
                .first()
                .map_or("Period", |period| period.label.as_str()),
            periods
                .last()
                .map_or("Period", |period| period.label.as_str())
        ),
        start: periods.first().unwrap().start,
        end_exclusive: periods.last().unwrap().end_exclusive,
    }
}

fn rolling_period(latest_complete: NaiveDate, days: i64, label: &str) -> TimePeriod {
    let range = complete_days_range(latest_complete, days);
    TimePeriod {
        label: label.to_owned(),
        start: range.start,
        end_exclusive: range.end_exclusive,
    }
}

fn periods_are_contiguous(periods: &[TimePeriod]) -> bool {
    periods
        .windows(2)
        .all(|window| window[0].end_exclusive == window[1].start)
}

fn periods_equal_any(period: &TimePeriod, periods: &[TimePeriod]) -> bool {
    periods.iter().any(|candidate| {
        candidate.start == period.start && candidate.end_exclusive == period.end_exclusive
    })
}

fn is_referential(question: &str) -> bool {
    contains_any(
        &format!(" {question} "),
        &[" that ", " those ", " same ", " it ", " previous "],
    )
}

fn references_prior_periods(question: &str, current_periods_missing: bool) -> bool {
    current_periods_missing
        || contains_any(
            &format!(" {question} "),
            &[
                " that ",
                " those ",
                " it ",
                " previous ",
                " above ",
                " earlier ",
            ],
        )
}

fn extract_limit(question: &str) -> Option<usize> {
    let tokens = question.split_whitespace().collect::<Vec<_>>();
    tokens.windows(2).find_map(|window| {
        (window[0] == "top")
            .then(|| numeric_prefix(window[1]))
            .flatten()
    })
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

fn numeric_prefix(value: &str) -> Option<usize> {
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn contains_any(value: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| value.contains(term))
}

trait TitleCase {
    fn to_ascii_titlecase(&self) -> String;
}

impl TitleCase for str {
    fn to_ascii_titlecase(&self) -> String {
        let mut characters = self.chars();
        characters
            .next()
            .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRESHNESS: &str = "2026-08-11 06:16:44.439091+00";

    fn plan(question: &str) -> AnalyticsPlan {
        parse_semantic_plan(question, FRESHNESS, None)
            .unwrap()
            .expect("question should produce a plan")
    }

    #[test]
    fn four_months_are_sorted_and_preserved_as_first_class_periods() {
        let plan = plan("compare may june July and april orders of 2026");
        assert_eq!(plan.metrics, vec![Metric::OrderCount]);
        assert_eq!(plan.dimensions, vec![Dimension::Period]);
        assert_eq!(plan.periods.len(), 4);
        assert_eq!(plan.periods[0].label, "April 2026");
        assert_eq!(plan.periods[3].label, "July 2026");
    }

    #[test]
    fn repeated_month_with_distinct_years_is_non_contiguous() {
        let plan = plan("Compare May 2025 with May 2026 order volume.");
        assert_eq!(plan.periods.len(), 2);
        assert_eq!(plan.periods[0].start.year(), 2025);
        assert_eq!(plan.periods[1].start.year(), 2026);
    }

    #[test]
    fn typo_weekly_range_becomes_one_may_through_june_period() {
        let plan =
            plan("comapre week by week orders from 1st week of may to last week of june 2026");
        assert_eq!(plan.dimensions, vec![Dimension::Week]);
        assert_eq!(plan.periods.len(), 1);
        assert_eq!(
            plan.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap()
        );
        assert_eq!(
            plan.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
        );
    }

    #[test]
    fn monthly_unique_customers_and_variant_rankings_are_typed() {
        let customers =
            plan("Show unique customer count for every month from January through June 2026.");
        assert_eq!(customers.metrics, vec![Metric::UniqueCustomers]);
        assert_eq!(customers.dimensions, vec![Dimension::Month]);
        assert_eq!(customers.periods.len(), 1);
        assert_eq!(
            customers.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
        assert_eq!(
            customers.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
        );

        let variants = plan("Give me the top 10 SKUs from May 2026.");
        assert_eq!(variants.metrics, vec![Metric::QuantitySold]);
        assert_eq!(variants.dimensions, vec![Dimension::Variant]);
        assert_eq!(variants.limit, Some(10));
    }

    #[test]
    fn multi_period_status_and_cancellation_are_structured() {
        let cancellation = plan("Compare non-cancelled order count for April, May and June 2026.");
        assert_eq!(cancellation.metrics, vec![Metric::OrderCount]);
        assert_eq!(cancellation.periods.len(), 3);
        assert!(cancellation.filters.exclude_cancelled);

        let cancelled = plan("Compare cancelled order count for April, May and June 2026.");
        assert_eq!(cancelled.metrics, vec![Metric::CancelledOrderCount]);

        let status = plan("Show operational order status breakdown for May and June separately.");
        assert_eq!(
            status.dimensions,
            vec![Dimension::Period, Dimension::OperationalStatus]
        );
    }

    #[test]
    fn complete_day_status_language_stays_on_the_deterministic_path() {
        let plan = plan("Show unfulfilled orders from the last 30 complete days.");
        assert_eq!(plan.metrics, vec![Metric::OrderCount]);
        assert_eq!(
            plan.periods[0].end_exclusive - plan.periods[0].start,
            chrono::Duration::days(30)
        );
        assert_eq!(
            plan.filters.status_filters[0].dimension,
            StatusDimension::Fulfillment
        );
        assert_eq!(plan.filters.status_filters[0].values, vec!["not_fulfilled"]);
    }

    #[test]
    fn referential_context_adds_prior_period_but_self_contained_requests_do_not() {
        let referential = parse_semantic_plan(
            "Compare that with June 2026",
            FRESHNESS,
            Some("Show May 2026 orders"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(referential.periods.len(), 2);
        let isolated = plan("Compare April and July 2026 orders");
        assert_eq!(isolated.periods.len(), 2);
        assert!(!isolated
            .periods
            .iter()
            .any(|period| period.start.month() == 5));

        let aov = parse_semantic_plan(
            "Now do the same for AOV.",
            FRESHNESS,
            Some("Show May and June orders."),
        )
        .unwrap()
        .unwrap();
        assert_eq!(aov.metrics, vec![Metric::AverageOrderValue]);
        assert_eq!(aov.periods.len(), 2);

        let same_metric_new_periods = parse_semantic_plan(
            "Compare the same metric for April and July.",
            FRESHNESS,
            Some("Compare AOV for May and June 2026."),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            same_metric_new_periods.metrics,
            vec![Metric::AverageOrderValue]
        );
        assert_eq!(same_metric_new_periods.periods.len(), 2);
        assert_eq!(same_metric_new_periods.periods[0].start.month(), 4);
        assert_eq!(same_metric_new_periods.periods[1].start.month(), 7);
    }
    #[test]
    fn explicit_single_calendar_dates_do_not_expand_to_whole_months() {
        let full = plan("how many orders were placed on 1st august 2026");

        assert_eq!(
            full.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
        assert_eq!(
            full.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 2).unwrap()
        );

        let abbreviated = plan("how many orders were placed on aug 1st");

        assert_eq!(
            abbreviated.periods[0].start,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
        assert_eq!(
            abbreviated.periods[0].end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 2).unwrap()
        );
    }

    #[test]
    fn fulfilled_typo_normalizes_to_operational_fulfilled_filter() {
        let plan = plan("how many july orders were fulfiled");

        assert_eq!(plan.filters.status_filters.len(), 1);
        assert_eq!(
            plan.filters.status_filters[0].dimension,
            StatusDimension::Operational
        );
        assert_eq!(plan.filters.status_filters[0].values, vec!["FULFILLED"]);
    }

    #[test]
    fn product_and_variant_rankings_keep_distinct_grains() {
        let products = plan("what were the top selling products in july");
        assert_eq!(products.metrics, vec![Metric::QuantitySold]);
        assert_eq!(products.dimensions, vec![Dimension::Product]);

        let variants = plan("what were the top selling variants in july");
        assert_eq!(variants.metrics, vec![Metric::QuantitySold]);
        assert_eq!(variants.dimensions, vec![Dimension::Variant]);
    }

    #[test]
    fn customer_cohort_spelling_normalization_is_preserved() {
        assert_eq!(
            normalize_analytics_language(
                "customers with atleast 5 orders who have not ordered with in 45 days"
            ),
            "customers with at least 5 orders who have not ordered within 45 days"
        );
    }
}
