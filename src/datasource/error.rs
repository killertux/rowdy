use std::fmt;

#[derive(Debug)]
pub enum DatasourceError {
    Connect(String),
    Execute(String),
    Introspect(String),
    Cancelled,
}

impl fmt::Display for DatasourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(msg) => write!(f, "connect: {msg}"),
            Self::Execute(msg) => write!(f, "execute: {msg}"),
            Self::Introspect(msg) => write!(f, "introspect: {msg}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for DatasourceError {}

pub type DatasourceResult<T> = std::result::Result<T, DatasourceError>;
