//! Sway-oriented Quick Settings integrations.
//!
//! These commands intentionally use direct, structured process invocations—no
//! shell interpolation. The defaults target the user's stated tools: iwd/Impala,
//! BlueZ/bluetui, Dunst, systemd, and TLP.

use std::process::{Command as StdCommand, Stdio};

use tokio::process::{Child, Command};

use crate::types::{PowerProfile, QuickSettingsAction, QuickSettingsSnapshot};

/// Handles for the daemon-owned inhibitor processes.
#[derive(Default)]
pub struct Inhibitors {
    idle: Option<Child>,
    sleep: Option<Child>,
}

fn output_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn error_text(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("command exited with status {}", output.status)
    } else {
        stderr
    }
}

async fn run_output(program: &str, arguments: &[&str]) -> Result<std::process::Output, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .await
        .map_err(|error| format!("could not start {program}: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(error_text(&output))
    }
}

/// Send a normal desktop notification. Dunst displays standard notifications.
fn notify(summary: &str, body: &str) {
    let notification = StdCommand::new("notify-send")
        .args(["-a", "Unified Launcher", summary, body])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    // Dunst installations commonly include dunstify; use it only as a fallback
    // when libnotify's notify-send is unavailable.
    if notification.is_err() {
        let _ = StdCommand::new("dunstify")
            .args(["-a", "Unified Launcher", summary, body])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn strip_ansi_sequences(value: &str) -> String {
    let mut clean = String::new();
    let mut characters = value.chars().peekable();

    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            clean.push(character);
            continue;
        }

        // Strip a CSI escape sequence such as `\x1b[1;32m`. This keeps the
        // parser reliable when iwctl formats a table for a terminal session.
        if characters.peek() == Some(&'[') {
            let _ = characters.next();
            for next in characters.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }

    clean
}

fn parse_iwd_device_list(output: &str) -> Option<(String, bool)> {
    output.lines().find_map(|line| {
        let clean_line = strip_ansi_sequences(line);
        let fields: Vec<_> = clean_line.split_whitespace().collect();
        let powered_index = fields
            .iter()
            .position(|field| matches!(*field, "on" | "off"))?;
        let device = fields[..powered_index]
            .iter()
            .find(|field| !matches!(**field, "*" | ">" | "-"))?;
        Some(((*device).to_string(), fields[powered_index] == "on"))
    })
}

async fn wifi_device_state() -> Result<(String, bool), String> {
    let output = run_output("iwctl", &["device", "list"]).await?;
    parse_iwd_device_list(&output_text(&output))
        .ok_or_else(|| "iwd did not report a Wi-Fi device".to_string())
}

async fn wifi_state() -> Option<bool> {
    wifi_device_state().await.ok().map(|(_, enabled)| enabled)
}

async fn bluetooth_state() -> Option<bool> {
    let output = run_output("bluetoothctl", &["show"]).await.ok()?;
    output_text(&output)
        .lines()
        .find_map(|line| line.trim().strip_prefix("Powered:"))
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        })
}

async fn power_profile() -> PowerProfile {
    let Ok(output) = run_output("tlp-stat", &["-s"]).await else {
        return PowerProfile::Unknown;
    };
    let status = output_text(&output);
    let Some(line) = status.lines().find(|line| line.contains("TLP profile")) else {
        return PowerProfile::Unknown;
    };
    let lower = line.to_ascii_lowercase();

    // TLP 1.10 shows `(manual)` after profiles explicitly chosen by the user.
    // Without it, `tlp start` is letting TLP follow the power source normally.
    if !lower.contains("(manual)") {
        return PowerProfile::Automatic;
    }
    if lower.contains("performance") {
        PowerProfile::Performance
    } else if lower.contains("power-saver") || lower.contains("power saver") {
        PowerProfile::PowerSaver
    } else if lower.contains("balanced") {
        PowerProfile::Balanced
    } else {
        PowerProfile::Unknown
    }
}

/// Query integrations once when the daemon starts. Missing optional tools are
/// represented as unknown rather than making the launcher unavailable.
pub async fn initial_snapshot() -> QuickSettingsSnapshot {
    QuickSettingsSnapshot {
        wifi_enabled: wifi_state().await,
        bluetooth_enabled: bluetooth_state().await,
        idle_inhibited: false,
        sleep_inhibited: false,
        power_profile: power_profile().await,
    }
}

async fn toggle_wifi(snapshot: &mut QuickSettingsSnapshot) -> Result<String, String> {
    let (device, current_state) = wifi_device_state().await?;
    let enabled = !current_state;
    let mode = if enabled { "on" } else { "off" };
    run_output(
        "iwctl",
        &["device", &device, "set-property", "Powered", mode],
    )
    .await?;
    snapshot.wifi_enabled = Some(enabled);

    let message = format!("Wi-Fi {}", if enabled { "enabled" } else { "disabled" });
    notify("Wi-Fi", &message);
    Ok(message)
}

async fn toggle_bluetooth(snapshot: &mut QuickSettingsSnapshot) -> Result<String, String> {
    let enabled = !snapshot.bluetooth_enabled.unwrap_or(false);
    let mode = if enabled { "on" } else { "off" };
    run_output("bluetoothctl", &["power", mode]).await?;
    snapshot.bluetooth_enabled = Some(enabled);

    let message = format!("Bluetooth {}", if enabled { "enabled" } else { "disabled" });
    notify("Bluetooth", &message);
    Ok(message)
}

async fn toggle_inhibitor(
    snapshot: &mut QuickSettingsSnapshot,
    inhibitors: &mut Inhibitors,
    idle: bool,
) -> Result<String, String> {
    let currently_enabled = if idle {
        snapshot.idle_inhibited
    } else {
        snapshot.sleep_inhibited
    };

    if currently_enabled {
        let child = if idle {
            inhibitors.idle.take()
        } else {
            inhibitors.sleep.take()
        };
        if let Some(mut child) = child {
            child
                .kill()
                .await
                .map_err(|error| format!("could not stop inhibitor: {error}"))?;
            let _ = child.wait().await;
        }
        if idle {
            snapshot.idle_inhibited = false;
        } else {
            snapshot.sleep_inhibited = false;
        }
    } else {
        let (what, reason) = if idle {
            ("idle", "Keep screen awake")
        } else {
            ("sleep", "Prevent automatic suspend")
        };
        let arguments = vec![
            format!("--what={what}"),
            "--mode=block".to_string(),
            "--who=Unified Launcher".to_string(),
            format!("--why={reason}"),
            "sleep".to_string(),
            "infinity".to_string(),
        ];
        let child = Command::new("systemd-inhibit")
            .args(&arguments)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not start inhibitor: {error}"))?;
        if idle {
            inhibitors.idle = Some(child);
            snapshot.idle_inhibited = true;
        } else {
            inhibitors.sleep = Some(child);
            snapshot.sleep_inhibited = true;
        }
    }

    let message = if idle {
        format!(
            "Keep screen awake {}",
            if snapshot.idle_inhibited {
                "enabled"
            } else {
                "disabled"
            }
        )
    } else {
        format!(
            "Suspend prevention {}",
            if snapshot.sleep_inhibited {
                "enabled"
            } else {
                "disabled"
            }
        )
    };
    notify("Quick Settings", &message);
    Ok(message)
}

async fn set_power_profile(
    snapshot: &mut QuickSettingsSnapshot,
    profile: PowerProfile,
) -> Result<String, String> {
    let arguments: &[&str] = match profile {
        PowerProfile::Automatic => &["tlp", "start"],
        PowerProfile::Performance => &["tlp", "performance"],
        PowerProfile::Balanced => &["tlp", "balanced"],
        PowerProfile::PowerSaver => &["tlp", "power-saver"],
        PowerProfile::Unknown => {
            return Err("Unknown is not a selectable power profile".to_string())
        }
    };
    run_output("pkexec", arguments).await?;
    snapshot.power_profile = profile;

    let message = format!("Power profile: {}", profile.label());
    notify("Power profile", &message);
    Ok(message)
}

fn open_manager(program: &str) -> Result<String, String> {
    StdCommand::new("footclient")
        .args(["-e", program])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| format!("Opening {program}"))
        .map_err(|error| format!("could not open {program}: {error}"))
}

/// Execute an action and return an updated snapshot for the UI.
///
/// Manager launches deliberately do not emit notifications. Every toggle and
/// power-profile change does, including failures.
pub async fn execute(
    action: QuickSettingsAction,
    snapshot: &mut QuickSettingsSnapshot,
    inhibitors: &mut Inhibitors,
) -> Result<String, String> {
    let notify_failures = !matches!(
        &action,
        QuickSettingsAction::OpenWifiManager | QuickSettingsAction::OpenBluetoothManager
    );
    let result = match action {
        QuickSettingsAction::ToggleWifi => toggle_wifi(snapshot).await,
        QuickSettingsAction::ToggleBluetooth => toggle_bluetooth(snapshot).await,
        QuickSettingsAction::ToggleIdleInhibit => {
            toggle_inhibitor(snapshot, inhibitors, true).await
        }
        QuickSettingsAction::ToggleSleepInhibit => {
            toggle_inhibitor(snapshot, inhibitors, false).await
        }
        QuickSettingsAction::SetPowerProfile { profile } => {
            set_power_profile(snapshot, profile).await
        }
        QuickSettingsAction::OpenWifiManager => open_manager("impala"),
        QuickSettingsAction::OpenBluetoothManager => open_manager("bluetui"),
    };

    if let Err(error) = &result {
        if notify_failures {
            notify("Quick Settings", error);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iwd_device_table() {
        let table = "\
                                    Devices
--------------------------------------------------------------------------------
  Name                  Address               Powered     Adapter     Mode
--------------------------------------------------------------------------------
  wlan0                 xx:xx:xx:xx:xx:xx     on          phy0        station
";
        assert_eq!(
            parse_iwd_device_list(table),
            Some(("wlan0".to_string(), true))
        );
    }

    #[test]
    fn parses_a_marked_iwd_device() {
        let table = "* wlan0 xx:xx:xx:xx:xx:xx off phy0 station";
        assert_eq!(
            parse_iwd_device_list(table),
            Some(("wlan0".to_string(), false))
        );
    }

    #[test]
    fn profile_labels_are_human_readable() {
        assert_eq!(PowerProfile::PowerSaver.label(), "Power saver");
        assert_eq!(
            PowerProfile::parse("power-saver"),
            Some(PowerProfile::PowerSaver)
        );
        assert_eq!(PowerProfile::parse("not-a-profile"), None);
    }
}
