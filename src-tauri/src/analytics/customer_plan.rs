use super::plan::TimePeriod;
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

pub(crate) const LOYAL_CUSTOMER_MIN_ORDERS: u32 = 3;
pub(crate) const LOYAL_CUSTOMER_LOOKBACK_MONTHS: u32 = 4;

pub(crate) const MAX_PURCHASE_ORDINAL: u32 = 20;
pub(crate) const MAX_FREQUENCY_BUCKET: u32 = 100;
pub(crate) const MAX_RETENTION_WINDOWS: usize = 8;
pub(crate) const MAX_RETENTION_WINDOW_DAYS: u32 = 365;
pub(crate) const MAX_CUSTOMER_ANALYSIS_LIMIT: u32 = 100;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CustomerSegment {
    /*
     * All qualifying customers represented by the requested analysis.
     */
    All,

    /*
     * Mill-it business definition:
     *
     * >= 3 qualifying orders in the trailing 4-month eligibility
     * window ending at the analysis as-of boundary.
     *
     * The eligibility window determines segment membership only.
     * It does NOT reset lifetime purchase ordinals.
     */
    Loyal,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CustomerProgressionSubject {
    Category,
    Product,
    Variant,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CustomerReorderGrain {
    Product,
    Variant,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CustomerAnalysis {
    /*
     * Example:
     *
     * "For loyal customers, what categories are purchased on
     * their first, second and third orders?"
     */
    PurchaseProgression {
        subject: CustomerProgressionSubject,
        max_purchase_number: u32,
    },

    /*
     * Example:
     *
     * "For loyal customers, show how categories transition
     * from purchase 1 to 2 and 2 to 3."
     *
     * A customer's order can contain multiple categories.
     * Therefore this represents category-pair incidence rather
     * than an exclusive single-category state transition.
     */
    PurchaseTransition {
        subject: CustomerProgressionSubject,
        max_purchase_number: u32,
    },

    /*
     * Example:
     *
     * "Which products had the highest reorder rate?"
     *
     * Reordering means another qualifying order containing the
     * same product/variant, never multiple units in one order.
     */
    ProductReorder {
        grain: CustomerReorderGrain,
        minimum_buyers: u32,
        limit: u32,
    },

    /*
     * Example:
     *
     * "How many customers placed 1, 2, 3, 4 or more orders?"
     *
     * max_order_count_bucket controls the final explicit threshold.
     * The executor may present the final bucket as N+.
     */
    FrequencyDistribution {
        max_order_count_bucket: u32,
    },

    /*
     * Example:
     *
     * "Of first-time customers, how many returned within
     * 30, 60 and 90 days?"
     */
    CohortRetention {
        windows_days: Vec<u32>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CustomerAnalysisPlan {
    /*
     * Analysis population / observation period.
     *
     * The executor may deliberately inspect qualifying history
     * before this period when the analysis requires lifetime
     * purchase ordinals.
     */
    pub(crate) period: TimePeriod,

    pub(crate) segment: CustomerSegment,

    pub(crate) analysis: CustomerAnalysis,
}

impl CustomerAnalysisPlan {
    pub(crate) fn validate(&self) -> Result<(), CustomerPlanValidationError> {
        validate_period(&self.period)?;

        match &self.analysis {
            CustomerAnalysis::PurchaseProgression {
                max_purchase_number,
                ..
            }
            | CustomerAnalysis::PurchaseTransition {
                max_purchase_number,
                ..
            } => {
                if !(2..=MAX_PURCHASE_ORDINAL).contains(max_purchase_number) {
                    return Err(CustomerPlanValidationError::InvalidPurchaseOrdinal);
                }
            }

            CustomerAnalysis::ProductReorder {
                minimum_buyers,
                limit,
                ..
            } => {
                if *minimum_buyers == 0 {
                    return Err(CustomerPlanValidationError::InvalidMinimumPopulation);
                }

                if *limit == 0 || *limit > MAX_CUSTOMER_ANALYSIS_LIMIT {
                    return Err(CustomerPlanValidationError::InvalidLimit);
                }
            }

            CustomerAnalysis::FrequencyDistribution {
                max_order_count_bucket,
            } => {
                if !(2..=MAX_FREQUENCY_BUCKET).contains(max_order_count_bucket) {
                    return Err(CustomerPlanValidationError::InvalidFrequencyBucket);
                }
            }

            CustomerAnalysis::CohortRetention { windows_days } => {
                if windows_days.is_empty() || windows_days.len() > MAX_RETENTION_WINDOWS {
                    return Err(CustomerPlanValidationError::InvalidRetentionWindows);
                }

                let valid_values = windows_days
                    .iter()
                    .all(|days| *days > 0 && *days <= MAX_RETENTION_WINDOW_DAYS);

                let strictly_increasing =
                    windows_days.windows(2).all(|window| window[0] < window[1]);

                if !valid_values || !strictly_increasing {
                    return Err(CustomerPlanValidationError::InvalidRetentionWindows);
                }
            }
        }

        Ok(())
    }
}

fn validate_period(period: &TimePeriod) -> Result<(), CustomerPlanValidationError> {
    if period.label.trim().is_empty()
        || period.label.len() > 80
        || period.start >= period.end_exclusive
        || period.start.year() < 2000
        || period.end_exclusive.year() > 2101
    {
        return Err(CustomerPlanValidationError::InvalidPeriod);
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CustomerPlanValidationError {
    InvalidPeriod,
    InvalidPurchaseOrdinal,
    InvalidMinimumPopulation,
    InvalidLimit,
    InvalidFrequencyBucket,
    InvalidRetentionWindows,
}

impl fmt::Display for CustomerPlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPeriod => {
                "customer analysis has an invalid period"
            }

            Self::InvalidPurchaseOrdinal => {
                "customer purchase progression requires a purchase number between 2 and 20"
            }

            Self::InvalidMinimumPopulation => {
                "customer analysis minimum population must be greater than zero"
            }

            Self::InvalidLimit => {
                "customer analysis limit must be between 1 and 100"
            }

            Self::InvalidFrequencyBucket => {
                "customer frequency distribution requires a maximum bucket between 2 and 100"
            }

            Self::InvalidRetentionWindows => {
                "customer retention windows must contain 1 to 8 strictly increasing day values between 1 and 365"
            }
        };

        formatter.write_str(message)
    }
}

impl Error for CustomerPlanValidationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn period() -> TimePeriod {
        TimePeriod {
            label: "January through July 2026".to_owned(),
            start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        }
    }

    #[test]
    fn loyal_segment_business_definition_is_locked() {
        assert_eq!(LOYAL_CUSTOMER_MIN_ORDERS, 3);
        assert_eq!(LOYAL_CUSTOMER_LOOKBACK_MONTHS, 4);
    }

    #[test]
    fn loyal_category_progression_through_third_purchase_is_valid() {
        let plan = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::Loyal,
            analysis: CustomerAnalysis::PurchaseProgression {
                subject: CustomerProgressionSubject::Category,
                max_purchase_number: 3,
            },
        };

        assert!(plan.validate().is_ok());
    }

    #[test]
    fn purchase_progression_rejects_non_progression_and_unbounded_history() {
        let first_only = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::PurchaseProgression {
                subject: CustomerProgressionSubject::Product,
                max_purchase_number: 1,
            },
        };

        assert_eq!(
            first_only.validate(),
            Err(CustomerPlanValidationError::InvalidPurchaseOrdinal)
        );

        let too_many = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::PurchaseProgression {
                subject: CustomerProgressionSubject::Product,
                max_purchase_number: MAX_PURCHASE_ORDINAL + 1,
            },
        };

        assert_eq!(
            too_many.validate(),
            Err(CustomerPlanValidationError::InvalidPurchaseOrdinal)
        );
    }

    #[test]
    fn product_reorder_requires_bounded_population_and_result_limit() {
        let valid = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::ProductReorder {
                grain: CustomerReorderGrain::Product,
                minimum_buyers: 10,
                limit: 20,
            },
        };

        assert!(valid.validate().is_ok());

        let invalid = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::ProductReorder {
                grain: CustomerReorderGrain::Product,
                minimum_buyers: 0,
                limit: 20,
            },
        };

        assert_eq!(
            invalid.validate(),
            Err(CustomerPlanValidationError::InvalidMinimumPopulation)
        );
    }

    #[test]
    fn frequency_distribution_supports_open_ended_final_bucket() {
        let plan = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::FrequencyDistribution {
                max_order_count_bucket: 10,
            },
        };

        assert!(plan.validate().is_ok());
    }

    #[test]
    fn retention_windows_must_be_unique_ordered_and_bounded() {
        let valid = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::CohortRetention {
                windows_days: vec![30, 60, 90],
            },
        };

        assert!(valid.validate().is_ok());

        let duplicate = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::CohortRetention {
                windows_days: vec![30, 30, 90],
            },
        };

        assert_eq!(
            duplicate.validate(),
            Err(CustomerPlanValidationError::InvalidRetentionWindows)
        );

        let unordered = CustomerAnalysisPlan {
            period: period(),
            segment: CustomerSegment::All,
            analysis: CustomerAnalysis::CohortRetention {
                windows_days: vec![60, 30, 90],
            },
        };

        assert_eq!(
            unordered.validate(),
            Err(CustomerPlanValidationError::InvalidRetentionWindows)
        );
    }
    #[test]
    fn loyal_category_transition_through_third_purchase_is_valid() {
        let plan = CustomerAnalysisPlan {
            period: period(),

            segment: CustomerSegment::Loyal,

            analysis: CustomerAnalysis::PurchaseTransition {
                subject: CustomerProgressionSubject::Category,

                max_purchase_number: 3,
            },
        };

        assert!(plan.validate().is_ok());
    }
}
