use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

pub(crate) const MAX_PERIODS: usize = 24;
pub(crate) const MAX_PLAN_LIMIT: u32 = 100;
pub(crate) const MAX_METRICS: usize = 6;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnalyticsDomain {
    Operations,
    Customers,
    Products,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Metric {
    OrderCount,
    UniqueCustomers,
    QuantitySold,
    UniqueVariants,
    OrderValue,
    AverageOrderValue,
    AverageItemQuantityPerOrder,
    CancelledOrderCount,
    CancellationRate,
}

impl Metric {
    pub(crate) const fn value_key(self) -> &'static str {
        match self {
            Self::OrderCount => "order_count",
            Self::UniqueCustomers => "customer_count",
            Self::QuantitySold => "quantity_sold",
            Self::UniqueVariants => "variant_count",
            Self::OrderValue => "order_value",
            Self::AverageOrderValue => "average_order_value",
            Self::AverageItemQuantityPerOrder => "average_item_quantity_per_order",
            Self::CancelledOrderCount => "cancelled_order_count",
            Self::CancellationRate => "cancellation_rate",
        }
    }

    pub(crate) const fn is_order_grain(self) -> bool {
        matches!(
            self,
            Self::OrderCount
                | Self::OrderValue
                | Self::AverageOrderValue
                | Self::AverageItemQuantityPerOrder
                | Self::CancelledOrderCount
                | Self::CancellationRate
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Dimension {
    Day,
    Week,
    Month,
    Period,
    OperationalStatus,
    OrderStatus,
    FulfillmentStatus,
    PaymentStatus,
    DeliveryStatus,
    Store,
    Variant,
    Product,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TimePeriod {
    pub(crate) label: String,
    pub(crate) start: NaiveDate,
    pub(crate) end_exclusive: NaiveDate,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AnalyticsFilters {
    pub(crate) exclude_cancelled: bool,
    pub(crate) status_filters: Vec<StatusFilter>,
    pub(crate) store_id: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) variant_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StatusDimension {
    Operational,
    Order,
    Fulfillment,
    Payment,
    Delivery,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct StatusFilter {
    pub(crate) dimension: StatusDimension,
    pub(crate) values: Vec<String>,
    #[serde(default)]
    pub(crate) exclude: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SortSpec {
    Chronological,
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VisualizationIntent {
    Bar,
    HorizontalBar,
    Line,
    Table,
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct MetricQueryPlan {
    pub(crate) domain: AnalyticsDomain,
    pub(crate) metrics: Vec<Metric>,
    #[serde(default)]
    pub(crate) dimensions: Vec<Dimension>,
    pub(crate) periods: Vec<TimePeriod>,
    #[serde(default)]
    pub(crate) filters: AnalyticsFilters,
    pub(crate) sort: Option<SortSpec>,
    pub(crate) limit: Option<u32>,
    pub(crate) visualization: Option<VisualizationIntent>,
}

pub(crate) type AnalyticsPlan = MetricQueryPlan;

impl MetricQueryPlan {
    pub(crate) fn validate(&self) -> Result<(), PlanValidationError> {
        if self.metrics.is_empty() || self.metrics.len() > MAX_METRICS {
            return Err(PlanValidationError::MetricCount);
        }
        let mut unique_metrics = self.metrics.clone();
        unique_metrics.sort_by_key(|metric| *metric as u8);
        unique_metrics.dedup();
        if unique_metrics.len() != self.metrics.len() {
            return Err(PlanValidationError::DuplicateMetric);
        }
        if self.periods.is_empty() || self.periods.len() > MAX_PERIODS {
            return Err(PlanValidationError::PeriodCount);
        }
        for period in &self.periods {
            if period.label.trim().is_empty()
                || period.label.len() > 80
                || period.start >= period.end_exclusive
                || period.start.year() < 2000
                || period.end_exclusive.year() > 2101
            {
                return Err(PlanValidationError::InvalidPeriod);
            }
        }
        if self.dimensions.len() > 2 {
            return Err(PlanValidationError::TooManyDimensions);
        }
        if self.periods.len() > 1 && !self.dimensions.contains(&Dimension::Period) {
            return Err(PlanValidationError::MissingPeriodDimension);
        }
        if self.dimensions.contains(&Dimension::Period) {
            for (index, left) in self.periods.iter().enumerate() {
                if self.periods[index + 1..].iter().any(|right| {
                    left.start < right.end_exclusive && right.start < left.end_exclusive
                }) {
                    return Err(PlanValidationError::OverlappingPeriods);
                }
            }
        }
        if self
            .limit
            .is_some_and(|limit| limit == 0 || limit > MAX_PLAN_LIMIT)
        {
            return Err(PlanValidationError::InvalidLimit);
        }
        for metric in &self.metrics {
            validate_domain_metric(self.domain, *metric)?;
            validate_dimensions(*metric, &self.dimensions)?;
        }
        validate_filters(self)?;
        Ok(())
    }

    pub(crate) fn expected_shape(&self) -> &'static str {
        if self.dimensions.contains(&Dimension::Variant)
            || self.dimensions.contains(&Dimension::Product)
        {
            "ranking"
        } else if self.dimensions.iter().any(|dimension| {
            matches!(
                dimension,
                Dimension::Day | Dimension::Week | Dimension::Month
            )
        }) {
            "time_series"
        } else if self.dimensions.iter().any(Dimension::is_status) {
            "category_breakdown"
        } else if self.dimensions.contains(&Dimension::Period) {
            "comparison"
        } else {
            "scalar"
        }
    }
}

fn validate_domain_metric(
    domain: AnalyticsDomain,
    metric: Metric,
) -> Result<(), PlanValidationError> {
    let valid = matches!(
        (domain, metric),
        (AnalyticsDomain::Operations, Metric::OrderCount)
            | (AnalyticsDomain::Operations, Metric::OrderValue)
            | (AnalyticsDomain::Operations, Metric::AverageOrderValue)
            | (
                AnalyticsDomain::Operations,
                Metric::AverageItemQuantityPerOrder
            )
            | (AnalyticsDomain::Operations, Metric::CancelledOrderCount)
            | (AnalyticsDomain::Operations, Metric::CancellationRate)
            | (AnalyticsDomain::Customers, Metric::UniqueCustomers)
            | (
                AnalyticsDomain::Products,
                Metric::QuantitySold | Metric::UniqueVariants
            )
    );
    valid.then_some(()).ok_or(PlanValidationError::DomainMetric)
}

fn validate_dimensions(
    metric: Metric,
    dimensions: &[Dimension],
) -> Result<(), PlanValidationError> {
    let invalid = dimensions.iter().any(|dimension| match metric {
        Metric::OrderCount => !matches!(
            dimension,
            Dimension::Day
                | Dimension::Week
                | Dimension::Month
                | Dimension::Period
                | Dimension::OperationalStatus
                | Dimension::OrderStatus
                | Dimension::FulfillmentStatus
                | Dimension::PaymentStatus
                | Dimension::DeliveryStatus
                | Dimension::Store
        ),
        Metric::OrderValue
        | Metric::AverageOrderValue
        | Metric::AverageItemQuantityPerOrder
        | Metric::CancelledOrderCount
        | Metric::CancellationRate => !matches!(
            dimension,
            Dimension::Day
                | Dimension::Week
                | Dimension::Month
                | Dimension::Period
                | Dimension::OperationalStatus
                | Dimension::OrderStatus
                | Dimension::FulfillmentStatus
                | Dimension::PaymentStatus
                | Dimension::DeliveryStatus
                | Dimension::Store
        ),
        Metric::UniqueCustomers => !matches!(
            dimension,
            Dimension::Day
                | Dimension::Week
                | Dimension::Month
                | Dimension::Period
                | Dimension::Store
        ),
        Metric::QuantitySold => !matches!(
            dimension,
            Dimension::Day
                | Dimension::Week
                | Dimension::Month
                | Dimension::Period
                | Dimension::Variant
                | Dimension::Product
        ),
        Metric::UniqueVariants => !matches!(
            dimension,
            Dimension::Day | Dimension::Week | Dimension::Month | Dimension::Period
        ),
    });
    (!invalid)
        .then_some(())
        .ok_or(PlanValidationError::UnsupportedDimension)
}

fn validate_filters(plan: &MetricQueryPlan) -> Result<(), PlanValidationError> {
    let has_cancellation_metric = plan.metrics.iter().any(|metric| {
        matches!(
            metric,
            Metric::CancelledOrderCount | Metric::CancellationRate
        )
    });

    if has_cancellation_metric && plan.filters.exclude_cancelled {
        return Err(PlanValidationError::ContradictoryCancellationFilter);
    }

    if has_cancellation_metric
        && plan.filters.status_filters.iter().any(|status| {
            status.exclude
                && matches!(
                    status.dimension,
                    StatusDimension::Operational | StatusDimension::Order
                )
                && status.values.iter().any(|value| {
                    matches!(
                        value.trim().to_ascii_uppercase().as_str(),
                        "CANCELED" | "CANCELLED"
                    )
                })
        })
    {
        return Err(PlanValidationError::ContradictoryCancellationFilter);
    }
    if !plan.filters.status_filters.is_empty()
        && plan.metrics.iter().any(|metric| !metric.is_order_grain())
    {
        return Err(PlanValidationError::UnsupportedFilter);
    }
    if (plan.filters.product.is_some() || plan.filters.variant_id.is_some())
        && plan
            .metrics
            .iter()
            .any(|metric| !matches!(metric, Metric::QuantitySold | Metric::UniqueVariants))
    {
        return Err(PlanValidationError::UnsupportedFilter);
    }
    for status in &plan.filters.status_filters {
        if status.values.is_empty()
            || status.values.len() > 12
            || status.values.iter().any(|value| {
                value.is_empty()
                    || value.len() > 40
                    || !value.chars().all(|character| {
                        character.is_ascii_alphanumeric() || " _-".contains(character)
                    })
            })
        {
            return Err(PlanValidationError::InvalidStatusFilter);
        }
    }
    Ok(())
}

impl Dimension {
    pub(crate) const fn is_status(&self) -> bool {
        matches!(
            self,
            Self::OperationalStatus
                | Self::OrderStatus
                | Self::FulfillmentStatus
                | Self::PaymentStatus
                | Self::DeliveryStatus
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanValidationError {
    PeriodCount,
    MetricCount,
    DuplicateMetric,
    InvalidPeriod,
    TooManyDimensions,
    MissingPeriodDimension,
    InvalidLimit,
    DomainMetric,
    UnsupportedDimension,
    UnsupportedFilter,
    InvalidStatusFilter,
    ContradictoryCancellationFilter,
    OverlappingPeriods,
}

impl fmt::Display for PlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PeriodCount => "plans require between one and twenty-four periods",
            Self::MetricCount => "metric queries require between one and six metrics",
            Self::DuplicateMetric => "metric queries cannot contain duplicate metrics",
            Self::InvalidPeriod => "a period label or half-open date range is invalid",
            Self::TooManyDimensions => {
                "the current semantic engine supports at most two dimensions"
            }
            Self::MissingPeriodDimension => "multiple periods require the period dimension",
            Self::InvalidLimit => "the limit must be between one and one hundred",
            Self::DomainMetric => "the metric is not supported by the selected domain",
            Self::UnsupportedDimension => "the metric/dimension combination is unsupported",
            Self::UnsupportedFilter => "the filter is unsupported for this metric",
            Self::InvalidStatusFilter => "a status filter is empty, too large, or unsafe",
            Self::OverlappingPeriods => {
    "comparison periods must not overlap; use one distinct non-overlapping interval for each comparison period"
}
            Self::ContradictoryCancellationFilter => {
                "cancellation metrics cannot exclude the cancelled population they measure; \
     set filters.exclude_cancelled=false and remove any operational or order \
     status filter with exclude=true and value CANCELED or CANCELLED"
            }
        })
    }
}

impl Error for PlanValidationError {}

use chrono::Datelike;

#[cfg(test)]
mod tests {
    use super::*;

    fn period(label: &str, start: (i32, u32, u32), end: (i32, u32, u32)) -> TimePeriod {
        TimePeriod {
            label: label.to_owned(),
            start: NaiveDate::from_ymd_opt(start.0, start.1, start.2).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(end.0, end.1, end.2).unwrap(),
        }
    }

    #[test]
    fn arbitrary_period_vectors_are_validated_without_a_two_period_assumption() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount],
            dimensions: vec![Dimension::Period],
            periods: vec![
                period("April", (2026, 4, 1), (2026, 5, 1)),
                period("May", (2026, 5, 1), (2026, 6, 1)),
                period("June", (2026, 6, 1), (2026, 7, 1)),
                period("July", (2026, 7, 1), (2026, 8, 1)),
            ],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Bar),
        };
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_ranges_and_unsupported_combinations() {
        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Customers,
            metrics: vec![Metric::UniqueCustomers],
            dimensions: vec![Dimension::Variant],
            periods: vec![period("May", (2026, 5, 1), (2026, 6, 1))],
            filters: AnalyticsFilters::default(),
            sort: None,
            limit: None,
            visualization: None,
        };
        assert_eq!(
            plan.validate(),
            Err(PlanValidationError::UnsupportedDimension)
        );
        plan.dimensions.clear();
        plan.periods[0].end_exclusive = plan.periods[0].start;
        assert_eq!(plan.validate(), Err(PlanValidationError::InvalidPeriod));
    }

    #[test]
    fn twelve_period_comparisons_do_not_require_a_contract_redesign() {
        let periods = (1..=12)
            .map(|month| {
                let (next_year, next_month) = if month == 12 {
                    (2027, 1)
                } else {
                    (2026, month + 1)
                };
                period(
                    &format!("Month {month}"),
                    (2026, month, 1),
                    (next_year, next_month, 1),
                )
            })
            .collect();
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
        assert!(plan.validate().is_ok());
    }
    #[test]
    fn cancellation_metrics_reject_filters_that_remove_cancelled_orders() {
        let mut plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::CancelledOrderCount],
            dimensions: vec![],
            periods: vec![period("July", (2026, 7, 1), (2026, 8, 1))],
            filters: AnalyticsFilters::default(),
            sort: None,
            limit: None,
            visualization: None,
        };

        plan.filters.status_filters = vec![StatusFilter {
            dimension: StatusDimension::Operational,
            values: vec!["CANCELED".to_owned()],
            exclude: true,
        }];

        assert_eq!(
            plan.validate(),
            Err(PlanValidationError::ContradictoryCancellationFilter)
        );

        plan.filters.status_filters.clear();
        plan.filters.exclude_cancelled = true;

        assert_eq!(
            plan.validate(),
            Err(PlanValidationError::ContradictoryCancellationFilter)
        );
    }
    #[test]
    fn period_comparison_rejects_overlapping_periods() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount, Metric::AverageOrderValue],
            dimensions: vec![Dimension::Period],
            periods: vec![
                period("May through July", (2026, 5, 1), (2026, 8, 1)),
                period("June", (2026, 6, 1), (2026, 7, 1)),
                period("July", (2026, 7, 1), (2026, 8, 1)),
            ],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Bar),
        };

        assert_eq!(
            plan.validate(),
            Err(PlanValidationError::OverlappingPeriods)
        );
    }
    #[test]
    fn period_comparison_accepts_adjacent_half_open_periods() {
        let plan = AnalyticsPlan {
            domain: AnalyticsDomain::Operations,
            metrics: vec![Metric::OrderCount, Metric::AverageOrderValue],
            dimensions: vec![Dimension::Period],
            periods: vec![
                period("May", (2026, 5, 1), (2026, 6, 1)),
                period("June", (2026, 6, 1), (2026, 7, 1)),
                period("July", (2026, 7, 1), (2026, 8, 1)),
            ],
            filters: AnalyticsFilters::default(),
            sort: Some(SortSpec::Chronological),
            limit: None,
            visualization: Some(VisualizationIntent::Bar),
        };

        assert!(plan.validate().is_ok());
    }
}
