use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DailyObservation {
    pub(crate) date: NaiveDate,
    pub(crate) orders: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ForecastPoint {
    pub(crate) date: NaiveDate,
    pub(crate) forecast: f64,
    pub(crate) low: f64,
    pub(crate) high: f64,
    pub(crate) threshold: f64,
    pub(crate) classification: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ForecastMethod {
    SameWeekdayPreviousWeek,
    MeanPreviousFourMatchingWeekdays,
    MedianPreviousFourMatchingWeekdays,
    RecencyWeightedMatchingWeekdays,
    TrendAdjustedMatchingWeekdayMean,
}

impl ForecastMethod {
    pub(crate) const ALL: [Self; 5] = [
        Self::SameWeekdayPreviousWeek,
        Self::MeanPreviousFourMatchingWeekdays,
        Self::MedianPreviousFourMatchingWeekdays,
        Self::RecencyWeightedMatchingWeekdays,
        Self::TrendAdjustedMatchingWeekdayMean,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::SameWeekdayPreviousWeek => "same_weekday_previous_week",
            Self::MeanPreviousFourMatchingWeekdays => "mean_previous_4_matching_weekdays",
            Self::MedianPreviousFourMatchingWeekdays => "median_previous_4_matching_weekdays",
            Self::RecencyWeightedMatchingWeekdays => "recency_weighted_matching_weekdays",
            Self::TrendAdjustedMatchingWeekdayMean => "trend_adjusted_matching_weekday_mean",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CandidateMetrics {
    pub(crate) method: ForecastMethod,
    pub(crate) mae: f64,
    pub(crate) wape: f64,
    pub(crate) evaluated_days: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ForecastSelection {
    pub(crate) selected: CandidateMetrics,
    pub(crate) candidates: Vec<CandidateMetrics>,
    pub(crate) points: Vec<ForecastPoint>,
}

pub(crate) fn median(values: &[f64]) -> Option<f64> {
    percentile(values, 0.5)
}

pub(crate) fn percentile(values: &[f64], percentile: f64) -> Option<f64> {
    let mut sorted = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let index = (sorted.len() - 1) as f64 * percentile.clamp(0.0, 1.0);
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    if lower == upper {
        Some(sorted[lower])
    } else {
        let weight = index - lower as f64;
        Some(sorted[lower] * (1.0 - weight) + sorted[upper] * weight)
    }
}

pub(crate) fn median_absolute_deviation(values: &[f64]) -> Option<f64> {
    let center = median(values)?;
    median(
        &values
            .iter()
            .map(|value| (value - center).abs())
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn select_and_forecast(
    history: &[DailyObservation],
    first_forecast_date: NaiveDate,
    horizon_days: i64,
) -> Result<ForecastSelection, String> {
    if history.len() < 28 || !(1..=90).contains(&horizon_days) {
        return Err(
            "forecasting requires at least 28 complete days and a 1-90 day horizon".to_owned(),
        );
    }
    let validation_days = history.len().saturating_sub(28).min(42);
    let candidates = ForecastMethod::ALL
        .iter()
        .map(|method| backtest_candidate(history, *method, validation_days))
        .collect::<Vec<_>>();
    let selected = candidates
        .iter()
        .min_by(|left, right| {
            left.mae
                .total_cmp(&right.mae)
                .then(left.wape.total_cmp(&right.wape))
        })
        .cloned()
        .ok_or_else(|| "forecast candidate selection failed".to_owned())?;
    let volumes = history.iter().map(|row| row.orders).collect::<Vec<_>>();
    let busy = percentile(&volumes, 0.75).unwrap_or(0.0);
    let elevated = percentile(&volumes, 0.60).unwrap_or(busy);
    let spread = (selected.mae * 1.5).max(1.0);
    let points = (0..horizon_days)
        .map(|offset| {
            let date = first_forecast_date + Duration::days(offset);
            let forecast = predict(history, date, selected.method).max(0.0);
            let classification = if forecast >= busy {
                "likely_busy"
            } else if forecast >= elevated {
                "elevated"
            } else {
                "normal"
            };
            ForecastPoint {
                date,
                forecast,
                low: (forecast - spread).max(0.0),
                high: forecast + spread,
                threshold: busy,
                classification: classification.to_owned(),
            }
        })
        .collect();
    Ok(ForecastSelection {
        selected,
        candidates,
        points,
    })
}

fn backtest_candidate(
    history: &[DailyObservation],
    method: ForecastMethod,
    validation_days: usize,
) -> CandidateMetrics {
    let start = history.len().saturating_sub(validation_days).max(28);
    let mut absolute_error = 0.0;
    let mut actual_total = 0.0;
    let mut evaluated = 0;
    for index in start..history.len() {
        let actual = history[index].orders;
        let predicted = predict(&history[..index], history[index].date, method);
        absolute_error += (predicted - actual).abs();
        actual_total += actual.abs();
        evaluated += 1;
    }
    CandidateMetrics {
        method,
        mae: if evaluated == 0 {
            f64::INFINITY
        } else {
            absolute_error / evaluated as f64
        },
        wape: if actual_total == 0.0 {
            0.0
        } else {
            absolute_error / actual_total * 100.0
        },
        evaluated_days: evaluated,
    }
}

fn predict(history: &[DailyObservation], target: NaiveDate, method: ForecastMethod) -> f64 {
    if history.is_empty() {
        return 0.0;
    }
    let matching = history
        .iter()
        .filter(|row| row.date < target && row.date.weekday() == target.weekday())
        .rev()
        .take(4)
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return mean(
            &history
                .iter()
                .rev()
                .take(28)
                .map(|row| row.orders)
                .collect::<Vec<_>>(),
        );
    }
    match method {
        ForecastMethod::SameWeekdayPreviousWeek => matching[0].orders,
        ForecastMethod::MeanPreviousFourMatchingWeekdays => {
            mean(&matching.iter().map(|row| row.orders).collect::<Vec<_>>())
        }
        ForecastMethod::MedianPreviousFourMatchingWeekdays => {
            median(&matching.iter().map(|row| row.orders).collect::<Vec<_>>()).unwrap_or(0.0)
        }
        ForecastMethod::RecencyWeightedMatchingWeekdays => {
            let weights = [4.0, 3.0, 2.0, 1.0];
            let total = matching
                .iter()
                .enumerate()
                .map(|(index, row)| row.orders * weights[index])
                .sum::<f64>();
            total / weights[..matching.len()].iter().sum::<f64>()
        }
        ForecastMethod::TrendAdjustedMatchingWeekdayMean => {
            let weekday_mean = mean(&matching.iter().map(|row| row.orders).collect::<Vec<_>>());
            let recent = mean(
                &history
                    .iter()
                    .rev()
                    .take(14)
                    .map(|row| row.orders)
                    .collect::<Vec<_>>(),
            );
            let prior = mean(
                &history
                    .iter()
                    .rev()
                    .skip(14)
                    .take(14)
                    .map(|row| row.orders)
                    .collect::<Vec<_>>(),
            );
            weekday_mean
                * if prior > 0.0 {
                    (recent / prior).clamp(0.75, 1.25)
                } else {
                    1.0
                }
        }
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(days: i64) -> Vec<DailyObservation> {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        (0..days)
            .map(|day| DailyObservation {
                date: start + Duration::days(day),
                orders: if day == 50 {
                    500.0
                } else {
                    30.0 + ((day + 3) % 7) as f64 * 4.0
                },
            })
            .collect()
    }

    #[test]
    fn selection_tests_all_candidates_and_chooses_minimum_mae() {
        let data = history(120);
        let selection =
            select_and_forecast(&data, data.last().unwrap().date + Duration::days(1), 14).unwrap();
        assert_eq!(selection.candidates.len(), 5);
        assert_eq!(selection.points.len(), 14);
        let minimum = selection
            .candidates
            .iter()
            .map(|metrics| metrics.mae)
            .min_by(f64::total_cmp)
            .unwrap();
        assert_eq!(selection.selected.mae, minimum);
    }

    #[test]
    fn handles_zero_days_missing_days_outliers_and_limits() {
        let mut data = history(35);
        data[3].orders = 0.0;
        data.remove(8);
        assert!(
            select_and_forecast(&data, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(), 7).is_ok()
        );
        assert!(
            select_and_forecast(&data[..20], NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(), 7)
                .is_err()
        );
        assert!(
            select_and_forecast(&data, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(), 91).is_err()
        );
    }
}
