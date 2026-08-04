//! Power / session actions (Sway-optimized).

use std::io;
use std::process::Command;

use crate::types::PowerAction;

/// Execute a validated power/session action.
pub fn handle_power_action(action: PowerAction) -> io::Result<()> {
    let (command, arguments): (&str, &[&str]) = match action {
        PowerAction::Lock => ("swaylock", &["-f", "-c", "000000"]),
        PowerAction::Logout => ("swaymsg", &["exit"]),
        PowerAction::Shutdown => ("systemctl", &["poweroff"]),
        PowerAction::Reboot => ("systemctl", &["reboot"]),
    };

    Command::new(command).args(arguments).spawn().map(|_| ())
}
