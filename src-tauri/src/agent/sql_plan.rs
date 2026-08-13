use crate::{analytics::ForecastPlan, intelligence::IntelligenceDomain};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SqlPlannerRoute {
    Sql,
    Forecast,
    Specialized,
    Conversation,
    Blocked,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SqlPlannerResponse {
    pub(crate) route: SqlPlannerRoute,
    #[serde(default)]
    pub(crate) domains: Vec<IntelligenceDomain>,
    #[serde(default)]
    pub(crate) query: Option<SqlQueryPlan>,
    #[serde(default)]
    pub(crate) forecast_plan: Option<ForecastPlan>,
    #[serde(default)]
    pub(crate) specialized_tool: Option<String>,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

impl SqlPlannerResponse {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.domains.is_empty() || self.domains.len() > 6 {
            return Err("planner domains must contain one through six known domains".to_owned());
        }
        match self.route {
            SqlPlannerRoute::Sql => self
                .query
                .as_ref()
                .ok_or_else(|| "sql route requires query".to_owned())?
                .validate(),
            SqlPlannerRoute::Forecast => self
                .forecast_plan
                .as_ref()
                .ok_or_else(|| "forecast route requires forecast_plan".to_owned())?
                .validate(),
            SqlPlannerRoute::Specialized => matches!(
                self.specialized_tool.as_deref(),
                Some("reorder_demand" | "product_performance" | "busy_day_forecast")
            )
            .then_some(())
            .ok_or_else(|| "specialized route requires a supported algorithm".to_owned()),
            SqlPlannerRoute::Conversation => Ok(()),
            SqlPlannerRoute::Blocked => self
                .message
                .as_deref()
                .filter(|message| !message.trim().is_empty() && message.len() <= 300)
                .map(|_| ())
                .ok_or_else(|| "blocked route requires a concise message".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SqlQueryPlan {
    pub(crate) sql: String,
    #[serde(default)]
    pub(crate) parameters: Vec<Value>,
    pub(crate) result_shape: SqlResultShape,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) x_column: Option<String>,
    #[serde(default)]
    pub(crate) metrics: Vec<String>,
    pub(crate) visualization: SqlVisualization,
}

impl SqlQueryPlan {
    fn validate(&self) -> Result<(), String> {
        if self.sql.trim().is_empty() || self.sql.len() > 20_000 {
            return Err("SQL is empty or too large".to_owned());
        }
        if self.title.trim().is_empty() || self.title.len() > 120 {
            return Err("query title is empty or too large".to_owned());
        }
        if self.parameters.len() > 64
            || self
                .parameters
                .iter()
                .any(|value| !is_scalar_parameter(value))
        {
            return Err("query parameters must be at most 64 JSON scalars".to_owned());
        }
        if self.metrics.len() > 6
            || self.metrics.iter().any(|metric| !safe_identifier(metric))
            || self
                .x_column
                .as_deref()
                .is_some_and(|column| !safe_identifier(column))
        {
            return Err("metrics or x_column contain an invalid identifier".to_owned());
        }
        if self.metrics.is_empty() {
            return Err(
                "SQL plans require at least one returned-column alias in metrics".to_owned(),
            );
        }
        if matches!(
            self.result_shape,
            SqlResultShape::Scalar | SqlResultShape::RecordList
        ) && !matches!(self.visualization, SqlVisualization::None)
        {
            return Err("scalar and record-list plans cannot request charts".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SqlResultShape {
    Scalar,
    Comparison,
    TimeSeries,
    Ranking,
    CategoryBreakdown,
    RecordList,
    Table,
}

impl SqlResultShape {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Comparison => "comparison",
            Self::TimeSeries => "time_series",
            Self::Ranking => "ranking",
            Self::CategoryBreakdown => "category_breakdown",
            Self::RecordList => "record_list",
            Self::Table => "table",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SqlVisualization {
    None,
    Bar,
    HorizontalBar,
    Line,
}

fn is_scalar_parameter(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_scalar_parameters_and_arbitrary_ui_values() {
        let plan: SqlPlannerResponse = serde_json::from_value(serde_json::json!({
            "route":"sql",
            "domains":["orders"],
            "query":{
                "sql":"SELECT COUNT(*) AS order_count FROM \"order\"",
                "parameters":[{"unsafe":"value"}],
                "result_shape":"scalar",
                "title":"Orders",
                "x_column":null,
                "metrics":["order_count"],
                "visualization":"none"
            }
        }))
        .unwrap();
        assert!(plan.validate().is_err());
    }
}
