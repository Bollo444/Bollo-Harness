//! Slash commands. Names are Bollo contracts (docs/reference/cli.md), not
//! promised upstream aliases. Commands that need composition-wide state (policy,
//! sessions, MCP, doctor, checkpoints) are delegated to a [`SlashCommands`]
//! implementation provided by the composition root; the client only owns the
//! commands that are purely about the transcript in front of it.

/// Parse `/name argument` into its parts. Returns `None` for non-commands.
pub fn parse(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('/')?;
    let mut parts = rest.trim().splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return None;
    }
    Some((name, parts.next().unwrap_or("").trim()))
}

/// Commands owned by the client itself.
pub fn is_client_command(name: &str) -> bool {
    matches!(name, "help" | "quit" | "exit" | "transcript")
}

/// Delegated commands (policy/sessions/MCP/doctor/model/compaction/checkpoints).
pub trait SlashCommands {
    /// Return the lines to print, or `None` when the command is unknown here.
    fn handle(&mut self, name: &str, argument: &str) -> Option<Vec<String>>;
}

pub fn help(extra: &[&str]) -> Vec<String> {
    let mut lines = vec![
        "client commands:".to_string(),
        "  /help              this list".to_string(),
        "  /transcript        replay what this session has shown".to_string(),
        "  /quit              leave the client".to_string(),
    ];
    if !extra.is_empty() {
        lines.push("composition commands:".to_string());
        for command in extra {
            lines.push(format!("  {command}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_arguments() {
        assert_eq!(parse("/diff"), Some(("diff", "")));
        assert_eq!(parse("/policy show"), Some(("policy", "show")));
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("/"), None);
    }

    #[test]
    fn client_commands_are_recognized() {
        assert!(is_client_command("help"));
        assert!(is_client_command("quit"));
        assert!(!is_client_command("policy"));
    }

    #[test]
    fn help_lists_available_composition_commands() {
        let lines = help(&["/policy show", "/sessions list"]);
        assert!(lines.iter().any(|line| line.contains("/transcript")));
        assert!(lines.iter().any(|line| line.trim() == "/policy show"));
    }
}
