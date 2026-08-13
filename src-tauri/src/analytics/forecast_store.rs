use super::{
    operation::ResolvedForecastPlan,
    statistics::{CandidateMetrics, ForecastPoint},
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ForecastSnapshot {
    pub(crate) forecast_id: String,
    pub(crate) created_at: String,
    pub(crate) data_as_of: String,
    pub(crate) plan: ResolvedForecastPlan,
    pub(crate) selected_method: String,
    pub(crate) backtest: CandidateMetrics,
    pub(crate) candidates: Vec<CandidateMetrics>,
    pub(crate) projections: Vec<ForecastPoint>,
}

#[derive(Clone)]
pub(crate) struct ForecastStore {
    path: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl ForecastStore {
    pub(crate) fn new(app_data_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: Arc::new(app_data_dir.into().join("forecast-snapshots.json")),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn test_default() -> Self {
        Self::new(std::env::temp_dir().join(format!("mill-it-analytics-{}", std::process::id())))
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn save(&self, snapshot: ForecastSnapshot) -> io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("forecast store lock poisoned"))?;
        let mut snapshots = self.load_unlocked()?;
        snapshots.push(snapshot);
        if snapshots.len() > 200 {
            snapshots.drain(..snapshots.len() - 200);
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&snapshots)?)?;
        fs::rename(temporary, self.path.as_ref())
    }

    pub(crate) fn latest(&self) -> io::Result<Option<ForecastSnapshot>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("forecast store lock poisoned"))?;
        Ok(self.load_unlocked()?.pop())
    }

    fn load_unlocked(&self) -> io::Result<Vec<ForecastSnapshot>> {
        match fs::read(self.path.as_ref()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::operation::{ForecastMetric, ResolvedForecastPlan, TimeBucket};
    use crate::analytics::plan::{AnalyticsFilters, TimePeriod};
    use crate::analytics::statistics::ForecastMethod;
    use chrono::NaiveDate;

    #[test]
    fn snapshot_survives_store_reconstruction() {
        let directory = std::env::temp_dir().join(format!(
            "mill-it-forecast-store-test-{}",
            std::process::id()
        ));
        let store = ForecastStore::new(&directory);
        let _ = fs::remove_file(store.path());
        let period = TimePeriod {
            label: "test".to_owned(),
            start: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
        };
        store
            .save(ForecastSnapshot {
                forecast_id: "forecast-test".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                data_as_of: "2026-01-01".to_owned(),
                plan: ResolvedForecastPlan {
                    metric: ForecastMetric::OrderCount,
                    training_period: period.clone(),
                    forecast_period: period,
                    grain: TimeBucket::Day,
                    filters: AnalyticsFilters::default(),
                    comparison_period: None,
                },
                selected_method: ForecastMethod::SameWeekdayPreviousWeek.label().to_owned(),
                backtest: CandidateMetrics {
                    method: ForecastMethod::SameWeekdayPreviousWeek,
                    mae: 1.0,
                    wape: 2.0,
                    evaluated_days: 10,
                },
                candidates: Vec::new(),
                projections: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            ForecastStore::new(&directory)
                .latest()
                .unwrap()
                .unwrap()
                .forecast_id,
            "forecast-test"
        );
        let _ = fs::remove_file(store.path());
    }
}
