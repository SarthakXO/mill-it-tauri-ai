use chrono::NaiveDate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FastToolKind {
    OrderCount,
    OrderTimeseries,
    TopVariants,
    StatusBreakdown,
    LapsedCustomers,
    ReorderDemand,
    BusyDayForecast,
    ProductPerformance,
}

impl FastToolKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OrderCount => "order_count",
            Self::OrderTimeseries => "order_timeseries",
            Self::TopVariants => "top_variants",
            Self::StatusBreakdown => "status_breakdown",
            Self::LapsedCustomers => "lapsed_customers",
            Self::ReorderDemand => "reorder_demand",
            Self::BusyDayForecast => "busy_day_forecast",
            Self::ProductPerformance => "product_performance",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeBucket {
    Day,
    Week,
    Month,
}

impl TimeBucket {
    pub(crate) const fn sql_unit(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PerformanceDirection {
    Underperforming,
    Overperforming,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DateRange {
    pub(crate) start: NaiveDate,
    pub(crate) end_exclusive: NaiveDate,
}

impl DateRange {
    pub(crate) fn days(&self) -> i64 {
        (self.end_exclusive - self.start).num_days().max(1)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ToolPlan {
    OrderCount {
        range: DateRange,
    },
    OrderTimeseries {
        range: DateRange,
        bucket: TimeBucket,
    },
    TopVariants {
        range: DateRange,
        limit: usize,
    },
    StatusBreakdown {
        range: DateRange,
    },
    LapsedCustomers {
        minimum_orders: usize,
        lookback_months: u32,
        inactive_days: i64,
        product_filter: Option<String>,
        return_records: bool,
    },
    ReorderDemand {
        horizon_days: i64,
        limit: usize,
    },
    BusyDayForecast {
        horizon_days: i64,
        lookback_months: u32,
    },
    ProductPerformance {
        current: DateRange,
        baseline: DateRange,
        direction: PerformanceDirection,
        limit: usize,
    },
}

impl ToolPlan {
    pub(crate) const fn kind(&self) -> FastToolKind {
        match self {
            Self::OrderCount { .. } => FastToolKind::OrderCount,
            Self::OrderTimeseries { .. } => FastToolKind::OrderTimeseries,
            Self::TopVariants { .. } => FastToolKind::TopVariants,
            Self::StatusBreakdown { .. } => FastToolKind::StatusBreakdown,
            Self::LapsedCustomers { .. } => FastToolKind::LapsedCustomers,
            Self::ReorderDemand { .. } => FastToolKind::ReorderDemand,
            Self::BusyDayForecast { .. } => FastToolKind::BusyDayForecast,
            Self::ProductPerformance { .. } => FastToolKind::ProductPerformance,
        }
    }
}
