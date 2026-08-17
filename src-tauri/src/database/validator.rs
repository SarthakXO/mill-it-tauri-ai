use sqlparser::{
    ast::{
        Expr, FunctionArguments, ObjectName, ObjectNamePart, Query, Select, SelectItem, SetExpr,
        Statement, TableFactor, Value, ValueWithSpan, Visit, VisitMut, Visitor, VisitorMut,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};
use std::{collections::HashSet, error::Error, fmt, ops::ControlFlow};

const MAX_SQL_LENGTH: usize = 20_000;

const ALLOWED_TABLES: &[&str] = &[
    "order",
    "line_item",
    "product_category",
    "product_category_product",
    "fulfillment",
    "payment",
    "customer",
    "product",
    "product_variant",
    "store",
    "refund_request",
    "partial_order_cancellation",
    "partial_order_cancellation_item",
    "order_lifecycle_log",
];

const ALLOWED_FUNCTIONS: &[&str] = &[
    "avg",
    "ceil",
    "coalesce",
    "count",
    "current_date",
    "current_timestamp",
    "date_part",
    "date_trunc",
    "dense_rank",
    "extract",
    "floor",
    "greatest",
    "lag",
    "lead",
    "least",
    "lower",
    "max",
    "min",
    "nullif",
    "percentile_cont",
    "percentile_disc",
    "rank",
    "round",
    "row_number",
    "sum",
    "timezone",
    "to_char",
    "upper",
];

const SENSITIVE_COLUMNS: &[&str] = &[
    "address",
    "customer_name",
    "customer_number",
    "data",
    "date_of_birth",
    "dob",
    "email",
    "first_name",
    "gateway_order_id",
    "gateway_payment_id",
    "last_name",
    "latitude",
    "longitude",
    "maps_link",
    "metadata",
    "mobile",
    "otp",
    "otp_code",
    "otp_expiry",
    "password",
    "password_hash",
    "payment_data",
    "payment_details",
    "pg_order_id",
    "pg_payment_id",
    "phone",
    "phone_number",
    "raw_route_payload",
    "raw_data",
    "route_payload",
    "secret",
    "token",
    "tracking_numbers",
    "zoho_invoice_id",
    "zoho_salesorder",
    "zoho_salesorder_id",
];

#[derive(Clone, Debug)]
pub(crate) struct ValidatedQuery {
    sql: String,
    parameter_count: usize,
    projection_count: usize,
}

impl ValidatedQuery {
    pub(crate) fn sql(&self) -> &str {
        &self.sql
    }

    pub(crate) const fn parameter_count(&self) -> usize {
        self.parameter_count
    }

    pub(crate) const fn projection_count(&self) -> usize {
        self.projection_count
    }

    #[cfg(debug_assertions)]
    pub(crate) fn redacted_sql_for_log(&self) -> String {
        let Ok(mut statements) = Parser::parse_sql(&PostgreSqlDialect {}, &self.sql) else {
            return "<approved SELECT omitted>".to_owned();
        };
        let _ = VisitMut::visit(&mut statements, &mut LiteralRedactor);
        statements
            .first()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<approved SELECT omitted>".to_owned())
    }
}

#[cfg(debug_assertions)]
struct LiteralRedactor;

#[cfg(debug_assertions)]
impl VisitorMut for LiteralRedactor {
    type Break = ();

    fn pre_visit_value(&mut self, value: &mut ValueWithSpan) -> ControlFlow<Self::Break> {
        if !matches!(
            &value.value,
            Value::Placeholder(_) | Value::Boolean(_) | Value::Null
        ) {
            value.value = Value::SingleQuotedString("<redacted>".to_owned());
        }
        ControlFlow::Continue(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SqlPolicyError {
    Empty,
    TooLong,
    Parse,
    MultipleStatements,
    ReadOnlyOnly,
    SelectInto,
    ExplicitLock,
    Wildcard,
    SensitiveColumn(String),
    ForbiddenFunction(String),
    ForbiddenRelation(String),
    InvalidPlaceholder,
    Unsupported(String),
}

impl SqlPolicyError {
    pub(crate) const fn user_message(&self) -> &'static str {
        match self {
            Self::SensitiveColumn(_) | Self::Wildcard => {
                "Mill-it Ops cannot retrieve sensitive customer or payment data."
            }
            Self::ReadOnlyOnly | Self::SelectInto | Self::ExplicitLock => {
                "Mill-it Ops only supports read-only analytics queries."
            }
            _ => "Mill-it Ops could not produce a query that passed the read-only safety policy.",
        }
    }
}

impl fmt::Display for SqlPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("SQL was empty"),
            Self::TooLong => formatter.write_str("SQL exceeded the length limit"),
            Self::Parse => formatter.write_str("SQL could not be parsed"),
            Self::MultipleStatements => formatter.write_str("multiple statements are forbidden"),
            Self::ReadOnlyOnly => formatter.write_str("only SELECT queries are allowed"),
            Self::SelectInto => formatter.write_str("SELECT INTO is forbidden"),
            Self::ExplicitLock => formatter.write_str("explicit row locks are forbidden"),
            Self::Wildcard => formatter.write_str("projection wildcards are forbidden"),
            Self::SensitiveColumn(column) => write!(formatter, "sensitive column: {column}"),
            Self::ForbiddenFunction(function) => {
                write!(formatter, "function is not allowed: {function}")
            }
            Self::ForbiddenRelation(relation) => {
                write!(formatter, "relation is not allowed: {relation}")
            }
            Self::InvalidPlaceholder => formatter.write_str("query placeholders were invalid"),
            Self::Unsupported(detail) => write!(formatter, "unsupported SQL feature: {detail}"),
        }
    }
}

impl Error for SqlPolicyError {}

pub(crate) fn validate_sql(sql: &str) -> Result<ValidatedQuery, SqlPolicyError> {
    validate_sql_with_sensitive_allowlist(sql, &[])
}

pub(crate) fn validate_sql_with_sensitive_allowlist(
    sql: &str,
    allowed_sensitive_columns: &[&str],
) -> Result<ValidatedQuery, SqlPolicyError> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(SqlPolicyError::Empty);
    }
    if sql.len() > MAX_SQL_LENGTH {
        return Err(SqlPolicyError::TooLong);
    }

    let mut statements =
        Parser::parse_sql(&PostgreSqlDialect {}, sql).map_err(|_| SqlPolicyError::Parse)?;
    if statements.len() != 1 {
        return Err(SqlPolicyError::MultipleStatements);
    }

    let _ = VisitMut::visit(&mut statements, &mut ReservedRelationQuoter);
    let Statement::Query(query) = &statements[0] else {
        return Err(SqlPolicyError::ReadOnlyOnly);
    };
    validate_set_expr(&query.body)?;

    let mut visitor = PolicyVisitor {
        allowed_sensitive_columns: allowed_sensitive_columns
            .iter()
            .map(|column| column.to_lowercase())
            .collect(),
        ..PolicyVisitor::default()
    };
    match query.visit(&mut visitor) {
        ControlFlow::Continue(()) => {}
        ControlFlow::Break(error) => return Err(error),
    }

    if visitor.parameter_indexes.iter().copied().max().unwrap_or(0)
        != visitor.parameter_indexes.len()
    {
        return Err(SqlPolicyError::InvalidPlaceholder);
    }

    Ok(ValidatedQuery {
        sql: query.to_string(),
        parameter_count: visitor.parameter_indexes.len(),
        projection_count: output_projection_count(&query.body),
    })
}

fn output_projection_count(expression: &SetExpr) -> usize {
    match expression {
        SetExpr::Select(select) => select.projection.len(),
        SetExpr::Query(query) => output_projection_count(&query.body),
        SetExpr::SetOperation { left, right, .. } => {
            output_projection_count(left).max(output_projection_count(right))
        }
        _ => 0,
    }
}

struct ReservedRelationQuoter;

impl VisitorMut for ReservedRelationQuoter {
    type Break = ();

    fn pre_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<Self::Break> {
        if let Some(ObjectNamePart::Identifier(identifier)) = relation.0.last_mut() {
            if identifier.value.eq_ignore_ascii_case("order") {
                identifier.quote_style = Some('"');
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<Self::Break> {
        if let Expr::CompoundIdentifier(identifiers) = expression {
            let qualifier_count = identifiers.len().saturating_sub(1);
            for identifier in identifiers.iter_mut().take(qualifier_count) {
                if identifier.value.eq_ignore_ascii_case("order") {
                    identifier.quote_style = Some('"');
                }
            }
        }
        ControlFlow::Continue(())
    }
}

fn validate_set_expr(expression: &SetExpr) -> Result<(), SqlPolicyError> {
    match expression {
        SetExpr::Select(_) => Ok(()),
        SetExpr::Query(query) => validate_set_expr(&query.body),
        SetExpr::SetOperation { left, right, .. } => {
            validate_set_expr(left)?;
            validate_set_expr(right)
        }
        SetExpr::Values(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_)
        | SetExpr::Table(_) => Err(SqlPolicyError::ReadOnlyOnly),
    }
}

#[derive(Default)]
struct PolicyVisitor {
    cte_names: HashSet<String>,
    parameter_indexes: HashSet<usize>,
    allowed_sensitive_columns: HashSet<String>,
}

impl Visitor for PolicyVisitor {
    type Break = SqlPolicyError;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if !query.locks.is_empty() {
            return ControlFlow::Break(SqlPolicyError::ExplicitLock);
        }
        if query.for_clause.is_some()
            || query.settings.is_some()
            || query.format_clause.is_some()
            || !query.pipe_operators.is_empty()
        {
            return ControlFlow::Break(SqlPolicyError::Unsupported(
                "non-PostgreSQL query clause".to_owned(),
            ));
        }
        if let Err(error) = validate_set_expr(&query.body) {
            return ControlFlow::Break(error);
        }
        if let Some(with) = &query.with {
            self.cte_names.extend(
                with.cte_tables
                    .iter()
                    .map(|cte| cte.alias.name.value.to_lowercase()),
            );
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if select.into.is_some() {
            return ControlFlow::Break(SqlPolicyError::SelectInto);
        }
        if select.projection.iter().any(|item| {
            matches!(
                item,
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _)
            )
        }) {
            return ControlFlow::Break(SqlPolicyError::Wildcard);
        }
        if select.prewhere.is_some()
            || !select.lateral_views.is_empty()
            || !select.connect_by.is_empty()
            || !select.cluster_by.is_empty()
            || !select.distribute_by.is_empty()
            || !select.sort_by.is_empty()
            || select.qualify.is_some()
            || select.value_table_mode.is_some()
        {
            return ControlFlow::Break(SqlPolicyError::Unsupported(
                "non-PostgreSQL SELECT clause".to_owned(),
            ));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, table: &TableFactor) -> ControlFlow<Self::Break> {
        match table {
            TableFactor::Table {
                args,
                with_hints,
                version,
                with_ordinality,
                partitions,
                json_path,
                sample,
                index_hints,
                ..
            } if args.is_none()
                && with_hints.is_empty()
                && version.is_none()
                && !with_ordinality
                && partitions.is_empty()
                && json_path.is_none()
                && sample.is_none()
                && index_hints.is_empty() =>
            {
                ControlFlow::Continue(())
            }
            TableFactor::Derived { lateral: false, .. } | TableFactor::NestedJoin { .. } => {
                ControlFlow::Continue(())
            }
            _ => ControlFlow::Break(SqlPolicyError::Unsupported(
                "table functions and lateral relations are forbidden".to_owned(),
            )),
        }
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        let identifiers = relation
            .0
            .iter()
            .filter_map(|part| part.as_ident())
            .map(|identifier| identifier.value.to_lowercase())
            .collect::<Vec<_>>();
        let allowed = match identifiers.as_slice() {
            [table] => ALLOWED_TABLES.contains(&table.as_str()) || self.cte_names.contains(table),
            [schema, table] => {
                schema == "public"
                    && (ALLOWED_TABLES.contains(&table.as_str()) || self.cte_names.contains(table))
            }
            _ => false,
        };
        if allowed {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(SqlPolicyError::ForbiddenRelation(relation.to_string()))
        }
    }

    fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
        match expression {
            Expr::Identifier(identifier) => {
                check_column(&identifier.value, &self.allowed_sensitive_columns)
            }
            Expr::CompoundIdentifier(identifiers) => identifiers
                .last()
                .map_or(ControlFlow::Continue(()), |identifier| {
                    check_column(&identifier.value, &self.allowed_sensitive_columns)
                }),
            Expr::Function(function) => {
                let identifiers = function
                    .name
                    .0
                    .iter()
                    .filter_map(|part| part.as_ident())
                    .collect::<Vec<_>>();
                let Some(name) = identifiers.first().filter(|_| identifiers.len() == 1) else {
                    return ControlFlow::Break(SqlPolicyError::ForbiddenFunction(
                        function.name.to_string(),
                    ));
                };
                let name = name.value.to_lowercase();
                if function.uses_odbc_syntax
                    || !matches!(function.parameters, FunctionArguments::None)
                    || !ALLOWED_FUNCTIONS.contains(&name.as_str())
                {
                    ControlFlow::Break(SqlPolicyError::ForbiddenFunction(name))
                } else {
                    ControlFlow::Continue(())
                }
            }
            Expr::JsonAccess { .. } | Expr::CompoundFieldAccess { .. } => {
                ControlFlow::Break(SqlPolicyError::Unsupported(
                    "JSON and compound-field access are forbidden".to_owned(),
                ))
            }
            _ => ControlFlow::Continue(()),
        }
    }

    fn pre_visit_value(&mut self, value: &ValueWithSpan) -> ControlFlow<Self::Break> {
        if let Value::Placeholder(placeholder) = &value.value {
            let Some(index) = placeholder
                .strip_prefix('$')
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|index| *index > 0 && *index <= 64)
            else {
                return ControlFlow::Break(SqlPolicyError::InvalidPlaceholder);
            };
            self.parameter_indexes.insert(index);
        }
        ControlFlow::Continue(())
    }
}

fn check_column(
    column: &str,
    allowed_sensitive_columns: &HashSet<String>,
) -> ControlFlow<SqlPolicyError> {
    let column = column.to_lowercase();
    if allowed_sensitive_columns.contains(&column) {
        return ControlFlow::Continue(());
    }
    if SENSITIVE_COLUMNS.contains(&column.as_str())
        || column.contains("password")
        || column.contains("otp")
        || column.contains("email")
        || column.contains("phone")
        || column.contains("address")
        || column.contains("latitude")
        || column.contains("longitude")
        || column.contains("birth")
        || column.contains("token")
        || column.contains("secret")
    {
        ControlFlow::Break(SqlPolicyError::SensitiveColumn(column))
    } else {
        ControlFlow::Continue(())
    }
}

pub(crate) fn is_sensitive_column(column: &str) -> bool {
    matches!(check_column(column, &HashSet::new()), ControlFlow::Break(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_read_only_selects() {
        assert!(validate_sql("SELECT COUNT(*) AS order_count FROM \"order\"").is_ok());
        assert!(validate_sql(
            "WITH daily AS (SELECT date_trunc('day', created_at) AS day, COUNT(*) AS orders FROM \"order\" GROUP BY 1) SELECT day, orders FROM daily ORDER BY day"
        )
        .is_ok());
    }

    #[test]
    fn safely_quotes_the_reserved_order_table() {
        let query =
            validate_sql("SELECT order.status FROM order").expect("SELECT should be accepted");
        assert!(query.sql().contains("FROM \"order\""));
        assert!(query.sql().contains("\"order\".status"));
    }

    #[test]
    fn rejects_mutations_and_ddl() {
        for sql in [
            "INSERT INTO customer (id) VALUES ('x')",
            "UPDATE customer SET first_name = 'x'",
            "DELETE FROM \"order\"",
            "MERGE INTO customer AS c USING (VALUES ('x')) AS v(id) ON c.id = v.id WHEN MATCHED THEN UPDATE SET first_name = 'x'",
            "DROP TABLE \"order\"",
            "ALTER TABLE \"order\" ADD COLUMN x int",
            "CREATE TABLE x (id int)",
            "TRUNCATE TABLE \"order\"",
            "COPY \"order\" TO '/tmp/orders.csv'",
            "SELECT id INTO archived_orders FROM \"order\"",
        ] {
            assert!(validate_sql(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn rejects_multiple_statements_locks_and_unsafe_functions() {
        for sql in [
            "SELECT id FROM \"order\"; DELETE FROM \"order\"",
            "SELECT id FROM \"order\" FOR UPDATE",
            "SELECT pg_read_file('/etc/passwd')",
            "SELECT nextval('order_seq')",
            "SELECT * FROM customer",
        ] {
            assert!(validate_sql(sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn placeholder_bound_supports_multi_period_plans_but_remains_finite() {
        assert!(validate_sql("SELECT $24 AS period_parameter FROM \"order\"").is_err());
        let contiguous = (1..=24)
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert!(validate_sql(&format!("SELECT {contiguous} FROM \"order\"")).is_ok());
        assert!(validate_sql("SELECT $65 FROM \"order\"").is_err());
    }

    #[test]
    fn rejects_sensitive_columns() {
        for column in ["password_hash", "otp", "email", "phone"] {
            let sql = format!("SELECT {column} FROM customer");
            assert!(validate_sql(&sql).is_err(), "should reject: {sql}");
        }
    }

    #[test]
    fn explicit_customer_projection_has_a_narrow_local_ui_exception() {
        let query = validate_sql_with_sensitive_allowlist(
            "SELECT c.id AS customer_id, c.first_name, c.phone FROM customer AS c",
            &["first_name", "phone"],
        );
        assert!(query.is_ok());
        assert!(validate_sql("SELECT c.id, c.first_name, c.phone FROM customer AS c").is_err());
        assert!(validate_sql_with_sensitive_allowlist(
            "SELECT c.password_hash FROM customer AS c",
            &["first_name", "phone"]
        )
        .is_err());
    }

    #[test]
    fn accepts_verified_product_category_relations() {
        validate_sql(
            r#"
        SELECT
            pcp.product_id,
            pc.id AS category_id,
            pc.parent_category_id
        FROM product_category_product AS pcp
        JOIN product_category AS pc
            ON pc.id = pcp.product_category_id
        "#,
        )
        .expect("verified product-category relations should be allowed for read-only analytics");
    }
}
