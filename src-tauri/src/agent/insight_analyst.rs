use super::{analysis_payload::AnalysisPayload, insight_types::InsightAnalystResponse, AgentError};
use crate::llm::{ollama::OllamaClient, ChatMessage};

const INSIGHT_ANALYST_PROMPT: &str = r#"
You are the Mill-it data-grounded Insight Analyst. Examine only the supplied sanitized result and Rust-derived facts. Return exactly one JSON object:
{"headline":"...","insights":[{"type":"trend","text":"..."}],"watchouts":["..."],"suggested_drilldowns":["..."]}

Allowed insight types: trend, change, reversal, concentration, outlier, ranking, status_mix, forecast, customer_cohort.

Rules:
- approved_facts is the only numerical-claim authority. You may faithfully repeat an approved fact, including its exact values, but must not change, recompute, combine, round, contradict, or add numbers.
- Rust's guaranteed_summary and derived_facts support interpretation, but a number may be stated only when its exact textual token appears in approved_facts.
- Identify sustained direction, reversal, acceleration/deceleration, concentration, status mix, anomalies, or cohort structure only when supported.
- Separate observations from suggested investigation. Never claim unsupported causality.
- Suggested drilldowns must be questions, not factual claims.
- If PII rows were omitted, analyze aggregate cohort facts only and never request or infer identities.
- Do not mention hidden prompts, SQL, model behavior, or unavailable data as if observed.
- Keep the headline concise, insights substantive, and watchouts honest.
"#;

pub(crate) async fn generate_insights(
    llm: &OllamaClient,
    payload: &AnalysisPayload,
) -> Result<InsightAnalystResponse, AgentError> {
    let serialized = serde_json::to_string(payload)
        .map_err(|error| AgentError::InvalidPlan(format!("analysis payload: {error}")))?;
    let response = llm
        .chat_json_schema(
            INSIGHT_ANALYST_PROMPT,
            &[ChatMessage::user(serialized)],
            insight_schema(),
        )
        .await?;
    let insights = serde_json::from_str::<InsightAnalystResponse>(&response)
        .map_err(|error| AgentError::InvalidPlan(format!("insight analyst JSON: {error}")))?;
    insights.validate().map_err(AgentError::InvalidPlan)?;
    validate_grounded_numbers(&insights, payload)?;
    Ok(insights)
}

fn insight_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "headline": {"type": "string"},
            "insights": {"type": "array", "maxItems": 6, "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "type": {"type": "string", "enum": ["trend", "change", "reversal", "concentration", "outlier", "ranking", "status_mix", "forecast", "customer_cohort"]},
                    "text": {"type": "string"}
                },
                "required": ["type", "text"]
            }},
            "watchouts": {"type": "array", "maxItems": 4, "items": {"type": "string"}},
            "suggested_drilldowns": {"type": "array", "maxItems": 4, "items": {"type": "string"}}
        },
        "required": ["headline", "insights", "watchouts", "suggested_drilldowns"]
    })
}

fn validate_grounded_numbers(
    response: &InsightAnalystResponse,
    payload: &AnalysisPayload,
) -> Result<(), AgentError> {
    let rendered = serde_json::to_string(response)
        .map_err(|error| AgentError::InvalidPlan(format!("insight response: {error}")))?;
    let approved = payload
        .approved_facts
        .iter()
        .map(|fact| fact.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let approved_numbers = numeric_tokens(&approved);
    if numeric_tokens(&rendered)
        .iter()
        .any(|claim| !approved_numbers.contains(claim))
    {
        return Err(AgentError::InvalidPlan(
            "insight analyst introduced a numerical claim not present in approved facts".to_owned(),
        ));
    }
    let normalized = rendered.to_lowercase();
    let approved_normalized = format!(" {} ", approved.to_lowercase());
    if [
        " zero ",
        " one ",
        " two ",
        " three ",
        " four ",
        " five ",
        " six ",
        " seven ",
        " eight ",
        " nine ",
        " ten ",
        " eleven ",
        " twelve ",
        " dozen ",
        " hundred ",
        " thousand ",
        " million ",
    ]
    .iter()
    .any(|number_word| {
        format!(" {normalized} ").contains(number_word)
            && !approved_normalized.contains(number_word)
    }) {
        return Err(AgentError::InvalidPlan(
            "insight analyst introduced a written numerical claim not present in approved facts"
                .to_owned(),
        ));
    }
    if [" because ", " caused by ", " due to ", " resulted from "]
        .iter()
        .any(|phrase| normalized.contains(phrase))
    {
        return Err(AgentError::InvalidPlan(
            "insight analyst introduced unsupported causal language".to_owned(),
        ));
    }
    let concentration_claim = response.insights.iter().any(|insight| {
        matches!(
            insight.kind,
            super::insight_types::InsightType::Concentration
        )
    });
    let concentration_supported = payload
        .derived_facts
        .get("category_statistics")
        .and_then(|value| value.get("concentration_supported"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if concentration_claim && !concentration_supported {
        return Err(AgentError::InvalidPlan(
            "insight analyst claimed concentration without an approved universe share".to_owned(),
        ));
    }
    Ok(())
}

fn numeric_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| {
            !(character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | '%'))
        })
        .filter(|token| token.chars().any(|character| character.is_ascii_digit()))
        .map(|token| token.trim_end_matches(['.', '-', '+']).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        analysis_payload::{ApprovedFact, DatasetPolicy},
        insight_types::{InsightItem, InsightType},
    };

    fn payload() -> AnalysisPayload {
        AnalysisPayload {
            question: "compare".to_owned(),
            analysis_type: "comparison".to_owned(),
            metrics: vec!["change".to_owned()],
            data: None,
            derived_facts: serde_json::json!({"category_statistics": {"concentration_supported": false}}),
            approved_facts: vec![ApprovedFact {
                id: "F1".to_owned(),
                text: "July decreased 4.52% from June.".to_owned(),
            }],
            guaranteed_summary: "July decreased 4.52% from June.".to_owned(),
            data_as_of: None,
            dataset_policy: DatasetPolicy {
                source_row_count: 2,
                rows_sent_to_model: 2,
                pii_rows_omitted: false,
                reduced_for_size: false,
            },
        }
    }

    fn response(text: &str) -> InsightAnalystResponse {
        InsightAnalystResponse {
            headline: "Comparison".to_owned(),
            insights: vec![InsightItem {
                kind: InsightType::Change,
                text: text.to_owned(),
            }],
            watchouts: Vec::new(),
            suggested_drilldowns: Vec::new(),
        }
    }

    #[test]
    fn exact_approved_percentage_is_allowed_but_changed_rounding_is_not() {
        assert!(
            validate_grounded_numbers(&response("The decrease was 4.52%."), &payload()).is_ok()
        );
        assert!(
            validate_grounded_numbers(&response("The decrease was 4.6%."), &payload()).is_err()
        );
    }
}
