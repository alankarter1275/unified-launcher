//! Power / session actions (Sway-optimized).

/// Execute a power/session action.
pub fn handle_power_action(action: &str) {
    let (cmd, args) = match action {
        "lock" => ("swaylock", vec!["-f", "-c", "000000"] as Vec<&str>),
        "logout" => ("swaymsg", vec!["exit"]),
        "shutdown" => ("systemctl", vec!["poweroff"]),
        "reboot" => ("systemctl", vec!["reboot"]),
        _ => return,
    };
    let args_refs: Vec<&str> = args.iter().map(|s| *s).collect();
    let _ = std::process::Command::new(cmd).args(args_refs).spawn();
}
