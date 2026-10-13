//! Shared SQL-literal rendering for `:export sql`.
//!
//! [`ansi`] is the portable baseline that [`Driver::format_literal`]
//! falls back to. Drivers override only the cells their dialect spells
//! differently (SQLite bools, Postgres bytea/uuid casts, …) and delegate
//! the rest here.
//!
//! [`Driver::format_literal`]: crate::datasource::Driver::format_literal

use std::fmt::Write as _;

use crate::datasource::Cell;

pub fn ansi(cell: &Cell) -> String {
    match cell {
        Cell::Null => "NULL".to_string(),
        Cell::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Cell::Int(v) => v.to_string(),
        Cell::UInt(v) => v.to_string(),
        Cell::Float(v) => {
            if v.is_finite() {
                format!("{v}")
            } else {
                // NaN / ±Inf can't appear in a portable SQL literal; fall
                // back to NULL so the INSERT at least loads.
                "NULL".to_string()
            }
        }
        Cell::Decimal(v) => v.clone(),
        Cell::Text(s) => quote_string(s),
        // Standard SQL-92 X'...' literal.
        Cell::Bytes(b) => format!("X'{}'", hex(b)),
        Cell::Timestamp(t) => quote_string(&t.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
        Cell::Date(d) => quote_string(&d.to_string()),
        Cell::Time(t) => quote_string(&t.to_string()),
        Cell::Uuid(u) => quote_string(&u.to_string()),
        // `repr` is empty when a driver fell all the way through its
        // decoder chain — we genuinely don't know the value, so NULL
        // is the only safe SQL emission. A populated `repr` (e.g. the
        // array literal synthesized by Postgres `format_array`) keeps
        // its previous best-effort string round-trip.
        Cell::Other { repr, .. } => {
            if repr.is_empty() {
                "NULL".to_string()
            } else {
                quote_string(repr)
            }
        }
    }
}

pub fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

pub fn hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(hex, "{:02x}", b);
    }
    hex
}
