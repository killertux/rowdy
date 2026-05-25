use std::borrow::Cow;

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
    ///
    /// Returns a `Cow` so the common cases that already own a string
    /// (`Text`, `Decimal`, `Other { repr }`) borrow instead of cloning.
    /// The renderer calls this per visible cell on every frame; with
    /// large TEXT/JSON values the clone alone could allocate megabytes
    /// per redraw.
    pub fn display(&self) -> Cow<'_, str> {
        match self {
            Self::Null => Cow::Borrowed("NULL"),
            Self::Bool(v) => Cow::Owned(v.to_string()),
            Self::Int(v) => Cow::Owned(v.to_string()),
            Self::UInt(v) => Cow::Owned(v.to_string()),
            Self::Float(v) => Cow::Owned(v.to_string()),
            Self::Decimal(v) => Cow::Borrowed(v.as_str()),
            Self::Text(v) => Cow::Borrowed(v.as_str()),
            Self::Bytes(v) => Cow::Owned(format!("<{} bytes>", v.len())),
            Self::Timestamp(v) => Cow::Owned(v.to_rfc3339()),
            Self::Date(v) => Cow::Owned(v.to_string()),
            Self::Time(v) => Cow::Owned(v.to_string()),
            Self::Uuid(v) => Cow::Owned(v.to_string()),
            Self::Other { type_name, repr } => {
                if repr.is_empty() {
                    Cow::Owned(format!("<{type_name}>"))
                } else {
                    Cow::Borrowed(repr.as_str())
                }
            }
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}
