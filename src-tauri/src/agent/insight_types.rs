use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct InsightAnalystResponse {
    pub(crate) headline: String,
    #[serde(default)]
    pub(crate) insights: Vec<InsightItem>,
    #[serde(default)]
    pub(crate) watchouts: Vec<String>,
    #[serde(default)]
    pub(crate) suggested_drilldowns: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct InsightItem {
    #[serde(rename = "type")]
    pub(crate) kind: InsightType,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InsightType {
    Trend,
    Change,
    Reversal,
    Concentration,
    Outlier,
    Ranking,
    StatusMix,
    Forecast,
    CustomerCohort,
}

impl InsightAnalystResponse {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !valid_text(&self.headline, 240)
            || self.insights.len() > 6
            || self.watchouts.len() > 4
            || self.suggested_drilldowns.len() > 4
            || self
                .insights
                .iter()
                .any(|item| !valid_text(&item.text, 500))
            || self.watchouts.iter().any(|item| !valid_text(item, 400))
            || self
                .suggested_drilldowns
                .iter()
                .any(|item| !valid_text(item, 400))
        {
            return Err("insight response exceeded the bounded contract".to_owned());
        }
        Ok(())
    }

    pub(crate) fn render(&self, guaranteed_summary: &str) -> String {
        let mut sections = vec![self.headline.clone(), guaranteed_summary.to_owned()];
        if !self.insights.is_empty() {
            sections.push(
                self.insights
                    .iter()
                    .map(|item| item.text.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        if !self.watchouts.is_empty() {
            sections.push(format!("Watchout: {}", self.watchouts.join(" ")));
        }
        if !self.suggested_drilldowns.is_empty() {
            sections.push(format!(
                "Useful follow-up: {}",
                self.suggested_drilldowns.join(" ")
            ));
        }
        sections.join("\n\n")
    }
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum
}
