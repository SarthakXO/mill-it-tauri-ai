use crate::{
    agent::AgentError,
    database::{
        validate_sql, validate_sql_with_sensitive_allowlist, Database, QueryParameter, QueryResult,
    },
};

pub(crate) const QUALIFYING_SALE_FILTER: &str = "NOT (lower(o.status::text) IN ('canceled', 'cancelled') OR upper(COALESCE(o.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED'))";
pub(crate) const PARTIAL_CANCEL_QUANTITY_JOIN: &str = " LEFT JOIN (SELECT line_item_id, SUM(cancelled_quantity)::bigint AS partial_cancelled_qty FROM partial_order_cancellation_item GROUP BY line_item_id) AS pcq ON pcq.line_item_id = li.id";
pub(crate) const NET_LINE_QUANTITY: &str =
    "GREATEST(li.quantity::bigint - COALESCE(pcq.partial_cancelled_qty, 0), 0)";

#[cfg(test)]
pub(crate) fn net_line_quantity(quantity: i64, partial_cancelled: i64) -> i64 {
    quantity.saturating_sub(partial_cancelled).max(0)
}

pub(crate) async fn execute(
    database: &Database,
    sql: &str,
    parameters: Vec<QueryParameter>,
) -> Result<QueryResult, AgentError> {
    let query = validate_sql(sql)?;
    database
        .execute_deterministic(&query, &parameters)
        .await
        .map_err(AgentError::from)
}

pub(crate) async fn execute_customer_records(
    database: &Database,
    sql: &str,
    parameters: Vec<QueryParameter>,
) -> Result<QueryResult, AgentError> {
    let query = validate_sql_with_sensitive_allowlist(sql, &["first_name", "phone"])?;
    database
        .execute_deterministic(&query, &parameters)
        .await
        .map_err(AgentError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_cancellation_reduces_quantity_without_going_negative() {
        assert_eq!(net_line_quantity(5, 2), 3);
        assert_eq!(net_line_quantity(2, 5), 0);
    }

    #[test]
    fn qualifying_predicate_uses_both_verified_status_sources() {
        assert!(QUALIFYING_SALE_FILTER.contains("o.status"));
        assert!(QUALIFYING_SALE_FILTER.contains("o.zoho_order_status"));
        assert!(!QUALIFYING_SALE_FILTER.contains("canceled_at"));
    }
}
