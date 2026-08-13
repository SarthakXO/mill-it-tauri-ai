use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SchemaRegistry {
    pub(crate) tables: Vec<PhysicalTable>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PhysicalTable {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) columns: Vec<PhysicalColumn>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PhysicalColumn {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) postgres_type: String,
    #[serde(default)]
    pub(crate) nullable: bool,
    #[serde(default)]
    pub(crate) primary_key: bool,
    #[serde(default)]
    pub(crate) references: Option<String>,
    pub(crate) sensitivity: Sensitivity,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Sensitivity {
    Safe,
    Internal,
    Pii,
    Secret,
    Denied,
}
