//! Control-channel vocabulary: the commands a client sends to the running service.
//!
//! This module owns the command set and its argument parsing. The unix socket
//! transport that carries these commands is implemented separately.

pub const USAGE: &str = "\
Usage: quickbar [COMMAND]

Without a command, quickbar runs as the shell service.

Commands:
  toggle-bar    toggle the top status bar
  show-bar      show the top status bar
  hide-bar      hide the top status bar
  dnd on|off    enable or disable do-not-disturb
  reload        reload the config file
  quit          stop the running service
  -h, --help    show this help
";

/// A command a client can ask the service to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ToggleBar,
    ShowBar,
    HideBar,
    Dnd { enabled: bool },
    Reload,
    Quit,
}

impl Command {
    /// Parse the command-line arguments that follow the program name.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [cmd] => match cmd.as_str() {
                "toggle-bar" => Ok(Self::ToggleBar),
                "show-bar" => Ok(Self::ShowBar),
                "hide-bar" => Ok(Self::HideBar),
                "reload" => Ok(Self::Reload),
                "quit" => Ok(Self::Quit),
                other => Err(format!("unknown command: {other}")),
            },
            [cmd, value] if cmd == "dnd" => match value.as_str() {
                "on" => Ok(Self::Dnd { enabled: true }),
                "off" => Ok(Self::Dnd { enabled: false }),
                other => Err(format!("invalid value for `dnd`: {other} (expected `on` or `off`)")),
            },
            [cmd, ..] if cmd == "dnd" => Err("`dnd` takes exactly one argument: on|off".to_string()),
            [] => Err("missing command".to_string()),
            [cmd, ..] => Err(format!("`{cmd}` takes no arguments")),
        }
    }
}
