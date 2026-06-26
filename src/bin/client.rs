#![allow(warnings)]

use slint::{ModelRc, VecModel, SharedString, Image};
use serde::{Deserialize, Serialize};
use std::os::unix::net::UnixStream;
use std::io::{BufRead, BufReader, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::env;
use std::path::Path;
use std::fs;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

slint::include_modules!();

#[derive(Clone, Debug, Deserialize)]
struct FileEntry { name: String, is_dir: bool, path: String } 

#[derive(Deserialize)]
struct AppInit { name: String, icon: Option<String> }

// STRIPPED: We no longer expect or use `scripts` from the daemon payload
#[derive(Deserialize)]
struct InitPayload {
    apps: Vec<AppInit>,
}

#[derive(Deserialize)]
struct DirResponse {
    current_path: String,
    contents: Vec<FileEntry>,
}

struct AppState {
    current_dir_path: String,
    current_dir_contents: Vec<FileEntry>,
}

fn find_icon(name: &str) -> Option<Image> {
    if name.starts_with('/') && Path::new(name).exists() {
        return Image::load_from_path(Path::new(name)).ok();
    }
    
    let exts = ["svg", "png", "xpm"];
    let bases = [
        "/usr/share/icons/hicolor/scalable/apps",
        "/usr/share/icons/hicolor/48x48/apps",
        "/usr/share/icons/Papirus/64x64/apps", 
        "/usr/share/pixmaps",
    ];

    for base in &bases {
        for ext in &exts {
            let path = format!("{}/{}.{}", base, name, ext);
            if Path::new(&path).exists() {
                return Image::load_from_path(Path::new(&path)).ok();
            }
        }
    }
    None
}

fn get_icon_for_file(name: &str, is_dir: bool) -> &'static str {
    if is_dir { return "󰉋"; } 
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "rs" => "", "md" | "txt" | "log" => "󰈙", "pdf" => "",
        "png" | "jpg" | "jpeg" | "webp" | "svg" | "gif" => "󰋩",
        "mp4" | "mkv" | "webm" | "avi" => "󰕧",
        "mp3" | "flac" | "wav" | "ogg" => "󰎆",
        "zip" | "tar" | "gz" | "rar" | "7z" => "󰓯",
        "json" | "toml" | "yaml" | "yml" | "ini" | "conf" => "",
        "sh" | "bash" | "zsh" => "󱆃", "lock" => "",
        _ => "󰈔", 
    }
}

fn format_size(bytes: u64) -> String {
    if bytes == 0 { return "0 B".to_string(); }
    let sizes = ["B", "KB", "MB", "GB"];
    let i = (bytes as f64).log(1024.0).floor() as usize;
    format!("{:.1} {}", (bytes as f64) / 1024_f64.powi(i as i32), sizes[i])
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = LauncherWindow::new()?;
    let matcher = Arc::new(SkimMatcherV2::default());
    
    let mut stream = UnixStream::connect("/tmp/unified_launcher.sock")
        .expect("Daemon is not running! Please start unified-launcher daemon first.");
    
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut init_line = String::new();
    reader.read_line(&mut init_line)?;
    
    let payload: InitPayload = serde_json::from_str(&init_line)?;
    
    let mut app_items = Vec::new();
    for app in payload.apps {
        let mut has_icon = false;
        let mut img = Image::default();
        if let Some(icon_name) = app.icon {
            if let Some(loaded) = find_icon(&icon_name) {
                img = loaded;
                has_icon = true;
            }
        }
        app_items.push(AppItem {
            text: app.name.into(),
            has_native_icon: has_icon,
            native_icon: img,
            text_icon: "".into(),
        });
    }

    let apps_rc = Rc::new(app_items);

    let state = Arc::new(Mutex::new(AppState {
        current_dir_path: String::new(),
        current_dir_contents: Vec::new(),
    }));

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

    let ui_weak_reader = ui.as_weak();
    let state_reader = state.clone();
    let matcher_reader = Arc::clone(&matcher);
    
    thread::spawn(move || {
        let mut line = String::new();
        while let Ok(bytes) = reader.read_line(&mut line) {
            if bytes == 0 { break; }
            let received = line.trim().to_string();
            
            if let Some(json_str) = received.strip_prefix("DIR_RESPONSE:") {
                if let Ok(dir_res) = serde_json::from_str::<DirResponse>(json_str) {
                    
                    {
                        let mut s = state_reader.lock().unwrap();
                        s.current_dir_path = dir_res.current_path.clone();
                        s.current_dir_contents = dir_res.contents.clone();
                    }

                    let current_contents = dir_res.contents;
                    let ui_clone = ui_weak_reader.clone();
                    let m_clone = Arc::clone(&matcher_reader);
                    
                    slint::invoke_from_event_loop(move || {
                        let search_term = {
                            let input = ui_clone.unwrap().get_search_text().to_string();
                            if let Some(idx) = input.rfind('/') { input[idx + 1..].to_string() } else { "".to_string() }
                        };

                        let mut scored_files = Vec::new();
                        for f in &current_contents {
                            if f.name.starts_with('.') && !search_term.starts_with('.') { continue; }
                            
                            let icon = get_icon_for_file(&f.name, f.is_dir);
                            
                            if search_term.is_empty() {
                                scored_files.push((100, f.name.clone(), icon)); 
                            } else if let Some(score) = m_clone.fuzzy_match(&f.name, &search_term) {
                                scored_files.push((score, f.name.clone(), icon));
                            }
                        }
                        
                        if !search_term.is_empty() { scored_files.sort_by(|a, b| b.0.cmp(&a.0)); }
                        let total_count = scored_files.len();
                        
                        let final_list: Vec<AppItem> = scored_files.into_iter().take(100).map(|(_, name, icon)| {
                            AppItem {
                                text: name.into(),
                                has_native_icon: false,
                                native_icon: Image::default(),
                                text_icon: icon.into(),
                            }
                        }).collect();
                        
                        if let Some(ui) = ui_clone.upgrade() {
                            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from(final_list))));
                            let lbl = if total_count == 1 { "1 Item".to_string() } else { format!("{} Items", total_count) };
                            ui.set_ribbon_text(SharedString::from(lbl));
                        }
                    }).unwrap();
                }
            }
            line.clear();
        }
    });

    let ui_handle_search = ui.as_weak();
    let matcher_search = Arc::clone(&matcher);
    let apps_search = Rc::clone(&apps_rc);
    
    ui.on_text_changed(move |query| {
        let ui = ui_handle_search.unwrap();
        let query_str = query.as_str();
        
        ui.set_in_grid(query_str.trim().is_empty());

        // STRIPPED: Module filtering logic completely removed

        if query_str.starts_with('/') || query_str.starts_with('~') || query_str.starts_with("$/") {
            ui.set_current_view("files".into());
            ui.set_is_split_view(false); ui.set_is_two_column(false);
            
            let home = env::var("HOME").unwrap_or_default();
            
            let expanded_query = if query_str.starts_with("$/") {
                query_str.replacen("$/", "/", 1)
            } else if query_str.starts_with('/') {
                format!("{}{}", home, query_str)
            } else {
                query_str.replacen("~", &home, 1)
            };

            if let Some(last_slash) = expanded_query.rfind('/') {
                let dir_path = &expanded_query[..=last_slash];
                let dir_path = if dir_path.is_empty() { "/" } else { dir_path };
                let _ = tx.send(format!("REQUEST_DIR:{}", dir_path)); 
            }
            return;
        }

        ui.set_current_view("main".into());
        ui.set_is_split_view(false); ui.set_is_two_column(false);
        
        if query_str.trim().is_empty() {
            ui.set_display_items(ModelRc::from(Rc::new(VecModel::from((*apps_search).clone()))));
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
            let lbl = if total_count == 1 { "1 Match".to_string() } else { format!("{} Matches", total_count) };
            ui.set_ribbon_text(SharedString::from(lbl));
        }
    });

    let ui_handle_exec = ui.as_weak();
    let state_exec = state.clone(); 
    ui.on_execute_selected(move |selected, _is_grid, is_shift| {
        let ui = ui_handle_exec.unwrap();
        let view = ui.get_current_view();
        let target_str = selected.as_str();

        if view == "main" {
            let _ = tx_exec.send(format!("EXEC_APP:{}", target_str));
            std::process::exit(0);
        } else if view == "files" {
            let s = state_exec.lock().unwrap();
            
            if let Some(file) = s.current_dir_contents.iter().find(|f| f.name == target_str) {
                if is_shift {
                    let path = if file.is_dir { &file.path } else { Path::new(&file.path).parent().unwrap().to_str().unwrap() };
                    let _ = tx_exec.send(format!("EXEC_YAZI:{}", path));
                    std::process::exit(0);
                } else if file.is_dir {
                    let display_path = file.path.replacen(&env::var("HOME").unwrap_or_default(), "~", 1);
                    ui.invoke_set_and_trigger_search(SharedString::from(format!("{}/", display_path)));
                    ui.invoke_focus_search(); 
                } else {
                    let _ = tx_exec.send(format!("EXEC_FILE:{}", file.path));
                    std::process::exit(0);
                }
            }
        }
    });

    let ui_handle_high = ui.as_weak();
    let state_high = state.clone();
    ui.on_item_highlighted(move |idx, total, name| {
        let ui = ui_handle_high.unwrap();
        let s = state_high.lock().unwrap();
        
        if ui.get_current_view() == "files" {
            if let Some(file) = s.current_dir_contents.iter().find(|f| f.name == name.as_str()) {
                if file.is_dir {
                    ui.set_ribbon_text(SharedString::from(format!("Directory  •  {}/{}", idx + 1, total)));
                } else {
                    let size_str = match fs::metadata(&file.path) {
                        Ok(meta) => format_size(meta.len()),
                        Err(_) => "Unknown Size".to_string(),
                    };
                    ui.set_ribbon_text(SharedString::from(format!("{}  •  {}/{}", size_str, idx + 1, total)));
                }
            }
        } else {
            let lbl = if total == 1 { "1 App".to_string() } else { format!("{} Apps", total) };
            ui.set_ribbon_text(SharedString::from(lbl));
        }
    });

    ui.on_sidebar_action(move |index| {
        match index {
            1 => { let _ = std::process::Command::new("zen-browser").spawn(); }
            2 => { let _ = std::process::Command::new("sh").args(["-c", "mpv --player-operation-mode=pseudo-gui"]).spawn(); }
            3 => { 
                let home = env::var("HOME").unwrap_or_default();
                let _ = std::process::Command::new("foot").args(["-D", &home, "-e", "yazi"]).spawn(); 
            }
            4 => { let _ = std::process::Command::new("foot").args(["-e", "btop"]).spawn(); }
            5 => { let _ = std::process::Command::new("foot").args(["-e", "nvim"]).spawn(); }
            _ => {}
        }
        std::process::exit(0); 
    });

    ui.run()?;
    Ok(())
}
