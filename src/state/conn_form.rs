use ratatui::style::Style;
use ratatui_textarea::TextArea;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnFormField {
    Name,
    Url,
}

/// What to do once `ConnFormState` is successfully submitted. Lets the
/// first-launch flow auto-connect while `:conn add` / `:conn edit` just
/// save and bounce back to the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnFormPostSave {
    /// Dispatch a `Connect` for the saved entry — used on first launch.
    AutoConnect,
    /// Save and reopen the connection list.
    ReturnToList,
}

#[derive(Debug)]
pub struct ConnFormState {
    pub name: TextArea<'static>,
    pub url: TextArea<'static>,
    pub focus: ConnFormField,
    pub error: Option<String>,
    /// Set when editing an existing connection (the original name we'd
    /// overwrite). `None` for a fresh create.
    pub original: Option<String>,
    pub post_save: ConnFormPostSave,
}

impl ConnFormState {
    pub fn new_create() -> Self {
        Self {
            name: build_input("e.g. local, prod, staging"),
            url: build_input("e.g. sqlite:./sample.db"),
            focus: ConnFormField::Name,
            error: None,
            original: None,
            post_save: ConnFormPostSave::AutoConnect,
        }
    }

    pub fn editing(name: String, url: String) -> Self {
        Self {
            name: seeded(&name, "e.g. local, prod, staging"),
            url: seeded(&url, "e.g. sqlite:./sample.db"),
            focus: ConnFormField::Url,
            error: None,
            original: Some(name),
            post_save: ConnFormPostSave::AutoConnect,
        }
    }

    pub fn with_post_save(mut self, post: ConnFormPostSave) -> Self {
        self.post_save = post;
        self
    }

    /// Pre-fill the name field and move focus to the URL — used by
    /// `:conn add <name>` so the user lands on the URL slot.
    pub fn with_prefilled_name(mut self, name: &str) -> Self {
        self.name = seeded(name, "e.g. local, prod, staging");
        self.focus = ConnFormField::Url;
        self
    }

    pub fn current_input_mut(&mut self) -> &mut TextArea<'static> {
        match self.focus {
            ConnFormField::Name => &mut self.name,
            ConnFormField::Url => &mut self.url,
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            ConnFormField::Name => ConnFormField::Url,
            ConnFormField::Url => ConnFormField::Name,
        };
    }

    /// Trimmed first-line value of the name field.
    pub fn name_value(&self) -> String {
        self.name
            .lines()
            .first()
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Trimmed first-line value of the url field.
    pub fn url_value(&self) -> String {
        self.url
            .lines()
            .first()
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

fn build_input(placeholder: &'static str) -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text(placeholder);
    input.set_cursor_line_style(Style::default());
    input
}

fn seeded(value: &str, placeholder: &'static str) -> TextArea<'static> {
    let mut input = if value.is_empty() {
        TextArea::default()
    } else {
        TextArea::new(vec![value.to_string()])
    };
    input.set_placeholder_text(placeholder);
    input.set_cursor_line_style(Style::default());
    input
}
