use super::plan::{AnalyticsFilters, AnalyticsPlan, SortSpec, TimePeriod};
use chrono::{Datelike, Duration, Months, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub(crate) enum AnalysisOperation {
    MetricQuery(AnalyticsPlan),
    EntityList(EntityListPlan),
    Forecast(ForecastPlan),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntityType {
    Customer,
    Order,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct EntityListPlan {
    pub(crate) entity: EntityType,
    pub(crate) period: TimePeriod,
    pub(crate) minimum_order_count: Option<u32>,
    pub(crate) maximum_order_count: Option<u32>,
    pub(crate) inactive_days: Option<u32>,
    pub(crate) no_orders_since: Option<NaiveDate>,
    pub(crate) purchased_product: Option<String>,
    pub(crate) return_records: bool,
    pub(crate) sort: Option<SortSpec>,
    pub(crate) limit: u32,
}

impl EntityListPlan {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.period.start >= self.period.end_exclusive || self.limit == 0 || self.limit > 500 {
            return Err("entity plan has an invalid period or limit".to_owned());
        }
        if self
            .minimum_order_count
            .is_some_and(|value| value == 0 || value > 1000)
            || self.maximum_order_count.is_some_and(|value| value > 1000)
            || self
                .inactive_days
                .is_some_and(|value| value == 0 || value > 3650)
        {
            return Err("entity plan threshold is outside the supported range".to_owned());
        }
        if let (Some(minimum), Some(maximum)) = (self.minimum_order_count, self.maximum_order_count)
        {
            if minimum > maximum {
                return Err("minimum order count exceeds maximum".to_owned());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ForecastMetric {
    OrderCount,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TimeBucket {
    Day,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ForecastPlan {
    pub(crate) metric: ForecastMetric,
    pub(crate) training_window: TrainingWindow,
    pub(crate) forecast_horizon: ForecastHorizon,
    pub(crate) grain: TimeBucket,
    #[serde(default)]
    pub(crate) filters: AnalyticsFilters,
}

impl ForecastPlan {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.training_window.count == 0 || self.training_window.count > 730 {
            return Err("forecast training-window count is outside the supported range".to_owned());
        }
        if self.forecast_horizon.count == 0 || self.forecast_horizon.count > 90 {
            return Err("forecast horizon is outside the supported range".to_owned());
        }
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        current_ist_date: NaiveDate,
    ) -> Result<ResolvedForecastPlan, String> {
        self.validate()?;
        let training_end = match self.training_window.kind {
            TrainingWindowKind::CalendarMonths => {
                NaiveDate::from_ymd_opt(current_ist_date.year(), current_ist_date.month(), 1)
                    .ok_or_else(|| "forecast month boundary was invalid".to_owned())?
            }
            TrainingWindowKind::CalendarWeeks => {
                current_ist_date
                    - Duration::days(match current_ist_date.weekday() {
                        Weekday::Mon => 0,
                        Weekday::Tue => 1,
                        Weekday::Wed => 2,
                        Weekday::Thu => 3,
                        Weekday::Fri => 4,
                        Weekday::Sat => 5,
                        Weekday::Sun => 6,
                    })
            }
            TrainingWindowKind::Days => current_ist_date,
        };
        let training_start = match self.training_window.kind {
            TrainingWindowKind::CalendarMonths => training_end
                .checked_sub_months(Months::new(self.training_window.count))
                .ok_or_else(|| "forecast month lookback was invalid".to_owned())?,
            TrainingWindowKind::CalendarWeeks => {
                training_end - Duration::weeks(i64::from(self.training_window.count))
            }
            TrainingWindowKind::Days => {
                training_end - Duration::days(i64::from(self.training_window.count))
            }
        };
        if (training_end - training_start).num_days() < 28 {
            return Err("forecasting requires at least 28 complete historical days".to_owned());
        }
        let forecast_start = current_ist_date + Duration::days(1);
        let forecast_end = match self.forecast_horizon.unit {
            ForecastHorizonUnit::Days => {
                forecast_start + Duration::days(i64::from(self.forecast_horizon.count))
            }
            ForecastHorizonUnit::CalendarMonths => forecast_start
                .checked_add_months(Months::new(self.forecast_horizon.count))
                .ok_or_else(|| "forecast calendar-month horizon was invalid".to_owned())?,
        };
        let forecast_days = (forecast_end - forecast_start).num_days();
        if !(1..=90).contains(&forecast_days) {
            return Err("resolved forecast horizon must be between 1 and 90 days".to_owned());
        }
        Ok(ResolvedForecastPlan {
            metric: self.metric,
            training_period: TimePeriod {
                label: format!(
                    "{} complete {}",
                    self.training_window.count,
                    self.training_window.kind.label()
                ),
                start: training_start,
                end_exclusive: training_end,
            },
            forecast_period: TimePeriod {
                label: format!(
                    "Next {} {}",
                    self.forecast_horizon.count,
                    self.forecast_horizon.unit.label()
                ),
                start: forecast_start,
                end_exclusive: forecast_end,
            },
            grain: self.grain,
            filters: self.filters.clone(),
            comparison_period: Some(TimePeriod {
                label: "Previous equivalent complete period".to_owned(),
                start: current_ist_date - Duration::days(forecast_days),
                end_exclusive: current_ist_date,
            }),
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrainingWindowKind {
    #[serde(rename = "complete_calendar_months")]
    CalendarMonths,
    #[serde(rename = "complete_calendar_weeks")]
    CalendarWeeks,
    #[serde(rename = "complete_days")]
    Days,
}

impl TrainingWindowKind {
    const fn label(self) -> &'static str {
        match self {
            Self::CalendarMonths => "calendar months",
            Self::CalendarWeeks => "calendar weeks",
            Self::Days => "days",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TrainingWindow {
    pub(crate) kind: TrainingWindowKind,
    pub(crate) count: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ForecastHorizonUnit {
    Days,
    CalendarMonths,
}

impl ForecastHorizonUnit {
    const fn label(self) -> &'static str {
        match self {
            Self::Days => "days",
            Self::CalendarMonths => "calendar months",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ForecastHorizon {
    pub(crate) unit: ForecastHorizonUnit,
    pub(crate) count: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ResolvedForecastPlan {
    pub(crate) metric: ForecastMetric,
    pub(crate) training_period: TimePeriod,
    pub(crate) forecast_period: TimePeriod,
    pub(crate) grain: TimeBucket,
    pub(crate) filters: AnalyticsFilters,
    pub(crate) comparison_period: Option<TimePeriod>,
}

#[cfg(test)]
mod forecast_tests {
    use super::*;

    #[test]
    fn resolves_six_complete_months_and_exact_fourteen_day_horizon() {
        let plan = ForecastPlan {
            metric: ForecastMetric::OrderCount,
            training_window: TrainingWindow {
                kind: TrainingWindowKind::CalendarMonths,
                count: 6,
            },
            forecast_horizon: ForecastHorizon {
                unit: ForecastHorizonUnit::Days,
                count: 14,
            },
            grain: TimeBucket::Day,
            filters: AnalyticsFilters::default(),
        };
        let resolved = plan
            .resolve(NaiveDate::from_ymd_opt(2026, 8, 13).unwrap())
            .unwrap();
        assert_eq!(
            resolved.training_period.start,
            NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()
        );
        assert_eq!(
            resolved.training_period.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
        assert_eq!(
            resolved.forecast_period.start,
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap()
        );
        assert_eq!(
            resolved.forecast_period.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 28).unwrap()
        );
        assert_eq!(
            (resolved.forecast_period.end_exclusive - resolved.forecast_period.start).num_days(),
            14
        );

        let comparison = resolved
            .comparison_period
            .as_ref()
            .expect("forecast should have a comparison period");

        assert_eq!(
            comparison.start,
            NaiveDate::from_ymd_opt(2026, 7, 30).unwrap()
        );

        assert_eq!(
            comparison.end_exclusive,
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        );
    }
}
