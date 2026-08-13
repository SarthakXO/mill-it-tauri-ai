use super::{
    types::SchemaColumnMetadata, Database, DatabaseError, DatabaseHealth, QueryParameter,
    QueryResult, ValidatedQuery,
};
use serde_json::{Map, Value};
use sqlx::{Postgres, Row, Transaction};
use std::time::Instant;

const MAX_RESULT_ROWS: usize = 500;
const MAX_RESULT_BYTES: usize = 1_000_000;
const MAX_DETERMINISTIC_ROWS: usize = 20_000;
const MAX_DETERMINISTIC_BYTES: usize = 8_000_000;

impl Database {
    pub(crate) async fn health(&self) -> Result<DatabaseHealth, DatabaseError> {
        let mut transaction = self.read_only_transaction().await?;
        let row = sqlx::query(
            r#"SELECT current_database()::text AS database,
                      current_setting('TimeZone')::text AS timezone,
                      (SELECT MAX(created_at)::text FROM "order") AS data_as_of"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        let health = DatabaseHealth {
            available: true,
            database: row.try_get("database").map_err(invalid_result)?,
            timezone: row.try_get("timezone").map_err(invalid_result)?,
            data_as_of: row.try_get("data_as_of").map_err(invalid_result)?,
        };
        transaction.rollback().await.map_err(map_sqlx_error)?;
        Ok(health)
    }

    pub(crate) async fn data_freshness(&self) -> Result<Option<String>, DatabaseError> {
        let mut transaction = self.read_only_transaction().await?;
        let value =
            sqlx::query_scalar::<_, Option<String>>(r#"SELECT MAX(created_at)::text FROM "order""#)
                .fetch_one(&mut *transaction)
                .await
                .map_err(map_sqlx_error)?;
        transaction.rollback().await.map_err(map_sqlx_error)?;
        Ok(value)
    }

    pub(crate) async fn schema_columns(
        &self,
        table_names: &[String],
    ) -> Result<Vec<SchemaColumnMetadata>, DatabaseError> {
        let mut transaction = self.read_only_transaction().await?;
        let rows = sqlx::query(
            r#"
            SELECT relation.relname::text AS table_name,
                   attribute.attname::text AS column_name,
                   pg_catalog.format_type(attribute.atttypid, attribute.atttypmod)::text AS data_type,
                   NOT attribute.attnotnull AS nullable,
                   primary_key.present AS primary_key,
                   foreign_key.foreign_table,
                   foreign_key.foreign_column
            FROM pg_catalog.pg_class AS relation
            JOIN pg_catalog.pg_namespace AS namespace
              ON namespace.oid = relation.relnamespace
            JOIN pg_catalog.pg_attribute AS attribute
              ON attribute.attrelid = relation.oid
             AND attribute.attnum > 0
             AND NOT attribute.attisdropped
            LEFT JOIN LATERAL (
                SELECT TRUE AS present
                FROM pg_catalog.pg_constraint AS constraint_record
                WHERE constraint_record.conrelid = relation.oid
                  AND constraint_record.contype = 'p'
                  AND attribute.attnum = ANY(constraint_record.conkey)
                LIMIT 1
            ) AS primary_key ON TRUE
            LEFT JOIN LATERAL (
                SELECT foreign_relation.relname::text AS foreign_table,
                       foreign_attribute.attname::text AS foreign_column
                FROM pg_catalog.pg_constraint AS constraint_record
                JOIN LATERAL unnest(constraint_record.conkey) WITH ORDINALITY
                  AS source_column(attnum, position)
                  ON source_column.attnum = attribute.attnum
                JOIN LATERAL unnest(constraint_record.confkey) WITH ORDINALITY
                  AS target_column(attnum, position)
                  ON target_column.position = source_column.position
                JOIN pg_catalog.pg_class AS foreign_relation
                  ON foreign_relation.oid = constraint_record.confrelid
                JOIN pg_catalog.pg_attribute AS foreign_attribute
                  ON foreign_attribute.attrelid = constraint_record.confrelid
                 AND foreign_attribute.attnum = target_column.attnum
                WHERE constraint_record.conrelid = relation.oid
                  AND constraint_record.contype = 'f'
                LIMIT 1
            ) AS foreign_key ON TRUE
            WHERE namespace.nspname = 'public'
              AND relation.relkind IN ('r', 'p')
              AND relation.relname = ANY($1)
            ORDER BY relation.relname, attribute.attnum
            "#,
        )
        .bind(table_names)
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;

        let columns = rows
            .into_iter()
            .map(|row| {
                Ok(SchemaColumnMetadata {
                    table_name: row.try_get("table_name").map_err(invalid_result)?,
                    column_name: row.try_get("column_name").map_err(invalid_result)?,
                    data_type: row.try_get("data_type").map_err(invalid_result)?,
                    nullable: row.try_get("nullable").map_err(invalid_result)?,
                    primary_key: row
                        .try_get::<Option<bool>, _>("primary_key")
                        .map_err(invalid_result)?
                        .unwrap_or(false),
                    foreign_table: row.try_get("foreign_table").map_err(invalid_result)?,
                    foreign_column: row.try_get("foreign_column").map_err(invalid_result)?,
                })
            })
            .collect::<Result<Vec<_>, DatabaseError>>()?;
        transaction.rollback().await.map_err(map_sqlx_error)?;
        Ok(columns)
    }

    pub(crate) async fn execute(
        &self,
        query: &ValidatedQuery,
        parameters: &[QueryParameter],
    ) -> Result<QueryResult, DatabaseError> {
        self.execute_with_limits(query, parameters, MAX_RESULT_ROWS, MAX_RESULT_BYTES)
            .await
    }

    pub(crate) async fn execute_deterministic(
        &self,
        query: &ValidatedQuery,
        parameters: &[QueryParameter],
    ) -> Result<QueryResult, DatabaseError> {
        self.execute_with_limits(
            query,
            parameters,
            MAX_DETERMINISTIC_ROWS,
            MAX_DETERMINISTIC_BYTES,
        )
        .await
    }

    async fn execute_with_limits(
        &self,
        query: &ValidatedQuery,
        parameters: &[QueryParameter],
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<QueryResult, DatabaseError> {
        if parameters.len() != query.parameter_count() {
            return Err(DatabaseError::InvalidParameters);
        }

        let started_at = Instant::now();
        let mut transaction = self.read_only_transaction().await?;
        let data_as_of =
            sqlx::query_scalar::<_, Option<String>>(r#"SELECT MAX(created_at)::text FROM "order""#)
                .fetch_one(&mut *transaction)
                .await
                .map_err(map_sqlx_error)?;

        let wrapped_sql = format!(
            "SELECT row_to_json(millit_result)::text FROM ({}) AS millit_result LIMIT {}",
            query.sql(),
            max_rows + 1
        );
        let mut database_query = sqlx::query_scalar::<_, String>(&wrapped_sql);
        for parameter in parameters {
            database_query = match parameter {
                QueryParameter::String(value) => database_query.bind(value),
                QueryParameter::Integer(value) => database_query.bind(value),
                QueryParameter::Float(value) => database_query.bind(value),
                QueryParameter::Boolean(value) => database_query.bind(value),
                QueryParameter::Null => database_query.bind(Option::<String>::None),
            };
        }

        let encoded_rows = database_query
            .fetch_all(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        transaction.rollback().await.map_err(map_sqlx_error)?;

        let mut bytes = 0usize;
        let mut rows = Vec::with_capacity(encoded_rows.len().min(max_rows));
        let mut truncated = encoded_rows.len() > max_rows;
        for encoded in encoded_rows.into_iter().take(max_rows) {
            bytes = bytes.saturating_add(encoded.len());
            if bytes > max_bytes {
                truncated = true;
                break;
            }
            let row = serde_json::from_str::<Map<String, Value>>(&encoded)
                .map_err(|error| DatabaseError::InvalidResult(error.to_string()))?;
            rows.push(row);
        }
        if rows.is_empty() && bytes > max_bytes {
            return Err(DatabaseError::ResultTooLarge);
        }

        let columns = rows
            .first()
            .map(|row| row.keys().cloned().collect())
            .unwrap_or_default();
        Ok(QueryResult {
            columns,
            row_count: rows.len(),
            rows,
            truncated,
            duration_ms: started_at.elapsed().as_millis(),
            data_as_of,
        })
    }

    async fn read_only_transaction(&self) -> Result<Transaction<'_, Postgres>, DatabaseError> {
        let mut transaction = self.pool()?.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query("SET LOCAL statement_timeout = '10s'")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        Ok(transaction)
    }
}

fn invalid_result(error: sqlx::Error) -> DatabaseError {
    DatabaseError::InvalidResult(error.to_string())
}

fn map_sqlx_error(error: sqlx::Error) -> DatabaseError {
    match &error {
        sqlx::Error::PoolTimedOut | sqlx::Error::Io(_) | sqlx::Error::Tls(_) => {
            DatabaseError::Unavailable
        }
        sqlx::Error::Database(database_error)
            if database_error.code().as_deref() == Some("57014") =>
        {
            DatabaseError::Timeout
        }
        _ => DatabaseError::QueryFailed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::validate_sql;

    #[test]
    #[ignore = "requires the configured PostgreSQL database"]
    fn live_database_health_schema_and_count() {
        tauri::async_runtime::block_on(async {
            let database = Database::from_environment().await;
            let health = database.health().await.expect("database health failed");
            assert!(health.available);
            println!(
                "Database health: database={:?}, timezone={:?}, data_as_of={:?}",
                health.database, health.timezone, health.data_as_of
            );

            let tables = [
                "order",
                "line_item",
                "product_variant",
                "product",
                "fulfillment",
                "payment",
                "refund_request",
                "partial_order_cancellation",
                "partial_order_cancellation_item",
            ]
            .map(str::to_owned);
            let schema = database
                .schema_columns(&tables)
                .await
                .expect("schema introspection failed");
            for table in &tables {
                let column_count = schema
                    .iter()
                    .filter(|column| &column.table_name == table)
                    .count();
                println!("Schema {table}: {column_count} columns discovered");
                if [
                    "order",
                    "line_item",
                    "payment",
                    "refund_request",
                    "fulfillment",
                    "partial_order_cancellation",
                    "partial_order_cancellation_item",
                ]
                .contains(&table.as_str())
                {
                    for column in schema.iter().filter(|column| &column.table_name == table) {
                        println!(
                            "  {} {} nullable={} foreign={:?}.{:?}",
                            column.column_name,
                            column.data_type,
                            column.nullable,
                            column.foreign_table,
                            column.foreign_column
                        );
                    }
                }
            }

            let query = validate_sql(r#"SELECT COUNT(id) AS order_count FROM "order""#)
                .expect("reference query should pass policy");
            let result = database
                .execute(&query, &[])
                .await
                .expect("reference count failed");
            println!(
                "Reference order count query returned {} row",
                result.row_count
            );
            assert_eq!(result.row_count, 1);

            let date_query = validate_sql(
                r#"SELECT COUNT(*) AS order_count
                   FROM "order"
                   WHERE timezone('Asia/Kolkata', created_at) >= '2026-08-10'::date
                     AND timezone('Asia/Kolkata', created_at) < '2026-08-11'::date"#,
            )
            .expect("date query should pass policy");
            let date_result = database
                .execute(&date_query, &[])
                .await
                .expect("reference date count failed");
            println!(
                "Reference August 10 IST query returned {} row",
                date_result.row_count
            );

            let product_query = validate_sql(
                r#"WITH freshness AS (
                       SELECT timezone('Asia/Kolkata', MAX(created_at))::date AS latest_day
                       FROM "order"
                   ), partial_cancelled AS (
                       SELECT line_item_id, SUM(cancelled_quantity) AS partial_cancelled_qty
                       FROM partial_order_cancellation_item
                       GROUP BY line_item_id
                   )
                   SELECT item.variant_id,
                          MAX(item.title) AS product_title,
                          SUM(GREATEST(item.quantity - COALESCE(partial_cancelled.partial_cancelled_qty, 0), 0)) AS units_sold
                   FROM line_item AS item
                   JOIN "order" AS orders ON orders.id = item.order_id
                   LEFT JOIN partial_cancelled ON partial_cancelled.line_item_id = item.id
                   CROSS JOIN freshness
                   WHERE NOT (
                           lower(orders.status::text) IN ('canceled', 'cancelled')
                           OR upper(COALESCE(orders.zoho_order_status, '')) IN ('CANCELED', 'CANCELLED')
                         )
                     AND timezone('Asia/Kolkata', item.created_at) >= freshness.latest_day - 29 * INTERVAL '1 day'
                     AND timezone('Asia/Kolkata', item.created_at) < freshness.latest_day + INTERVAL '1 day'
                   GROUP BY item.variant_id
                   ORDER BY units_sold DESC
                   LIMIT 5"#,
            )
            .expect("product query should pass policy");
            let product_result = database
                .execute(&product_query, &[])
                .await
                .expect("reference product ranking failed");
            println!(
                "Reference product query returned {} rows",
                product_result.row_count
            );
            assert_eq!(product_result.row_count, 5);
        });
    }
}
