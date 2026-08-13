use super::context::{ForeignKey, SchemaColumn, SchemaContext, SchemaTable};
use crate::database::{validator::is_sensitive_column, Database, DatabaseError};
use std::collections::BTreeMap;

pub(crate) async fn load_schema_context(
    database: &Database,
    table_names: &[String],
) -> Result<SchemaContext, DatabaseError> {
    load_schema_context_with_allowlist(database, table_names, &[]).await
}

pub(crate) async fn load_schema_context_with_allowlist(
    database: &Database,
    table_names: &[String],
    allowed_sensitive_columns: &[&str],
) -> Result<SchemaContext, DatabaseError> {
    let metadata = database.schema_columns(table_names).await?;
    let mut tables = BTreeMap::<String, Vec<SchemaColumn>>::new();

    for column in metadata {
        if (is_sensitive_column(&column.column_name)
            && !allowed_sensitive_columns.contains(&column.column_name.as_str()))
            || matches!(column.data_type.as_str(), "json" | "jsonb")
        {
            continue;
        }
        let references = column
            .foreign_table
            .zip(column.foreign_column)
            .map(|(table, column)| ForeignKey { table, column });
        tables
            .entry(column.table_name)
            .or_default()
            .push(SchemaColumn {
                name: column.column_name,
                postgres_type: column.data_type,
                nullable: column.nullable,
                primary_key: column.primary_key,
                references,
            });
    }

    Ok(SchemaContext {
        tables: tables
            .into_iter()
            .map(|(name, columns)| SchemaTable { name, columns })
            .collect(),
    })
}
