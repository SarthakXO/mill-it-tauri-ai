use crate::llm::{ChatMessage, ChatRole};
use serde::Serialize;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnalyticalShape {
    Scalar,
    Comparison,
    TimeSeries,
    CategoryBreakdown,
    Ranking,
    RecordList,
}

impl AnalyticalShape {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Comparison => "comparison",
            Self::TimeSeries => "time_series",
            Self::CategoryBreakdown => "category_breakdown",
            Self::Ranking => "ranking",
            Self::RecordList => "record_list",
        }
    }

    pub(crate) fn classify(question: &str) -> Self {
        let question = question.to_lowercase();
        let comparison = contains_any(&question, &["compare", "comparison", " versus ", " vs "]);
        let time_series = contains_any(
            &question,
            &[
                "daily",
                "weekly",
                "monthly",
                "by day",
                "by week",
                "by month",
                "by hour",
                "over time",
                "time series",
                "trend",
                "plot",
                "graph",
            ],
        );
        let category = contains_any(
            &question,
            &[
                "by status",
                "based on status",
                "based on their status",
                "by store",
                "by payment method",
                "breakdown",
            ],
        );
        let ranking = contains_any(
            &question,
            &[
                "top ",
                "best-selling",
                "best selling",
                "most selling",
                "highest-volume",
                "highest volume",
                "rank ",
            ],
        ) || ((question.contains("sku") || question.contains("variant"))
            && contains_any(&question, &["sales", "sold", "selling", "quantity"])
            && !contains_any(&question, &["how many", "count"]));
        let record_list = is_record_list_question(&question);
        if comparison {
            Self::Comparison
        } else if time_series {
            Self::TimeSeries
        } else if category {
            Self::CategoryBreakdown
        } else if ranking {
            Self::Ranking
        } else if record_list {
            Self::RecordList
        } else {
            Self::Scalar
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AnalyticsRequestContext {
    pub(crate) request_id: String,
    pub(crate) current_user_message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) relevant_conversation_context: Vec<String>,
}

impl AnalyticsRequestContext {
    pub(crate) fn from_messages(messages: &[ChatMessage]) -> Option<Self> {
        let latest_index = messages
            .iter()
            .rposition(|message| matches!(message.role, ChatRole::User))?;
        let current = messages.get(latest_index)?.content.trim();
        if current.is_empty() {
            return None;
        }
        let prior = &messages[..latest_index];
        let failed_prior_request = prior
            .iter()
            .rev()
            .find(|message| matches!(message.role, ChatRole::Assistant))
            .is_some_and(|message| looks_like_failed_request(&message.content));
        let relevant_conversation_context =
            if is_referential_follow_up(current) && !failed_prior_request {
                prior
                    .iter()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|message| format!("{}: {}", message.role.as_str(), message.content.trim()))
                    .collect()
            } else {
                Vec::new()
            };
        Some(Self {
            request_id: request_id(),
            current_user_message: current.to_owned(),
            relevant_conversation_context,
        })
    }

    pub(crate) fn isolated_prompt(&self, purpose: &str) -> ChatMessage {
        ChatMessage::user(
            serde_json::json!({
                "instruction": purpose,
                "current_analytical_request": self.current_user_message,
                "optional_prior_conversation_context": self.relevant_conversation_context,
                "priority_rule": "The current analytical request is authoritative. Use optional prior context only to resolve an explicit reference such as that/those/same/it. Never import a prior date, SQL fragment, result, CTE, or parameter into a self-contained request."
            })
            .to_string(),
        )
    }

    pub(crate) fn analytical_scope(&self) -> String {
        if self.relevant_conversation_context.is_empty() {
            self.current_user_message.clone()
        } else {
            format!(
                "CURRENT: {}\nREFERENTIAL CONTEXT: {}",
                self.current_user_message,
                self.relevant_conversation_context.join("\n")
            )
        }
    }

    pub(crate) fn referential_user_context(&self) -> Option<&str> {
        self.relevant_conversation_context
            .iter()
            .rev()
            .find_map(|message| message.strip_prefix("user: "))
    }
}

fn looks_like_failed_request(message: &str) -> bool {
    let normalized = message.to_lowercase();
    [
        "could not create a safe analytics plan",
        "could not produce a query",
        "could not be completed safely",
        "database is currently unavailable",
        "local ai model is unavailable",
        "analytical query timed out",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

pub(crate) fn is_customer_record_request(question: &str) -> bool {
    let normalized = question.to_lowercase();
    normalized.contains("customer") && is_record_list_question(&normalized)
}

pub(crate) fn is_referential_follow_up(question: &str) -> bool {
    let normalized = format!(" {} ", question.to_lowercase());
    contains_any(
        &normalized,
        &[
            " that ",
            " those ",
            " them ",
            " it ",
            " same ",
            " above ",
            " previous ",
            " earlier ",
            " former ",
        ],
    )
}

pub(crate) fn redact_prompt_for_log(prompt: &str) -> String {
    let mut redacted = prompt.replace('\n', " ");
    if redacted.len() > 500 {
        redacted.truncate(500);
        redacted.push('…');
    }
    for marker in [
        "password", "otp", "token", "secret", "email", "phone", "address", "gateway",
    ] {
        if redacted.to_lowercase().contains(marker) {
            return "<redacted: prompt may contain sensitive data>".to_owned();
        }
    }
    redacted.replace('"', "\\\"")
}

fn is_record_list_question(question: &str) -> bool {
    let record_noun = contains_any(
        question,
        &[
            "orders",
            "order record",
            "order id",
            "customers",
            "customer record",
        ],
    );
    let list_verb = contains_any(
        question,
        &[
            "list ",
            "show ",
            "which ",
            "give me ",
            "display ",
            "individual ",
        ],
    );
    record_noun
        && list_verb
        && !contains_any(
            question,
            &[
                "how many",
                "count",
                "volume",
                "by status",
                "by day",
                "graph",
                "plot",
                "top ",
            ],
        )
}

fn contains_any(value: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| value.contains(term))
}

fn request_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("analytics-{millis}-{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_contained_request_drops_prior_analytics() {
        let messages = vec![
            ChatMessage::user("Show daily order volume for June 2026"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "June results".to_owned(),
            },
            ChatMessage::user("Compare May 1st and May 2nd 2026"),
        ];
        let context = AnalyticsRequestContext::from_messages(&messages).unwrap();
        assert!(context.relevant_conversation_context.is_empty());
        assert!(!context.isolated_prompt("plan").content.contains("June"));
    }

    #[test]
    fn referential_request_keeps_bounded_context() {
        let messages = vec![
            ChatMessage::user("Show order volume for June"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "June results".to_owned(),
            },
            ChatMessage::user("Compare that with May"),
        ];
        let context = AnalyticsRequestContext::from_messages(&messages).unwrap();
        assert_eq!(context.relevant_conversation_context.len(), 2);
    }

    #[test]
    fn referential_request_does_not_inherit_a_failed_request() {
        let messages = vec![
            ChatMessage::user("Show June orders"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "The local AI model could not create a safe analytics plan.".to_owned(),
            },
            ChatMessage::user("Try that again"),
        ];
        let context = AnalyticsRequestContext::from_messages(&messages).unwrap();
        assert!(context.relevant_conversation_context.is_empty());
    }

    #[test]
    fn classifies_required_shapes_deterministically() {
        assert_eq!(
            AnalyticalShape::classify("How many orders were placed in June?"),
            AnalyticalShape::Scalar
        );
        assert_eq!(
            AnalyticalShape::classify("Show daily order volume for June as a graph"),
            AnalyticalShape::TimeSeries
        );
        assert_eq!(
            AnalyticalShape::classify("Give me the order count by status"),
            AnalyticalShape::CategoryBreakdown
        );
        assert_eq!(
            AnalyticalShape::classify(
                "show me comparison between orders on may 1st 2026 and 2nd may 2026"
            ),
            AnalyticalShape::Comparison
        );
        assert_eq!(
            AnalyticalShape::classify("List orders placed yesterday"),
            AnalyticalShape::RecordList
        );
    }
}
