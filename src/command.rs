//! Pure parser for the `:` command line.
//!
//! [`parse`] turns a string into a [`Command`] without touching any
//! application state. The dispatcher in `action::submit_command` is
//! the only thing that touches `App`. Each parse error is the
//! verbatim message that ends up in the status bar — keeping that
//! text here lets us unit-test it.

use crate::export::ExportFormat;

/// One parsed `:` command. Variants mirror the user-visible vocabulary,
/// not the underlying `Action` enum — many commands feed into existing
/// `Action` variants but a few (`SetSchemaWidth`, `Conn`, …) drive
/// helpers in `action.rs` that have no `Action` representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Help,
    SetSchemaWidth(u16),
    Run,
    Cancel,
    Expand,
    Collapse,
    /// Hide the inline result preview without dropping history.
    /// `:close` / `:hide`. Bare `Q` does the same in Normal mode.
    CloseResult,
    Theme(ThemeChoice),
    Export {
        fmt: ExportFormat,
        target: ParsedTarget,
    },
    ExportSql {
        table: Option<String>,
        target: ParsedTarget,
    },
    Format(FormatScope),
    Reload,
    /// Re-read user + project config UI prefs, the user keybindings
    /// file, and the LLM provider records. Connections, crypto, the
    /// in-flight worker query, and the active session are NOT
    /// reloaded — those stay live across the call.
    Source,
    Conn(ConnSubcommand),
    Chat(ChatSubcommand),
    /// `:update` — manual check against the GitHub release API,
    /// independent of the 24h startup throttle and any prior
    /// dismissal. Newer release → standard "y/n" prompt; same
    /// version → "v0.7.x is the latest" notice; network failure →
    /// error in the bottom bar.
    Update,
}

/// `:chat` subcommands. Bare `:chat` toggles the right panel between
/// schema and chat; `:chat clear` wipes the message log; `:chat settings`
/// (phase 3) opens the provider/key configuration overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSubcommand {
    Toggle,
    Clear,
    Settings,
}

/// Which slice of the editor buffer `:format` should rewrite.
///
/// - `Cursor` (the bare `:format` / `:fmt`) — Visual selection if any,
///   otherwise the statement containing the cursor. Mirrors how `r`
///   picks what to run.
/// - `All` (`:format all`) — the entire buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatScope {
    Cursor,
    All,
}

/// `:theme` outcome. `Set` carries the theme file's stem (e.g. `"dark"`,
/// `"light"`, or any custom `themes/*.toml` name). The dispatcher
/// validates the name against the bundled registry; the parser stays
/// permissive so adding a new theme file is enough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeChoice {
    Toggle,
    Set(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnSubcommand {
    List,
    Add(Option<String>),
    Edit(String),
    Delete(String),
    Use(String),
}

/// Path target as parsed — `~` / `~/` is **not** expanded here so the
/// parser stays free of `$HOME` dependence. The dispatcher resolves
/// it before handing the path to the export helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedTarget {
    Clipboard,
    File(String),
}

/// Parse a single `:` line. `Ok(None)` is the empty-line case (treat
/// as no-op). `Err(msg)` is a user-facing error suitable for the
/// status bar.
pub fn parse(line: &str) -> Result<Option<Command>, String> {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return Ok(None);
    };
    let args: Vec<&str> = parts.collect();
    let parsed = match cmd {
        "q" | "quit" => Command::Quit,
        "help" | "?" => Command::Help,
        "width" => parse_width(&args)?,
        "run" | "r" => Command::Run,
        "cancel" => Command::Cancel,
        "expand" | "e" => Command::Expand,
        "collapse" | "c" => Command::Collapse,
        "close" | "hide" => Command::CloseResult,
        "theme" => parse_theme(&args)?,
        "export" => parse_export(&args)?,
        "format" | "fmt" => parse_format(&args)?,
        "reload" => Command::Reload,
        "source" => Command::Source,
        "conn" | "conns" => Command::Conn(parse_conn(&args)?),
        "chat" => Command::Chat(parse_chat(&args)?),
        "update" => Command::Update,
        _ => return Err(format!("unknown command: {cmd}")),
    };
    Ok(Some(parsed))
}

fn parse_format(args: &[&str]) -> Result<Command, String> {
    let scope = match args.first().copied() {
        None => FormatScope::Cursor,
        Some("all") => FormatScope::All,
        Some(other) => {
            return Err(format!(
                "unknown :format scope: {other} (use `all` or omit)"
            ));
        }
    };
    Ok(Command::Format(scope))
}

fn parse_width(args: &[&str]) -> Result<Command, String> {
    let v = args
        .first()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| "usage: :width <cols>".to_string())?;
    Ok(Command::SetSchemaWidth(v))
}

fn parse_theme(args: &[&str]) -> Result<Command, String> {
    let choice = match args.first().copied() {
        None | Some("toggle") => ThemeChoice::Toggle,
        Some(name) => ThemeChoice::Set(name.to_string()),
    };
    Ok(Command::Theme(choice))
}

fn parse_export(args: &[&str]) -> Result<Command, String> {
    let fmt_str = *args
        .first()
        .ok_or_else(|| "usage: :export <csv|tsv|json|sql> [args...]".to_string())?;
    let fmt = ExportFormat::parse(fmt_str)
        .ok_or_else(|| format!("unknown export format: {fmt_str} (use csv|tsv|json|sql)"))?;
    let rest = &args[1..];
    if matches!(fmt, ExportFormat::Sql) {
        return parse_export_sql(rest);
    }
    let target = parse_target(rest)?;
    Ok(Command::Export { fmt, target })
}

/// `:export sql [table] [path|> path]`. The first arg is the optional
/// table name unless it's the literal `>`, in which case it's already
/// the redirect marker and the table is left to inference.
fn parse_export_sql(args: &[&str]) -> Result<Command, String> {
    let (table, rest): (Option<String>, &[&str]) = match args.first().copied() {
        None => (None, args),
        Some(">") => (None, args),
        Some(name) => (Some(name.to_string()), &args[1..]),
    };
    let target = parse_target(rest)?;
    Ok(Command::ExportSql { table, target })
}

/// Parse the `[path]` / `> path` tail. Empty → clipboard; bare `>`
/// with no rest is an error; otherwise the joined remainder is the
/// path (we let spaces survive so `:export csv my file.csv` works).
fn parse_target(rest: &[&str]) -> Result<ParsedTarget, String> {
    Ok(match rest.first().copied() {
        None => ParsedTarget::Clipboard,
        Some(">") if rest.len() == 1 => return Err("missing path after `>`".into()),
        Some(">") => ParsedTarget::File(rest[1..].join(" ")),
        Some(_) => ParsedTarget::File(rest.join(" ")),
    })
}

fn parse_chat(args: &[&str]) -> Result<ChatSubcommand, String> {
    Ok(match args.first().copied() {
        None => ChatSubcommand::Toggle,
        Some("clear") => ChatSubcommand::Clear,
        Some("settings") | Some("config") => ChatSubcommand::Settings,
        Some(other) => {
            return Err(format!(
                "unknown :chat subcommand: {other} (use clear/settings or omit)"
            ));
        }
    })
}

fn parse_conn(args: &[&str]) -> Result<ConnSubcommand, String> {
    let sub = args.first().copied();
    // Connection names are allowed to contain spaces (the conn-form doesn't
    // forbid it), so the tail of the arg list is joined back together rather
    // than only taking the next token. Multiple internal spaces collapse to
    // one — round-tripping the exact whitespace through `:conn` isn't
    // supported.
    let rest_joined = || {
        let joined = args[1..].join(" ");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    };
    Ok(match sub {
        None | Some("list") | Some("ls") => ConnSubcommand::List,
        Some("add") => ConnSubcommand::Add(rest_joined()),
        Some("edit") => ConnSubcommand::Edit(
            rest_joined().ok_or_else(|| "usage: :conn edit <name>".to_string())?,
        ),
        Some("delete") | Some("rm") => ConnSubcommand::Delete(
            rest_joined().ok_or_else(|| "usage: :conn delete <name>".to_string())?,
        ),
        Some("use") => {
            ConnSubcommand::Use(rest_joined().ok_or_else(|| "usage: :conn use <name>".to_string())?)
        }
        Some(other) => {
            return Err(format!(
                "unknown :conn subcommand: {other} (use list/add/edit/delete/use)"
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_noop() {
        assert_eq!(parse(""), Ok(None));
        assert_eq!(parse("   "), Ok(None));
    }

    #[test]
    fn quit_aliases() {
        assert_eq!(parse("q"), Ok(Some(Command::Quit)));
        assert_eq!(parse("quit"), Ok(Some(Command::Quit)));
    }

    #[test]
    fn unknown_command_surfaces_message() {
        assert_eq!(parse("nope"), Err("unknown command: nope".into()));
    }

    #[test]
    fn close_and_hide_dismiss_result_preview() {
        assert_eq!(parse("close"), Ok(Some(Command::CloseResult)));
        assert_eq!(parse("hide"), Ok(Some(Command::CloseResult)));
    }

    #[test]
    fn update_command_parses() {
        assert_eq!(parse("update"), Ok(Some(Command::Update)));
        // Args after `:update` are ignored — manual check takes none.
        assert_eq!(parse("update extra"), Ok(Some(Command::Update)));
    }

    #[test]
    fn width_requires_unsigned_int() {
        assert_eq!(parse("width 32"), Ok(Some(Command::SetSchemaWidth(32))));
        assert_eq!(parse("width"), Err("usage: :width <cols>".into()));
        assert_eq!(parse("width junk"), Err("usage: :width <cols>".into()));
        assert_eq!(parse("width -1"), Err("usage: :width <cols>".into()));
    }

    #[test]
    fn theme_defaults_to_toggle() {
        assert_eq!(
            parse("theme"),
            Ok(Some(Command::Theme(ThemeChoice::Toggle)))
        );
        assert_eq!(
            parse("theme toggle"),
            Ok(Some(Command::Theme(ThemeChoice::Toggle)))
        );
        assert_eq!(
            parse("theme dark"),
            Ok(Some(Command::Theme(ThemeChoice::Set("dark".into()))))
        );
        assert_eq!(
            parse("theme light"),
            Ok(Some(Command::Theme(ThemeChoice::Set("light".into()))))
        );
        // Unknown names parse successfully — the dispatcher emits the
        // "unknown theme" status message after consulting the registry.
        assert_eq!(
            parse("theme neon"),
            Ok(Some(Command::Theme(ThemeChoice::Set("neon".into()))))
        );
    }

    #[test]
    fn export_clipboard() {
        assert_eq!(
            parse("export csv"),
            Ok(Some(Command::Export {
                fmt: ExportFormat::Csv,
                target: ParsedTarget::Clipboard,
            }))
        );
    }

    #[test]
    fn export_path_with_redirect() {
        assert_eq!(
            parse("export csv > out.csv"),
            Ok(Some(Command::Export {
                fmt: ExportFormat::Csv,
                target: ParsedTarget::File("out.csv".into()),
            }))
        );
    }

    #[test]
    fn export_path_without_redirect() {
        assert_eq!(
            parse("export json out.json"),
            Ok(Some(Command::Export {
                fmt: ExportFormat::Json,
                target: ParsedTarget::File("out.json".into()),
            }))
        );
    }

    #[test]
    fn export_path_with_spaces() {
        assert_eq!(
            parse("export csv my file.csv"),
            Ok(Some(Command::Export {
                fmt: ExportFormat::Csv,
                target: ParsedTarget::File("my file.csv".into()),
            }))
        );
    }

    #[test]
    fn export_redirect_without_path_errors() {
        assert_eq!(parse("export csv >"), Err("missing path after `>`".into()));
    }

    #[test]
    fn export_unknown_format() {
        assert!(matches!(parse("export xml"), Err(msg) if msg.contains("unknown export format")));
    }

    #[test]
    fn export_missing_format() {
        assert!(matches!(parse("export"), Err(msg) if msg.contains("usage:")));
    }

    #[test]
    fn export_sql_no_table_no_path() {
        assert_eq!(
            parse("export sql"),
            Ok(Some(Command::ExportSql {
                table: None,
                target: ParsedTarget::Clipboard,
            }))
        );
    }

    #[test]
    fn export_sql_with_table() {
        assert_eq!(
            parse("export sql users"),
            Ok(Some(Command::ExportSql {
                table: Some("users".into()),
                target: ParsedTarget::Clipboard,
            }))
        );
    }

    #[test]
    fn export_sql_with_table_and_path() {
        assert_eq!(
            parse("export sql users out.sql"),
            Ok(Some(Command::ExportSql {
                table: Some("users".into()),
                target: ParsedTarget::File("out.sql".into()),
            }))
        );
    }

    #[test]
    fn export_sql_no_table_redirect() {
        // `>` in the table slot means "no table, infer it" plus a redirect.
        assert_eq!(
            parse("export sql > out.sql"),
            Ok(Some(Command::ExportSql {
                table: None,
                target: ParsedTarget::File("out.sql".into()),
            }))
        );
    }

    #[test]
    fn conn_list_aliases() {
        assert_eq!(parse("conn"), Ok(Some(Command::Conn(ConnSubcommand::List))));
        assert_eq!(
            parse("conn list"),
            Ok(Some(Command::Conn(ConnSubcommand::List)))
        );
        assert_eq!(
            parse("conns ls"),
            Ok(Some(Command::Conn(ConnSubcommand::List)))
        );
    }

    #[test]
    fn conn_add_optional_name() {
        assert_eq!(
            parse("conn add"),
            Ok(Some(Command::Conn(ConnSubcommand::Add(None))))
        );
        assert_eq!(
            parse("conn add staging"),
            Ok(Some(Command::Conn(ConnSubcommand::Add(Some(
                "staging".into()
            )))))
        );
    }

    #[test]
    fn conn_use_requires_name() {
        assert_eq!(
            parse("conn use staging"),
            Ok(Some(Command::Conn(ConnSubcommand::Use("staging".into()))))
        );
        assert_eq!(parse("conn use"), Err("usage: :conn use <name>".into()));
    }

    #[test]
    fn format_defaults_to_cursor_scope() {
        assert_eq!(
            parse("format"),
            Ok(Some(Command::Format(FormatScope::Cursor)))
        );
        assert_eq!(parse("fmt"), Ok(Some(Command::Format(FormatScope::Cursor))));
    }

    #[test]
    fn format_all_scope() {
        assert_eq!(
            parse("format all"),
            Ok(Some(Command::Format(FormatScope::All)))
        );
        assert_eq!(
            parse("fmt all"),
            Ok(Some(Command::Format(FormatScope::All)))
        );
    }

    #[test]
    fn format_unknown_scope_errors() {
        assert!(matches!(
            parse("format buffer"),
            Err(msg) if msg.contains("unknown :format scope")
        ));
    }

    #[test]
    fn conn_subcommands_accept_names_with_spaces() {
        assert_eq!(
            parse("conn use staging server"),
            Ok(Some(Command::Conn(ConnSubcommand::Use(
                "staging server".into()
            ))))
        );
        assert_eq!(
            parse("conn edit my prod db"),
            Ok(Some(Command::Conn(ConnSubcommand::Edit(
                "my prod db".into()
            ))))
        );
        assert_eq!(
            parse("conn delete read replica"),
            Ok(Some(Command::Conn(ConnSubcommand::Delete(
                "read replica".into()
            ))))
        );
        assert_eq!(
            parse("conn add staging server"),
            Ok(Some(Command::Conn(ConnSubcommand::Add(Some(
                "staging server".into()
            )))))
        );
    }

    #[test]
    fn conn_unknown_subcommand() {
        assert!(matches!(
            parse("conn yikes"),
            Err(msg) if msg.contains("unknown :conn subcommand")
        ));
    }

    #[test]
    fn chat_bare_is_toggle() {
        assert_eq!(
            parse("chat"),
            Ok(Some(Command::Chat(ChatSubcommand::Toggle)))
        );
    }

    #[test]
    fn chat_subcommands() {
        assert_eq!(
            parse("chat clear"),
            Ok(Some(Command::Chat(ChatSubcommand::Clear)))
        );
        assert_eq!(
            parse("chat settings"),
            Ok(Some(Command::Chat(ChatSubcommand::Settings)))
        );
        assert_eq!(
            parse("chat config"),
            Ok(Some(Command::Chat(ChatSubcommand::Settings)))
        );
    }

    #[test]
    fn chat_unknown_subcommand() {
        assert!(matches!(
            parse("chat yikes"),
            Err(msg) if msg.contains("unknown :chat subcommand")
        ));
    }
}
