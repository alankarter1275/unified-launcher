use std::cell::RefCell;
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use slint::{Image, ModelRc, SharedString, VecModel};

use unified_launcher::paths::socket_path;
use unified_launcher::state::PIN_SLOT_COUNT;
use unified_launcher::types::{
    json_line, ClientMessage, PowerAction, PowerProfile, QuickSettingsAction,
    QuickSettingsSnapshot, ServerMessage,
};

slint::include_modules!();

type PinSlots = Rc<RefCell<Vec<Option<AppItem>>>>;

/// Connect to the daemon, starting it only when it is genuinely unavailable.
fn connect_or_start_daemon(socket_path: &Path) -> io::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket_path) {
        return Ok(stream);
    }

    let daemon_path = env::current_exe()
        .map(|mut executable| {
            executable.pop();
            executable.push("daemon");
            executable
        })
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("could not determine daemon executable path: {error}"),
            )
        })?;

    eprintln!("[Client] Daemon is not reachable. Starting it...");
    let child = std::process::Command::new(&daemon_path)
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not start daemon {}: {error}", daemon_path.display()),
            )
        })?;

    let mut last_error = None;
    for _ in 0..25 {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(100));
    }

    let reason = last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "unknown connection error".to_string());
    Err(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!(
            "daemon started as pid {}, but socket {} is unavailable: {reason}",
            child.id(),
            socket_path.display()
        ),
    ))
}

fn encode_request(request: &ClientMessage) -> io::Result<String> {
    json_line(request).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_request(writer: &Arc<Mutex<UnixStream>>, request: &ClientMessage) -> io::Result<()> {
    let payload = encode_request(request)?;
    let mut stream = writer.lock().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon writer lock was poisoned",
        )
    })?;
    stream.write_all(payload.as_bytes())?;
    stream.flush()
}

fn send_request(writer: &Arc<Mutex<UnixStream>>, request: &ClientMessage) {
    if let Err(error) = write_request(writer, request) {
        eprintln!("[Client] Could not send daemon request: {error}");
    }
}

fn request_response(
    writer: &Arc<Mutex<UnixStream>>,
    reader: &Arc<Mutex<BufReader<UnixStream>>>,
    request: &ClientMessage,
) -> Result<ServerMessage, String> {
    write_request(writer, request).map_err(|error| error.to_string())?;

    let mut reader = reader
        .lock()
        .map_err(|_| "daemon reader lock was poisoned".to_string())?;
    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if bytes_read == 0 {
        return Err("daemon closed the connection before acknowledging the request".to_string());
    }
    serde_json::from_str(&line).map_err(|error| error.to_string())
}

fn copy_to_clipboard(text: &str) {
    let _ = std::process::Command::new("wl-copy")
        .arg(text)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn resolve_pin_slots(pinned_app_ids: &[Option<String>], apps: &[AppItem]) -> Vec<Option<AppItem>> {
    (0..PIN_SLOT_COUNT)
        .map(|slot| {
            pinned_app_ids
                .get(slot)
                .and_then(Option::as_deref)
                .and_then(|app_id| {
                    apps.iter()
                        .find(|app| app.app_id.as_str() == app_id)
                        .cloned()
                })
        })
        .collect()
}

fn pin_item(slot: usize, app: Option<&AppItem>) -> PinItem {
    match app {
        Some(app) => PinItem {
            app_id: app.app_id.clone(),
            text: app.text.clone(),
            shortcut: format!("Alt+{}", slot + 1).into(),
            has_native_icon: app.has_native_icon,
            native_icon: app.native_icon.clone(),
            text_icon: app.text_icon.clone(),
        },
        None => PinItem {
            app_id: "".into(),
            text: "".into(),
            shortcut: format!("Alt+{}", slot + 1).into(),
            has_native_icon: false,
            native_icon: Image::default(),
            text_icon: "+".into(),
        },
    }
}

fn render_pins(ui: &LauncherWindow, pins: &[Option<AppItem>]) {
    let items: Vec<PinItem> = (0..PIN_SLOT_COUNT)
        .map(|slot| pin_item(slot, pins.get(slot).and_then(Option::as_ref)))
        .collect();
    ui.set_pinned_items(ModelRc::from(Rc::new(VecModel::from(items))));
}

fn show_toast(ui: &LauncherWindow, message: impl Into<SharedString>) {
    ui.set_toast_message(message.into());
}

fn switch_status(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "On",
        Some(false) => "Off",
        None => "Unavailable",
    }
}

fn apply_quick_settings(ui: &LauncherWindow, settings: &QuickSettingsSnapshot) {
    ui.set_wifi_enabled(settings.wifi_enabled.unwrap_or(false));
    ui.set_wifi_status(switch_status(settings.wifi_enabled).into());
    ui.set_bluetooth_enabled(settings.bluetooth_enabled.unwrap_or(false));
    ui.set_bluetooth_status(switch_status(settings.bluetooth_enabled).into());
    ui.set_idle_inhibited(settings.idle_inhibited);
    ui.set_sleep_inhibited(settings.sleep_inhibited);
    ui.set_power_profile(settings.power_profile.as_ui_value().into());
}

fn handle_quick_settings_response(ui: &LauncherWindow, response: Result<ServerMessage, String>) {
    match response {
        Ok(ServerMessage::ActionResult {
            message,
            quick_settings,
            ..
        }) => {
            if let Some(settings) = quick_settings {
                apply_quick_settings(ui, &settings);
            }
            show_toast(ui, message);
        }
        Ok(ServerMessage::Error { message }) => show_toast(ui, message),
        Ok(ServerMessage::Init { .. }) => {
            show_toast(ui, "Daemon returned an unexpected response");
        }
        Err(error) => show_toast(ui, format!("Quick Settings failed: {error}")),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = socket_path()?;
    let stream = connect_or_start_daemon(&socket_path)?;

    let ui = LauncherWindow::new()?;
    let matcher = Arc::new(SkimMatcherV2::default());

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut init_line = String::new();
    reader.read_line(&mut init_line)?;
    let initial_message: ServerMessage = serde_json::from_str(&init_line)?;
    let (apps, pinned_app_ids, quick_settings) = match initial_message {
        ServerMessage::Init {
            apps,
            pinned_app_ids,
            quick_settings,
        } => (apps, pinned_app_ids, quick_settings),
        ServerMessage::Error { message } => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, message).into());
        }
        ServerMessage::ActionResult { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "daemon sent an action result before initialization",
            )
            .into());
        }
    };

    let mut app_items = Vec::new();
    for app in apps {
        let mut has_icon = false;
        let mut image = Image::default();
        if let Some(icon_path) = app.icon {
            if let Ok(loaded) = Image::load_from_path(Path::new(&icon_path)) {
                image = loaded;
                has_icon = true;
            }
        }
        app_items.push(AppItem {
            app_id: app.id.into(),
            text: app.name.into(),
            has_native_icon: has_icon,
            native_icon: image,
            text_icon: "\u{f00e}".into(),
        });
    }

    let apps = Rc::new(app_items);
    ui.set_display_items(ModelRc::from(Rc::new(VecModel::from((*apps).clone()))));
    ui.set_ribbon_text(SharedString::from(format!("{} Apps", apps.len())));
    apply_quick_settings(&ui, &quick_settings);

    let pin_slots: PinSlots = Rc::new(RefCell::new(resolve_pin_slots(&pinned_app_ids, &apps)));
    render_pins(&ui, &pin_slots.borrow());

    // A synchronous local socket write is tiny and prevents the launcher from
    // exiting before a background writer has delivered its request.
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let reader = Arc::new(Mutex::new(reader));

    // ---- Calculator mode state ----
    let calculator_active = Arc::new(AtomicBool::new(false));
    let calculator_active_search = Arc::clone(&calculator_active);
    let calculator_active_execute = Arc::clone(&calculator_active);

    let ui_handle_search = ui.as_weak();
    let matcher_search = Arc::clone(&matcher);
    let apps_search = Rc::clone(&apps);

    ui.on_text_changed(move |query| {
        let ui = ui_handle_search.unwrap();
        let query = query.as_str();
        let trimmed = query.trim();

        // ---- CALCULATOR MODE (when search starts with =) ----
        if let Some(expression) = trimmed.strip_prefix('=') {
            calculator_active_search.store(true, Ordering::Relaxed);
            ui.set_calc_mode(true);
            let expression = expression.trim();

            if expression.is_empty() {
                ui.set_calc_expression("".into());
                ui.set_calc_result("...".into());
                ui.set_calc_error(false);
                ui.set_calc_raw_result("".into());
                return;
            }

            match meval::eval_str(expression) {
                Ok(value) => {
                    let (display, raw) = if value.fract() == 0.0 && value.is_finite() {
                        (format!("{}", value as i64), format!("{}", value as i64))
                    } else if value.is_finite() {
                        let formatted = format!("{value:.6}")
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                            .to_string();
                        (formatted.clone(), value.to_string())
                    } else if value.is_infinite() {
                        ("Infinity".to_string(), String::new())
                    } else {
                        ("undefined".to_string(), String::new())
                    };
                    ui.set_calc_expression(expression.into());
                    ui.set_calc_result(display.into());
                    ui.set_calc_error(false);
                    ui.set_calc_raw_result(raw.into());
                }
                Err(_) => {
                    ui.set_calc_expression(expression.into());
                    ui.set_calc_result("...".into());
                    ui.set_calc_error(false);
                    ui.set_calc_raw_result("".into());
                }
            }
            return;
        }

        // ---- NORMAL FUZZY SEARCH ----
        calculator_active_search.store(false, Ordering::Relaxed);
        ui.set_calc_mode(false);

        if trimmed.is_empty() {
            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from(
                (*apps_search).clone(),
            ))));
            ui.set_absolute_index(0);
            ui.invoke_adjust_scroll();
            ui.set_ribbon_text(SharedString::from(format!("{} Apps", apps_search.len())));
        } else {
            let mut scored_apps = Vec::new();
            for app in apps_search.iter() {
                if let Some(score) = matcher_search.fuzzy_match(app.text.as_str(), trimmed) {
                    scored_apps.push((score, app.clone()));
                }
            }
            scored_apps.sort_by_key(|item| std::cmp::Reverse(item.0));
            let total_count = scored_apps.len();
            let matches: Vec<AppItem> = scored_apps
                .into_iter()
                .take(100)
                .map(|(_, app)| app)
                .collect();

            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from(matches))));
            ui.set_absolute_index(0);
            ui.invoke_adjust_scroll();
            let label = if total_count == 1 {
                "1 Match".to_string()
            } else {
                format!("{total_count} Matches")
            };
            ui.set_ribbon_text(SharedString::from(label));
        }
    });

    let writer_execute = Arc::clone(&writer);
    let ui_execute = ui.as_weak();
    ui.on_execute_selected(move |selected, _is_shift| {
        let ui = ui_execute.unwrap();

        // Calculator mode: copy result to clipboard.
        if calculator_active_execute.load(Ordering::Relaxed) {
            let raw = ui.get_calc_raw_result();
            if !raw.is_empty() {
                copy_to_clipboard(raw.as_str());
            }
            std::process::exit(0);
        }

        send_request(
            &writer_execute,
            &ClientMessage::LaunchApp {
                app_id: selected.to_string(),
            },
        );
        std::process::exit(0);
    });

    let writer_pin = Arc::clone(&writer);
    let reader_pin = Arc::clone(&reader);
    let apps_pin = Rc::clone(&apps);
    let pin_slots_assign = Rc::clone(&pin_slots);
    let ui_pin = ui.as_weak();
    ui.on_pin_app_requested(move |app_id, slot| {
        let ui = ui_pin.unwrap();
        let slot = match usize::try_from(slot) {
            Ok(slot) if slot < PIN_SLOT_COUNT => slot,
            _ => {
                show_toast(&ui, "Invalid pin slot");
                return;
            }
        };
        let Some(app) = apps_pin
            .iter()
            .find(|app| app.app_id.as_str() == app_id.as_str())
            .cloned()
        else {
            show_toast(&ui, "That application is no longer available");
            return;
        };

        let response = request_response(
            &writer_pin,
            &reader_pin,
            &ClientMessage::SetAppPin {
                slot: slot as u8,
                app_id: app_id.to_string(),
            },
        );
        match response {
            Ok(ServerMessage::ActionResult { success: true, .. }) => {
                pin_slots_assign.borrow_mut()[slot] = Some(app.clone());
                render_pins(&ui, &pin_slots_assign.borrow());
                show_toast(&ui, format!("Pinned {} to Alt+{}", app.text, slot + 1));
            }
            Ok(ServerMessage::ActionResult {
                success: false,
                message,
                ..
            })
            | Ok(ServerMessage::Error { message }) => show_toast(&ui, message),
            Ok(ServerMessage::Init { .. }) => {
                show_toast(&ui, "Daemon returned an unexpected response");
            }
            Err(error) => show_toast(&ui, format!("Could not pin app: {error}")),
        }
    });

    let writer_launch_pin = Arc::clone(&writer);
    let pin_slots_launch = Rc::clone(&pin_slots);
    let ui_launch_pin = ui.as_weak();
    ui.on_launch_pinned_app(move |slot| {
        let ui = ui_launch_pin.unwrap();
        let slot = match usize::try_from(slot) {
            Ok(slot) if slot < PIN_SLOT_COUNT => slot,
            _ => return,
        };
        let app = pin_slots_launch
            .borrow()
            .get(slot)
            .and_then(Option::as_ref)
            .cloned();
        let Some(app) = app else {
            show_toast(&ui, format!("Alt+{} is empty", slot + 1));
            return;
        };

        send_request(
            &writer_launch_pin,
            &ClientMessage::LaunchApp {
                app_id: app.app_id.to_string(),
            },
        );
        std::process::exit(0);
    });

    let writer_wifi = Arc::clone(&writer);
    let reader_wifi = Arc::clone(&reader);
    let ui_wifi = ui.as_weak();
    ui.on_toggle_wifi(move || {
        let ui = ui_wifi.unwrap();
        let response = request_response(
            &writer_wifi,
            &reader_wifi,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::ToggleWifi,
            },
        );
        handle_quick_settings_response(&ui, response);
    });

    let writer_bluetooth = Arc::clone(&writer);
    let reader_bluetooth = Arc::clone(&reader);
    let ui_bluetooth = ui.as_weak();
    ui.on_toggle_bluetooth(move || {
        let ui = ui_bluetooth.unwrap();
        let response = request_response(
            &writer_bluetooth,
            &reader_bluetooth,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::ToggleBluetooth,
            },
        );
        handle_quick_settings_response(&ui, response);
    });

    let writer_idle = Arc::clone(&writer);
    let reader_idle = Arc::clone(&reader);
    let ui_idle = ui.as_weak();
    ui.on_toggle_idle_inhibit(move || {
        let ui = ui_idle.unwrap();
        let response = request_response(
            &writer_idle,
            &reader_idle,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::ToggleIdleInhibit,
            },
        );
        handle_quick_settings_response(&ui, response);
    });

    let writer_sleep = Arc::clone(&writer);
    let reader_sleep = Arc::clone(&reader);
    let ui_sleep = ui.as_weak();
    ui.on_toggle_sleep_inhibit(move || {
        let ui = ui_sleep.unwrap();
        let response = request_response(
            &writer_sleep,
            &reader_sleep,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::ToggleSleepInhibit,
            },
        );
        handle_quick_settings_response(&ui, response);
    });

    let writer_profile = Arc::clone(&writer);
    let reader_profile = Arc::clone(&reader);
    let ui_profile = ui.as_weak();
    ui.on_set_power_profile(move |profile| {
        let ui = ui_profile.unwrap();
        let Some(profile) = PowerProfile::parse(profile.as_str()) else {
            show_toast(&ui, "Unknown power profile");
            return;
        };
        let response = request_response(
            &writer_profile,
            &reader_profile,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::SetPowerProfile { profile },
            },
        );
        handle_quick_settings_response(&ui, response);
    });

    let writer_impala = Arc::clone(&writer);
    ui.on_open_wifi_manager(move || {
        send_request(
            &writer_impala,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::OpenWifiManager,
            },
        );
        std::process::exit(0);
    });

    let writer_bluetui = Arc::clone(&writer);
    ui.on_open_bluetooth_manager(move || {
        send_request(
            &writer_bluetui,
            &ClientMessage::QuickSettings {
                action: QuickSettingsAction::OpenBluetoothManager,
            },
        );
        std::process::exit(0);
    });

    // Clear search button handler.
    let ui_clear = ui.as_weak();
    let apps_clear = Rc::clone(&apps);
    ui.on_clear_search(move || {
        let ui = ui_clear.unwrap();
        if ui.get_calc_mode() {
            ui.set_search_text("=".into());
            ui.set_calc_expression("".into());
            ui.set_calc_result("...".into());
            ui.set_calc_error(false);
            ui.set_calc_raw_result("".into());
        } else {
            ui.set_search_text("".into());
            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from(
                (*apps_clear).clone(),
            ))));
            ui.set_absolute_index(0);
            ui.invoke_adjust_scroll();
            ui.set_ribbon_text(SharedString::from(format!("{} Apps", apps_clear.len())));
        }
    });

    let ui_handle_highlight = ui.as_weak();
    ui.on_item_highlighted(move |_index, total, _name| {
        let ui = ui_handle_highlight.unwrap();
        let label = if total == 1 {
            "1 App".to_string()
        } else {
            format!("{total} Apps")
        };
        ui.set_ribbon_text(SharedString::from(label));
    });

    let writer_power = Arc::clone(&writer);
    ui.on_power_action(move |action| {
        let Some(action) = PowerAction::parse(action.as_str()) else {
            eprintln!("[Client] Ignoring unknown power action: {action}");
            return;
        };
        send_request(&writer_power, &ClientMessage::PowerAction { action });
        std::process::exit(0);
    });

    ui.on_close_requested(move || {
        std::process::exit(0);
    });

    ui.run()?;
    Ok(())
}
