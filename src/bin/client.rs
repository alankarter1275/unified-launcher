use slint::{ModelRc, VecModel, SharedString, Image, Model};
use serde::Deserialize;
use std::os::unix::net::UnixStream;
use std::io::{BufRead, BufReader, Write};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::env;
use std::path::Path;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

slint::include_modules!();

#[derive(Deserialize)]
struct AppInit { name: String, icon: Option<String> }
#[derive(Deserialize)]
struct InitPayload { apps: Vec<AppInit> }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = LauncherWindow::new()?;
    let matcher = Arc::new(SkimMatcherV2::default());

    let socket_path = env::var("UNIFIED_LAUNCHER_SOCKET")
        .unwrap_or_else(|_| "/tmp/unified_launcher.sock".to_string());

    let mut stream = UnixStream::connect(&socket_path)
        .unwrap_or_else(|_| panic!("Daemon is not running! Start 'unified-launcher daemon' first. (socket: {})", socket_path));

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

    let ui_handle_search = ui.as_weak();
    let matcher_search = Arc::clone(&matcher);
    let apps_search = Rc::clone(&apps_rc);

    ui.on_text_changed(move |query| {
        let ui = ui_handle_search.unwrap();
        let query_str = query.as_str();

        if query_str.trim().is_empty() {
            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from((*apps_search).clone()))));
            ui.set_absolute_index(0);
            ui.invoke_adjust_scroll();
            ui.set_ribbon_text(SharedString::from(format!("{} Apps", apps_search.len())));
        } else {
            let mut scored_apps = Vec::new();
            for app in apps_search.iter() {
                if let Some(score) = matcher_search.fuzzy_match(app.text.as_str(), query_str) {
                    scored_apps.push((score, app.clone()));
                }
            }
            scored_apps.sort_by(|a, b| b.0.cmp(&a.0));
            let total_count = scored_apps.len();
            let final_list: Vec<AppItem> = scored_apps.into_iter().take(100).map(|(_, v)| v).collect();

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

    let ui_handle_exec = ui.as_weak();
    ui.on_execute_selected(move |selected, _is_shift| {
        let _ = tx_exec.send(format!("EXEC_APP:{}", selected.as_str()));
        std::process::exit(0);
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
            1 => { let _ = std::process::Command::new("zen-browser").spawn(); }
            2 => { let _ = std::process::Command::new("sh")
                        .args(["-c", "mpv --player-operation-mode=pseudo-gui"]).spawn(); }
            3 => { let _ = std::process::Command::new("footclient")
                        .current_dir(&env::var("HOME").unwrap_or_default())
                        .args(["-e", "yazi"]).spawn(); }
            4 => { let _ = std::process::Command::new("footclient")
                        .args(["-e", "btop"]).spawn(); }
            5 => { let _ = std::process::Command::new("footclient")
                        .args(["-e", "nvim"]).spawn(); }
            _ => {}
        }
        std::process::exit(0);
    });

    let tx_power2 = tx_power.clone();
    ui.on_power_action(move |action| {
        let _ = tx_power2.send(format!("POWER_ACTION:{}", action.as_str()));
        std::process::exit(0);
    });

    // Escape key → close the launcher silently
    ui.on_close_requested(move || {
        std::process::exit(0);
    });

    ui.run()?;
    Ok(())
}
