//! Dispatch for `:[range]s/pattern/replacement/flags`. The pure parsing and
//! application live in [`crate::substitute`]; this module wires the parsed
//! command to the live editor, status bar, and (for the `c` flag) the
//! interactive confirm overlay.

use edtui::{Highlight, Index2, Lines};

use crate::action::SubstituteConfirmAction;
use crate::app::App;
use crate::state::editor::{
    HighlightOwner, capture_undo, confirm_highlight_style, set_buffer_with_cursor,
};
use crate::state::overlay::Overlay;
use crate::state::status::QueryStatus;
use crate::state::substitute_confirm::ConfirmSubstituteState;
use crate::substitute::{
    SubstituteCmd, apply_substitute, build_regex, find_matches, resolve_range,
};

pub fn run(app: &mut App, cmd: SubstituteCmd) {
    // Resolve the pattern: an empty pattern (`:s//new/`) reuses the last
    // substitute pattern, falling back to the live `/` search pattern.
    let pattern = if cmd.pattern.is_empty() {
        let reuse = app.last_substitute_pattern.clone().or_else(|| {
            let p = app.editor.state.search_pattern();
            (!p.is_empty()).then(|| regex::escape(&p))
        });
        match reuse {
            Some(p) => p,
            None => return fail(app, "no previous pattern".to_string()),
        }
    } else {
        cmd.pattern.clone()
    };

    let re = match build_regex(&pattern, cmd.flags.ignore_case) {
        Ok(re) => re,
        Err(e) => return fail(app, e),
    };
    app.last_substitute_pattern = Some(pattern.clone());

    let last_row = app.editor.state.lines.len().saturating_sub(1);
    let cursor_row = app.editor.state.cursor.row;
    let selection_rows = app
        .editor
        .state
        .selection
        .as_ref()
        .map(|s| (s.start().row, s.end().row));
    let (start, end) = match resolve_range(&cmd.range, cursor_row, last_row, selection_rows) {
        Ok(rows) => rows,
        Err(e) => return fail(app, e),
    };

    let buffer = app.editor.text();
    let matches = find_matches(&buffer, (start, end), &re, cmd.flags.global);
    if matches.is_empty() {
        return fail(app, format!("Pattern not found: {pattern}"));
    }

    // One snapshot before any mutation so a single `u` unwinds the whole
    // substitution (including a full interactive session).
    capture_undo(&mut app.editor.state);

    if cmd.flags.confirm {
        let first = matches[0];
        let st = ConfirmSubstituteState::new(re, cmd.replacement, cmd.flags.global, end, first);
        render_current(app, &st);
        app.overlay = Some(Overlay::ConfirmSubstitute(st));
        return;
    }

    let outcome = apply_substitute(
        &buffer,
        (start, end),
        &re,
        &cmd.replacement,
        cmd.flags.global,
    );
    set_buffer_with_cursor(
        &mut app.editor.state,
        &outcome.text,
        outcome.last_changed_line,
        outcome.last_changed_col,
    );
    clear_highlights(app);
    app.status = QueryStatus::Notice {
        msg: substitution_message(outcome.substitutions, outcome.lines_changed),
    };
    super::schedule_session_save(app);
}

pub fn apply_confirm(app: &mut App, action: SubstituteConfirmAction) {
    let Some(Overlay::ConfirmSubstitute(mut st)) = app.overlay.take() else {
        return;
    };
    let mut buffer = app.editor.text();
    let mut finish = false;

    use SubstituteConfirmAction::*;
    match action {
        Yes => buffer = st.replace_current(&buffer),
        No => st.skip_current(),
        Last => {
            buffer = st.replace_current(&buffer);
            finish = true;
        }
        All => {
            loop {
                buffer = st.replace_current(&buffer);
                match st.find_next(&buffer) {
                    Some(m) => st.current = m,
                    None => break,
                }
            }
            finish = true;
        }
        Quit => finish = true,
    }

    if !finish {
        match st.find_next(&buffer) {
            Some(m) => st.current = m,
            None => finish = true,
        }
    }

    if finish {
        finalize_confirm(app, &st, &buffer);
    } else {
        app.editor.state.lines = Lines::from(buffer.as_str());
        app.editor.state.selection = None;
        render_current(app, &st);
        app.overlay = Some(Overlay::ConfirmSubstitute(st));
    }
}

fn finalize_confirm(app: &mut App, st: &ConfirmSubstituteState, buffer: &str) {
    let (row, col) = match st.last_changed_line {
        Some(r) => (r, st.last_changed_col),
        None => (app.editor.state.cursor.row, app.editor.state.cursor.col),
    };
    set_buffer_with_cursor(&mut app.editor.state, buffer, row, col);
    clear_highlights(app);
    app.status = QueryStatus::Notice {
        msg: substitution_message(st.substitutions, st.lines_changed),
    };
    super::schedule_session_save(app);
}

/// Highlight the current match and park the cursor on it so edtui scrolls it
/// into view.
fn render_current(app: &mut App, st: &ConfirmSubstituteState) {
    let style = confirm_highlight_style(app.theme.selection_bg, app.theme.fg);
    let m = st.current;
    let start = Index2::new(m.line, m.start_col);
    // edtui highlight ends are inclusive; a zero-width match highlights the
    // single cell at its start.
    let end_col = if m.end_col > m.start_col {
        m.end_col - 1
    } else {
        m.start_col
    };
    let end = Index2::new(m.line, end_col);
    app.editor.state.clear_highlights();
    app.editor
        .state
        .add_highlight(Highlight::new(start, end, style));
    app.editor.highlight_owner = Some(HighlightOwner::Substitute);
    app.editor.state.cursor = start;
}

fn clear_highlights(app: &mut App) {
    app.editor.state.clear_highlights();
    app.editor.highlight_owner = None;
}

fn substitution_message(substitutions: usize, lines: usize) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    format!(
        "{substitutions} substitution{} on {lines} line{}",
        plural(substitutions),
        plural(lines)
    )
}

fn fail(app: &mut App, error: String) {
    app.status = QueryStatus::Failed { error };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_singular_and_plural() {
        assert_eq!(substitution_message(1, 1), "1 substitution on 1 line");
        assert_eq!(substitution_message(3, 2), "3 substitutions on 2 lines");
    }
}
