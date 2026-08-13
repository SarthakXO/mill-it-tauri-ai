use serde::Serialize;
use serde_json::{Map, Value};
use std::{error::Error, fmt};

#[derive(Clone, Debug)]
pub(crate) enum QueryParameter {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Null,
}

impl TryFrom<&Value> for QueryParameter {
    type Error = DatabaseError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(value) => Ok(Self::String(value.clone())),
            Value::Bool(value) => Ok(Self::Boolean(*value)),
            Value::Null => Ok(Self::Null),
            Value::Number(value) => {
                if let Some(integer) = value.as_i64() {
                    Ok(Self::Integer(integer))
                } else if let Some(unsigned) = value.as_u64() {
                    i64::try_from(unsigned)
                        .map(Self::Integer)
                        .map_err(|_| DatabaseError::InvalidParameters)
                } else if let Some(float) = value.as_f64() {
                    Ok(Self::Float(float))
                } else {
                    Err(DatabaseError::InvalidParameters)
                }
            }
            Value::Array(_) | Value::Object(_) => Err(DatabaseError::InvalidParameters),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct QueryResult {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Map<String, Value>>,
    pub(crate) row_count: usize,
    pub(crate) truncated: bool,
    pub(crate) duration_ms: u128,
    pub(crate) data_as_of: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DatabaseHealth {
    pub(crate) available: bool,
    pub(crate) database: Option<String>,
    pub(crate) timezone: Option<String>,
    pub(crate) data_as_of: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SchemaColumnMetadata {
    pub(crate) table_name: String,
    pub(crate) column_name: String,
    pub(crate) data_type: String,
    pub(crate) nullable: bool,
    pub(crate) primary_key: bool,
    pub(crate) foreign_table: Option<String>,
    pub(crate) foreign_column: Option<String>,
}

#[derive(Debug)]
pub(crate) enum DatabaseError {
    Configuration,
    Unavailable,
    Timeout,
    InvalidParameters,
    QueryFailed(String),
    InvalidResult(String),
    ResultTooLarge,
}

impl DatabaseError {
    pub(crate) const fn user_message(&self) -> &'static str {
        match self {
            Self::Configuration | Self::Unavailable => {
                "The analytics database is currently unavailable."
            }
            Self::Timeout => "The analytical query timed out. Try a narrower question.",
            Self::InvalidParameters => "The generated query parameters were invalid.",
            Self::QueryFailed(_) | Self::InvalidResult(_) => {
                "The analytical query could not be completed safely."
            }
            Self::ResultTooLarge => "The analytical result was too large. Try a narrower question.",
        }
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration => formatter.write_str("database is not configured"),
            Self::Unavailable => formatter.write_str("database is unavailable"),
            Self::Timeout => formatter.write_str("database query timed out"),
            Self::InvalidParameters => formatter.write_str("query parameters were invalid"),
            Self::QueryFailed(detail) => write!(formatter, "query failed: {detail}"),
            Self::InvalidResult(detail) => write!(formatter, "invalid result: {detail}"),
            Self::ResultTooLarge => formatter.write_str("query result exceeded the size limit"),
        }
    }
}

impl Error for DatabaseError {}
