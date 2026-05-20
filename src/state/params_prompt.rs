//! State for the "fill in query parameters" popup that opens before
//! dispatching a query that contains `$N` / `:name` placeholders.

use ratatui::style::Style;
use ratatui_textarea::TextArea;

use crate::datasource::sql::placeholders::{ParamKey, Placeholder};

#[derive(Debug)]
pub struct ParamField {
    pub key: ParamKey,
    /// User-facing label including the original sigil — e.g. `$1`,
    /// `:name`. Pre-computed once so the renderer doesn't keep
    /// re-allocating.
    pub label: String,
    pub input: TextArea<'static>,
}

#[derive(Debug)]
pub struct ParamsPromptState {
    /// The original SQL with placeholders intact. Kept so submission
    /// can splice values back in without re-tokenizing.
    pub statement: String,
    /// The placeholder spans inside `statement`. Same vec the scanner
    /// returned — preserves byte ranges for substitution.
    pub placeholders: Vec<Placeholder>,
    pub fields: Vec<ParamField>,
    pub focus: usize,
}

impl ParamsPromptState {
    /// Build a popup for `statement`, one field per unique placeholder
    /// key in `keys`, pre-filled from `prefill` when a key matches.
    pub fn new(
        statement: String,
        placeholders: Vec<Placeholder>,
        keys: Vec<ParamKey>,
        prefill: impl Fn(&ParamKey) -> Option<String>,
    ) -> Self {
        let fields = keys
            .into_iter()
            .map(|key| {
                let label = key.label();
                let input = build_input(prefill(&key).as_deref().unwrap_or(""));
                ParamField { key, label, input }
            })
            .collect();
        Self {
            statement,
            placeholders,
            fields,
            focus: 0,
        }
    }

    pub fn current_input_mut(&mut self) -> Option<&mut TextArea<'static>> {
        self.fields.get_mut(self.focus).map(|f| &mut f.input)
    }

    pub fn cycle_focus(&mut self, forward: bool) {
        if self.fields.is_empty() {
            return;
        }
        let n = self.fields.len();
        self.focus = if forward {
            (self.focus + 1) % n
        } else {
            (self.focus + n - 1) % n
        };
    }

    /// Trimmed first-line value of the field at `idx`.
    pub fn field_value(&self, idx: usize) -> String {
        self.fields
            .get(idx)
            .and_then(|f| f.input.lines().first().cloned())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Collect all values into a `(key, value)` list ordered the same
    /// as `fields`. Empty values are kept verbatim so the caller can
    /// decide whether to block submit.
    pub fn collected(&self) -> Vec<(ParamKey, String)> {
        (0..self.fields.len())
            .map(|i| (self.fields[i].key.clone(), self.field_value(i)))
            .collect()
    }
}

fn build_input(seed: &str) -> TextArea<'static> {
    let mut input = if seed.is_empty() {
        TextArea::default()
    } else {
        TextArea::new(vec![seed.to_string()])
    };
    input.set_cursor_line_style(Style::default());
    input
}
