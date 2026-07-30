use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use meval;
use serde::Deserialize;
use slint::{Image, ModelRc, SharedString, VecModel};

use unified_launcher::types::socket_path;

slint::include_modules!();

#[derive(Deserialize)]
struct AppInit {
    name: String,
    icon: Option<String>,
}
#[derive(Deserialize)]
struct InitPayload {
    apps: Vec<AppInit>,
}

/// Try to spawn the daemon if it's not running.
fn ensure_daemon_running(sock_path: &str) -> bool {
    if Path::new(sock_path).exists() {
        return true;
    }

    let daemon_path = if let Ok(exe) = env::current_exe() {
        let mut d = exe.clone();
        d.pop();
        d.push("daemon");
        d
    } else {
        eprintln!("[Client] Could not determine executable path.");
        return false;
    };

    eprintln!("[Client] Daemon not running. Starting it...");

    match std::process::Command::new(&daemon_path).spawn() {
        Ok(child) => {
            for _ in 0..20 {
                if Path::new(sock_path).exists() {
                    return true;
                }
                thread::sleep(Duration::from_millis(100));
            }
            eprintln!("[Client] Daemon started (pid {}), waiting...", child.id());
            thread::sleep(Duration::from_millis(500));
            Path::new(sock_path).exists()
        }
        Err(e) => {
            eprintln!("[Client] Could not start daemon: {}", e);
            false
        }
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
    let sock_path = socket_path();

    if !ensure_daemon_running(&sock_path) {
        eprintln!("[Client] Daemon failed to start.");
        std::process::exit(1);
    }

    let ui = LauncherWindow::new()?;
    let matcher = Arc::new(SkimMatcherV2::default());

    let stream = UnixStream::connect(&sock_path)
        .unwrap_or_else(|_| panic!("[Client] Could not connect to daemon at {}", sock_path));

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut init_line = String::new();
    reader.read_line(&mut init_line)?;

    let payload: InitPayload = serde_json::from_str(&init_line)?;
    let mut app_items = Vec::new();
    for app in payload.apps {
        let mut has_icon = false;
        let mut img = Image::default();
        if let Some(icon_path) = app.icon {
            if let Ok(loaded) = Image::load_from_path(Path::new(&icon_path)) {
                img = loaded;
                has_icon = true;
            }
        }
        app_items.push(AppItem {
            text: app.name.into(),
            has_native_icon: has_icon,
            native_icon: img,
            text_icon: "\u{f00e}".into(),
        });
    }

    let apps_rc = Rc::new(app_items);
    ui.set_display_items(ModelRc::from(Rc::new(VecModel::from((*apps_rc).clone()))));
    ui.set_ribbon_text(SharedString::from(format!("{} Apps", apps_rc.len())));

    let (tx, rx) = mpsc::channel::<String>();
    let tx_exec = tx.clone();
    let mut write_stream = stream.try_clone()?;

    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            let _ = write_stream.write_all(format!("{}\n", msg).as_bytes());
        }
    });

    // ---- Calculator mode state ----
    let calc_active = Arc::new(AtomicBool::new(false));
    let calc_active_search = calc_active.clone();
    let calc_active_exec = calc_active.clone();

    let ui_handle_search = ui.as_weak();
    let matcher_search = Arc::clone(&matcher);
    let apps_search = Rc::clone(&apps_rc);

    ui.on_text_changed(move |query| {
        let ui = ui_handle_search.unwrap();
        let query_str = query.as_str();
        let trimmed = query_str.trim();

        // ---- CALCULATOR MODE ----
        if trimmed.starts_with('=') {
            calc_active_search.store(true, Ordering::Relaxed);
            let expr = trimmed[1..].trim();

            if expr.is_empty() {
                ui.set_calc_mode(true);
                ui.set_calc_expression("".into());
                ui.set_calc_result("...".into());
                ui.set_calc_error(false);
                ui.set_calc_raw_result("".into());
                return;
            }

            let result = meval::eval_str(expr);
            match result {
                Ok(val) => {
                    let (display, raw) = if val.fract() == 0.0 && val.is_finite() {
                        (format!("{}", val as i64), format!("{}", val as i64))
                    } else if val.is_finite() {
                        let formatted = format!("{:.6}", val)
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                            .to_string();
                        (formatted.clone(), format!("{}", val))
                    } else if val.is_infinite() {
                        ("Infinity".to_string(), String::new())
                    } else {
                        ("undefined".to_string(), String::new())
                    };

                    ui.set_calc_mode(true);
                    ui.set_calc_expression(expr.into());
                    ui.set_calc_result(display.into());
                    ui.set_calc_error(false);
                    ui.set_calc_raw_result(raw.into());
                }
                Err(_e) => {
                    // Just show "..." — don't show scary error text.
                    // The expression might be incomplete (e.g. "= 2+"), which is normal.
                    ui.set_calc_mode(true);
                    ui.set_calc_expression(expr.into());
                    ui.set_calc_result("...".into());
                    ui.set_calc_error(false);
                    ui.set_calc_raw_result("".into());
                }
            }
            return;
        }

        // ---- NORMAL FUZZY SEARCH ----
        calc_active_search.store(false, Ordering::Relaxed);
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
            scored_apps.sort_by(|a, b| b.0.cmp(&a.0));
            let total_count = scored_apps.len();
            let final_list: Vec<AppItem> =
                scored_apps.into_iter().take(100).map(|(_, v)| v).collect();

            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from(final_list))));
            ui.set_absolute_index(0);
            ui.invoke_adjust_scroll();
            let lbl = if total_count == 1 {
                "1 Match".to_string()
            } else {
                format!("{} Matches", total_count)
            };
            ui.set_ribbon_text(SharedString::from(lbl));
        }
    });

    // Clone BEFORE tx_exec gets moved into the execute_selected closure
    let tx_power = tx_exec.clone();

    let ui_exec = ui.as_weak();
    ui.on_execute_selected(move |selected, _is_shift| {
        let ui = ui_exec.unwrap();

        // Calculator mode: copy result to clipboard
        if calc_active_exec.load(Ordering::Relaxed) {
            let raw = ui.get_calc_raw_result();
            if !raw.is_empty() {
                copy_to_clipboard(raw.as_str());
            }
            std::process::exit(0);
        }

        // Normal mode: execute app
        let _ = tx_exec.send(format!("EXEC_APP:{}", selected.as_str()));
        std::process::exit(0);
    });

    // Clear search button handler
    let ui_clear = ui.as_weak();
    let apps_clear = Rc::clone(&apps_rc);
    ui.on_clear_search(move || {
        let ui = ui_clear.unwrap();
        if ui.get_calc_mode() {
            // In calc mode: clear the expression but stay in calc mode
            ui.set_search_text("=".into());
            ui.set_calc_expression("".into());
            ui.set_calc_result("...".into());
            ui.set_calc_error(false);
            ui.set_calc_raw_result("".into());
        } else {
            // Normal mode: clear search and reset app list
            ui.set_search_text("".into());
            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from(
                (*apps_clear).clone(),
            ))));
            ui.set_absolute_index(0);
            ui.invoke_adjust_scroll();
            ui.set_ribbon_text(SharedString::from(format!("{} Apps", apps_clear.len())));
        }
    });

    let ui_handle_high = ui.as_weak();
    ui.on_item_highlighted(move |_idx, total, _name| {
        let ui = ui_handle_high.unwrap();
        let lbl = if total == 1 {
            "1 App".to_string()
        } else {
            format!("{} Apps", total)
        };
        ui.set_ribbon_text(SharedString::from(lbl));
    });

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
                    .current_dir(&env::var("HOME").unwrap_or_default())
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

    let tx_power2 = tx_power.clone();
    ui.on_power_action(move |action| {
        let _ = tx_power2.send(format!("POWER_ACTION:{}", action.as_str()));
        std::process::exit(0);
    });

    ui.on_close_requested(move || {
        std::process::exit(0);
    });

    ui.run()?;
    Ok(())
}
