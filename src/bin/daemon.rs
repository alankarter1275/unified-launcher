use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{read_dir, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppEntry { name: String, exec: String, icon: Option<String>, needs_terminal: bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScriptEntry { name: String, path: PathBuf }

struct DaemonState {
    apps: Vec<AppEntry>,
    scripts: Vec<ScriptEntry>,
}

#[derive(Serialize)]
struct AppInit {
    name: String,
    icon: Option<String>,
}

#[derive(Serialize)]
struct InitPayload {
    apps: Vec<AppInit>,
    scripts: Vec<String>,
}

#[derive(Serialize)]
struct FileEntry {
    name: String,
    is_dir: bool,
    path: String,
}

#[derive(Serialize)]
struct DirResponse {
    current_path: String,
    contents: Vec<FileEntry>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Crawling Vaults (Apps, Scripts)...");
    
    let mut apps = crawl_desktop_entries().await;
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    
    let mut scripts = crawl_scripts().await;
    scripts.sort_by(|a, b| a.name.cmp(&b.name));

    println!("Vault locked: {} apps, {} scripts.", apps.len(), scripts.len());

    let state = Arc::new(Mutex::new(DaemonState { apps, scripts }));

    let socket_path = "/tmp/unified_launcher.sock";
    let _ = fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    
    println!("Daemon running. Listening on {}", socket_path);

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state_clone = Arc::clone(&state);
                
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let vault = state_clone.lock().await;
                    
                    let payload_data = InitPayload { 
                        apps: vault.apps.iter().map(|a| AppInit { name: a.name.clone(), icon: a.icon.clone() }).collect(), 
                        scripts: vault.scripts.iter().map(|s| s.name.clone()).collect(),
                    };
                    
                    let payload = serde_json::to_string(&payload_data).unwrap() + "\n";
                    if let Err(e) = writer.write_all(payload.as_bytes()).await {
                        eprintln!("Socket write failed: {}", e);
                        return;
                    }
                    drop(vault); 

                    let mut buf_reader = BufReader::new(reader);
                    let mut command_line = String::new();
                    
                    while let Ok(bytes_read) = buf_reader.read_line(&mut command_line).await {
                        if bytes_read == 0 { break; } 
                        
                        let received = command_line.trim();
                        let vault = state_clone.lock().await;
                        
                        if let Some(target_dir) = received.strip_prefix("REQUEST_DIR:") {
                            let home = env::var("HOME").unwrap_or_default();
                            let parsed_dir = target_dir.replace("~", &home);
                            let dir_path = PathBuf::from(&parsed_dir);

                            let mut contents = Vec::new();
                            if let Ok(mut dir) = read_dir(&dir_path).await {
                                while let Ok(Some(entry)) = dir.next_entry().await {
                                    if let Ok(metadata) = entry.metadata().await {
                                        contents.push(FileEntry {
                                            name: entry.file_name().to_string_lossy().to_string(),
                                            is_dir: metadata.is_dir(),
                                            path: entry.path().to_string_lossy().to_string(),
                                        });
                                    }
                                }
                            }

                            contents.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));

                            let response_data = DirResponse {
                                current_path: parsed_dir,
                                contents,
                            };
                            let response_json = format!("DIR_RESPONSE:{}\n", serde_json::to_string(&response_data).unwrap());
                            let _ = writer.write_all(response_json.as_bytes()).await;
                        }
                        else if let Some(app_name) = received.strip_prefix("EXEC_APP:") {
                            if let Some(app) = vault.apps.iter().find(|a| a.name == app_name) {
                                let mut parts = app.exec.split_whitespace();
                                if let Some(cmd) = parts.next() {
                                    let args: Vec<&str> = parts.collect();
                                    if app.needs_terminal {
                                        let mut term_args = vec!["-e", cmd];
                                        term_args.extend(args);
                                        let _ = std::process::Command::new("foot").args(term_args).spawn();
                                    } else {
                                        let _ = std::process::Command::new(cmd).args(args).spawn();
                                    }
                                }
                            }
                        }
                        else if let Some(path) = received.strip_prefix("EXEC_YAZI:") {
                            let _ = std::process::Command::new("foot").args(["-D", path, "-e", "yazi"]).spawn();
                        }
                        else if let Some(file_path) = received.strip_prefix("EXEC_FILE:") {
                            let _ = std::process::Command::new("xdg-open").arg(file_path).spawn();
                        }
                        command_line.clear();
                    }
                });
            }
            Err(e) => eprintln!("Connection failed: {}", e),
        }
    }
}

async fn crawl_scripts() -> Vec<ScriptEntry> {
    let mut entries = Vec::new();
    if let Ok(home) = env::var("HOME") {
        let script_dir = PathBuf::from(format!("{}/.config/unified-launcher/scripts", home));
        if let Ok(mut dir) = read_dir(&script_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                        entries.push(ScriptEntry { name: name.to_string(), path });
                    }
                }
            }
        }
    }
    entries
}

async fn crawl_desktop_entries() -> Vec<AppEntry> {
    let mut entries = Vec::new();
    let mut paths_to_check = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = env::var("HOME") { paths_to_check.push(PathBuf::from(format!("{}/.local/share/applications", home))); }
    for dir_path in paths_to_check {
        if let Ok(mut dir) = read_dir(&dir_path).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                    if let Some(app) = parse_single_desktop_file(&path).await { entries.push(app); }
                }
            }
        }
    }
    entries
}

async fn parse_single_desktop_file(path: &PathBuf) -> Option<AppEntry> {
    let file = File::open(path).await.ok()?;
    let mut reader = BufReader::new(file).lines();
    let mut name = String::new(); let mut exec = String::new();
    let mut icon = None;
    let mut no_display = false; let mut needs_terminal = false;

    while let Ok(Some(line)) = reader.next_line().await {
        if line.starts_with("Name=") && !line.contains("Name[") && name.is_empty() { name = line[5..].trim().to_string(); } 
        else if line.starts_with("Exec=") && exec.is_empty() {
            exec = line[5..].split_whitespace().filter(|&w| !w.starts_with('%')).collect::<Vec<_>>().join(" ");
        } 
        else if line.starts_with("Icon=") && icon.is_none() {
            icon = Some(line[5..].trim().to_string());
        }
        else if line.starts_with("NoDisplay=true") || line.starts_with("Hidden=true") { no_display = true; } 
        else if line.starts_with("Terminal=true") { needs_terminal = true; }
    }
    if no_display || name.is_empty() || exec.is_empty() { None } 
    else { Some(AppEntry { name, exec, icon, needs_terminal }) }
}
