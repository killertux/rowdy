use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Exact-precision numeric (Postgres NUMERIC, MySQL DECIMAL). Stored as
    /// the canonical string form returned by the driver — keeps full
    /// precision without dragging the `bigdecimal` type into our public API.
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Timestamp(DateTime<Utc>),
    Date(NaiveDate),
    Time(NaiveTime),
    Uuid(Uuid),
    /// Driver-specific or unmapped type. Preserves the source type name so
    /// exporters can emit it correctly when possible.
    Other {
        type_name: String,
        repr: String,
    },
}

impl Cell {
    /// Compact, single-line rendering for the TUI grid. Not a serialization format.
    pub fn display(&self) -> String {
        match self {
            Self::Null => "NULL".into(),
            Self::Bool(v) => v.to_string(),
            Self::Int(v) => v.to_string(),
            Self::UInt(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Decimal(v) => v.clone(),
            Self::Text(v) => v.clone(),
            Self::Bytes(v) => format!("<{} bytes>", v.len()),
            Self::Timestamp(v) => v.to_rfc3339(),
            Self::Date(v) => v.to_string(),
            Self::Time(v) => v.to_string(),
            Self::Uuid(v) => v.to_string(),
            Self::Other { type_name, repr } => {
                if repr.is_empty() {
                    format!("<{type_name}>")
                } else {
                    repr.clone()
                }
            }
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}
