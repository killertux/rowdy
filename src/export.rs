//! Pure formatters for the result-view export/yank flows. No I/O — callers
//! pipe the returned string into the system clipboard.

use std::fmt::Write as _;

use serde_json::{Map, Number, Value};

use crate::datasource::{Cell, Column};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Tsv,
    Json,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "tsv" => Some(Self::Tsv),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
        }
    }
}

/// Format a (columns, rows) rectangle in the chosen serialization. The
/// caller is responsible for picking the slice — this fn just renders.
pub fn format(format: ExportFormat, columns: &[&Column], rows: &[Vec<&Cell>]) -> String {
    match format {
        ExportFormat::Csv => to_csv(columns, rows),
        ExportFormat::Tsv => to_tsv(columns, rows),
        ExportFormat::Json => to_json(columns, rows),
    }
}

// ---------------------------------------------------------------------------
// CSV (RFC 4180): quote fields that contain commas, quotes, or line breaks;
// double internal quotes. NULL cells become empty fields.
// ---------------------------------------------------------------------------

fn to_csv(columns: &[&Column], rows: &[Vec<&Cell>]) -> String {
    let mut out = String::new();
    write_csv_row(&mut out, columns.iter().map(|c| c.name.as_str()));
    for row in rows {
        let fields: Vec<Cow<'_, str>> = row.iter().map(|c| display_or_empty(c)).collect();
        write_csv_row(&mut out, fields.iter().map(|f| f.as_ref()));
    }
    out
}

fn write_csv_row<'a, I: IntoIterator<Item = &'a str>>(out: &mut String, fields: I) {
    let mut first = true;
    for field in fields {
        if !first {
            out.push(',');
        }
        first = false;
        write_csv_field(out, field);
    }
    out.push('\n');
}

fn write_csv_field(out: &mut String, field: &str) {
    let needs_quote = field
        .chars()
        .any(|c| c == ',' || c == '"' || c == '\n' || c == '\r');
    if !needs_quote {
        out.push_str(field);
        return;
    }
    out.push('"');
    for ch in field.chars() {
        if ch == '"' {
            out.push_str("\"\"");
        } else {
            out.push(ch);
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// TSV: tabs separate fields, newlines separate rows. Cells get tabs/CRs/LFs
// replaced with spaces — lossy, but it preserves the table shape when pasted
// into a spreadsheet (which is the whole reason to pick TSV over CSV). Use
// CSV when you need to round-trip exactly.
// ---------------------------------------------------------------------------

fn to_tsv(columns: &[&Column], rows: &[Vec<&Cell>]) -> String {
    let mut out = String::new();
    write_tsv_record(&mut out, columns.iter().map(|c| c.name.as_str().to_string()));
    for row in rows {
        write_tsv_record(&mut out, row.iter().map(|c| display_or_empty(c).into_owned()));
    }
    out
}

fn write_tsv_record<I: IntoIterator<Item = String>>(out: &mut String, fields: I) {
    let mut first = true;
    for field in fields {
        if !first {
            out.push('\t');
        }
        first = false;
        out.push_str(&tsv_sanitize(&field));
    }
    out.push('\n');
}

fn tsv_sanitize(field: &str) -> String {
    field
        .chars()
        .map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c })
        .collect()
}

// ---------------------------------------------------------------------------
// JSON: an array of objects keyed by column name. Numeric/bool/null cells
// become native JSON values; everything else is a string. Bytes render as
// hex (`"0xdeadbeef"`).
// ---------------------------------------------------------------------------

fn to_json(columns: &[&Column], rows: &[Vec<&Cell>]) -> String {
    let array: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut obj = Map::with_capacity(columns.len());
            for (col, cell) in columns.iter().zip(row.iter()) {
                obj.insert(col.name.clone(), cell_to_json(cell));
            }
            Value::Object(obj)
        })
        .collect();
    let mut out = serde_json::to_string_pretty(&Value::Array(array))
        .unwrap_or_else(|_| "[]".to_string());
    out.push('\n');
    out
}

fn cell_to_json(cell: &Cell) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Bool(v) => Value::Bool(*v),
        Cell::Int(v) => Value::Number(Number::from(*v)),
        Cell::UInt(v) => Value::Number(Number::from(*v)),
        // f64 NaN/Inf can't be represented in JSON; fall back to null rather
        // than producing invalid output or panicking.
        Cell::Float(v) => Number::from_f64(*v).map(Value::Number).unwrap_or(Value::Null),
        Cell::Text(v) => Value::String(v.clone()),
        Cell::Bytes(v) => Value::String(bytes_to_hex(v)),
        Cell::Timestamp(v) => Value::String(v.to_rfc3339()),
        Cell::Date(v) => Value::String(v.to_string()),
        Cell::Time(v) => Value::String(v.to_string()),
        Cell::Uuid(v) => Value::String(v.to_string()),
        Cell::Other { type_name, repr } => {
            if repr.is_empty() {
                Value::String(format!("<{type_name}>"))
            } else {
                Value::String(repr.clone())
            }
        }
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

// ---------------------------------------------------------------------------
// Cell → string used by CSV/TSV. Borrows when the cell already owns a
// string of its own; allocates only when we have to format.
// ---------------------------------------------------------------------------

use std::borrow::Cow;

fn display_or_empty(cell: &Cell) -> Cow<'_, str> {
    match cell {
        Cell::Null => Cow::Borrowed(""),
        Cell::Text(s) => Cow::Borrowed(s.as_str()),
        Cell::Bytes(v) => Cow::Owned(bytes_to_hex(v)),
        other => Cow::Owned(other.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasource::Column;

    fn col(name: &str) -> Column {
        Column { name: name.into() }
    }

    fn columns_borrowed(cols: &[Column]) -> Vec<&Column> {
        cols.iter().collect()
    }

    fn row_borrowed(cells: &[Cell]) -> Vec<&Cell> {
        cells.iter().collect()
    }

    #[test]
    fn csv_quotes_fields_with_specials() {
        let cols = [col("name"), col("note")];
        let row = [Cell::Text("Doe, John".into()), Cell::Text("said \"hi\"\nthen left".into())];
        let csv = to_csv(&columns_borrowed(&cols), &[row_borrowed(&row)]);
        assert_eq!(
            csv,
            "name,note\n\"Doe, John\",\"said \"\"hi\"\"\nthen left\"\n"
        );
    }

    #[test]
    fn csv_handles_nulls_and_numbers() {
        let cols = [col("a"), col("b")];
        let row = [Cell::Null, Cell::Int(42)];
        let csv = to_csv(&columns_borrowed(&cols), &[row_borrowed(&row)]);
        assert_eq!(csv, "a,b\n,42\n");
    }

    #[test]
    fn tsv_replaces_control_chars_with_spaces() {
        let cols = [col("a")];
        let row = [Cell::Text("x\ty\nz".into())];
        let tsv = to_tsv(&columns_borrowed(&cols), &[row_borrowed(&row)]);
        assert_eq!(tsv, "a\nx y z\n");
    }

    #[test]
    fn json_preserves_native_types() {
        let cols = [col("i"), col("b"), col("n"), col("s")];
        let row = [
            Cell::Int(7),
            Cell::Bool(true),
            Cell::Null,
            Cell::Text("hi".into()),
        ];
        let json = to_json(&columns_borrowed(&cols), &[row_borrowed(&row)]);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let obj = arr[0].as_object().unwrap();
        assert_eq!(obj["i"], Value::Number(7.into()));
        assert_eq!(obj["b"], Value::Bool(true));
        assert_eq!(obj["n"], Value::Null);
        assert_eq!(obj["s"], Value::String("hi".into()));
    }

    #[test]
    fn json_renders_bytes_as_hex_string() {
        let cols = [col("b")];
        let row = [Cell::Bytes(vec![0xde, 0xad, 0xbe, 0xef])];
        let json = to_json(&columns_borrowed(&cols), &[row_borrowed(&row)]);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["b"], Value::String("0xdeadbeef".into()));
    }

    #[test]
    fn json_falls_back_to_null_for_nan_and_inf() {
        let cols = [col("f")];
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let row = [Cell::Float(v)];
            let json = to_json(&columns_borrowed(&cols), &[row_borrowed(&row)]);
            let parsed: Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed[0]["f"], Value::Null);
        }
    }

    #[test]
    fn parse_format_is_case_insensitive() {
        assert_eq!(ExportFormat::parse("csv"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::parse("TSV"), Some(ExportFormat::Tsv));
        assert_eq!(ExportFormat::parse("Json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::parse("xml"), None);
    }
}
