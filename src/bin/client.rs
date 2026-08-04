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
use unified_launcher::types::{json_line, ClientMessage, PowerAction, ServerMessage};

slint::include_modules!();

/// Try to spawn the daemon only when a connection cannot be established.
fn ensure_daemon_running(socket_path: &Path) -> bool {
    if UnixStream::connect(socket_path).is_ok() {
        return true;
    }

    let daemon_path = if let Ok(executable) = env::current_exe() {
        let mut daemon_path = executable;
        daemon_path.pop();
        daemon_path.push("daemon");
        daemon_path
    } else {
        eprintln!("[Client] Could not determine the daemon executable path.");
        return false;
    };

    eprintln!("[Client] Daemon is not reachable. Starting it...");
    let child = match std::process::Command::new(&daemon_path).spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("[Client] Could not start daemon: {error}");
            return false;
        }
    };

    for _ in 0..25 {
        if UnixStream::connect(socket_path).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }

    eprintln!(
        "[Client] Daemon started as pid {}, but the socket is still unavailable.",
        child.id()
    );
    false
}

fn send_request(writer: &Arc<Mutex<UnixStream>>, request: &ClientMessage) {
    let payload = match json_line(request) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("[Client] Could not encode daemon request: {error}");
            return;
        }
    };

    let mut stream = match writer.lock() {
        Ok(stream) => stream,
        Err(_) => {
            eprintln!("[Client] Daemon writer lock was poisoned.");
            return;
        }
    };
    if let Err(error) = stream
        .write_all(payload.as_bytes())
        .and_then(|()| stream.flush())
    {
        eprintln!("[Client] Could not send daemon request: {error}");
    }
}

fn copy_to_clipboard(text: &str) {
    let _ = std::process::Command::new("wl-copy")
        .arg(text)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = socket_path()?;
    if !ensure_daemon_running(&socket_path) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "daemon failed to become reachable",
        )
        .into());
    }

    let ui = LauncherWindow::new()?;
    let matcher = Arc::new(SkimMatcherV2::default());
    let stream = UnixStream::connect(&socket_path)?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut init_line = String::new();
    reader.read_line(&mut init_line)?;
    let initial_message: ServerMessage = serde_json::from_str(&init_line)?;
    let apps = match initial_message {
        ServerMessage::Init { apps } => apps,
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

    // A synchronous local socket write is tiny and prevents the launcher from
    // exiting before a background writer has delivered its request.
    let writer = Arc::new(Mutex::new(stream.try_clone()?));

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
            scored_apps.sort_by(|left, right| right.0.cmp(&left.0));
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

    // These fixed actions remain until Phase 2 replaces them with inline views.
    ui.on_sidebar_action(move |index| {
        match index {
            1 => {
                let _ = std::process::Command::new("zen-browser").spawn();
            }
            2 => {
                let _ = std::process::Command::new("sh")
                    .args(["-c", "mpv --player-operation-mode=pseudo-gui"])
                    .spawn();
            }
            3 => {
                let _ = std::process::Command::new("footclient")
                    .current_dir(env::var("HOME").unwrap_or_default())
                    .args(["-e", "yazi"])
                    .spawn();
            }
            4 => {
                let _ = std::process::Command::new("footclient")
                    .args(["-e", "btop"])
                    .spawn();
            }
            5 => {
                let _ = std::process::Command::new("footclient")
                    .args(["-e", "nvim"])
                    .spawn();
            }
            _ => {}
        }
        std::process::exit(0);
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
