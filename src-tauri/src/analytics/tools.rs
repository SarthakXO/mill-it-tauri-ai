use super::{
    customers::{lapsed_customers, reorder_demand},
    forecasting::busy_day_forecast,
    orders::{order_count, order_timeseries, status_breakdown},
    products::{product_performance, top_variants},
    types::ToolPlan,
};
use crate::{
    agent::{AgentError, AgentResponse},
    database::Database,
};

pub(crate) async fn execute_tool(
    database: &Database,
    question: &str,
    data_as_of: &str,
    plan: &ToolPlan,
) -> Result<AgentResponse, AgentError> {
    match plan {
        ToolPlan::OrderCount { range } => order_count(database, question, range).await,
        ToolPlan::OrderTimeseries { range, bucket } => {
            order_timeseries(database, question, range, *bucket).await
        }
        ToolPlan::TopVariants { range, limit } => {
            top_variants(database, question, range, *limit).await
        }
        ToolPlan::StatusBreakdown { range } => status_breakdown(database, question, range).await,
        ToolPlan::LapsedCustomers {
            minimum_orders,
            lookback_months,
            inactive_days,
            product_filter,
            return_records,
        } => {
            lapsed_customers(
                database,
                question,
                data_as_of,
                *minimum_orders,
                *lookback_months,
                *inactive_days,
                product_filter.as_deref(),
                *return_records,
            )
            .await
        }
        ToolPlan::ReorderDemand {
            horizon_days,
            limit,
        } => reorder_demand(database, question, data_as_of, *horizon_days, *limit).await,
        ToolPlan::BusyDayForecast {
            horizon_days,
            lookback_months,
        } => {
            busy_day_forecast(
                database,
                question,
                data_as_of,
                *horizon_days,
                *lookback_months,
            )
            .await
        }
        ToolPlan::ProductPerformance {
            current,
            baseline,
            direction,
            limit,
        } => product_performance(database, question, current, baseline, *direction, *limit).await,
    }
}
