use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{read_dir, File};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use zbus::{dbus_interface, dbus_proxy, zvariant::Value, ConnectionBuilder};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppEntry { name: String, exec: String, icon: Option<String>, needs_terminal: bool }

#[derive(Serialize)]
struct AppInit { name: String, icon: Option<String> }
#[derive(Serialize)]
struct InitPayload { apps: Vec<AppInit> }

struct DaemonState { apps: Vec<AppEntry> }

fn find_icon(name: &str) -> Option<String> {
    if name.starts_with('/') && PathBuf::from(name).exists() { return Some(name.to_string()); }
    let exts = ["svg", "png", "xpm"];
    let bases = ["/usr/share/icons/hicolor/scalable/apps", "/usr/share/icons/hicolor/48x48/apps", "/usr/share/icons/Papirus/64x64/apps", "/usr/share/pixmaps"];
    for base in &bases {
        for ext in &exts {
            let path = format!("{}/{}.{}", base, name, ext);
            if PathBuf::from(&path).exists() { return Some(path); }
        }
    }
    None
}

// --------------------------------------------------------
// POLKIT AGENT INTERFACE
// --------------------------------------------------------
struct PolkitAgent;

#[dbus_interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl PolkitAgent {
    async fn begin_authentication(
        &mut self,
        _action_id: String,
        message: String,
        _icon_name: String,
        _details: std::collections::HashMap<String, String>,
        cookie: String,
        _identities: Vec<(String, std::collections::HashMap<String, zbus::zvariant::OwnedValue>)>,
    ) {
        let mut client_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("polkit-client"));
        client_exe.pop();
        client_exe.push("polkit-client");

        let output = tokio::process::Command::new(client_exe)
            .arg(&message)
            .stdout(std::process::Stdio::piped())
            .output()
            .await;

        if let Ok(out) = output {
            if out.status.success() {
                let full_output = String::from_utf8_lossy(&out.stdout).to_string();

                let password = if let (Some(start), Some(end)) = (full_output.find("__PASS__"), full_output.find("__END__")) {
                    full_output[start + 8..end].to_string()
                } else {
                    full_output.trim().to_string()
                };

                let user = env::var("USER").unwrap_or_else(|_| "root".to_string());

                use tokio::process::Command;
                use std::process::Stdio;

                if let Ok(mut child) = Command::new("/usr/lib/polkit-1/polkit-agent-helper-1")
                    .arg(&user)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    // Inherit stderr so we can see PAM's anger directly in the daemon console if it fails
                    .stderr(Stdio::inherit())
                    .spawn()
                {
                    if let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) {

                        let _ = stdin.write_all(format!("{}\n", cookie).as_bytes()).await;
                        let _ = stdin.flush().await;

                        use tokio::io::AsyncReadExt;
                        let mut prompt = String::new();
                        let mut buf = [0; 1];

                        println!("\n[Polkit] Handshake initiated. Waiting for PAM...");
                        loop {
                            match stdout.read(&mut buf).await {
                                Ok(0) => break,
                                Ok(_) => {
                                    let c = buf[0] as char;
                                    prompt.push(c);

                                    // Print the exact output to your daemon console so we can monitor it
                                    print!("{}", c);
                                    let _ = std::io::Write::flush(&mut std::io::stdout());

                                    let lower = prompt.to_lowercase();
                                    if lower.ends_with("password: ") || lower.ends_with("password:") {
                                        println!("\n[Polkit] System is ready. Firing payload...");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("\n[Polkit] Error reading PAM output: {}", e);
                                    break;
                                }
                            }
                        }

                        let _ = stdin.write_all(format!("{}\n", password).as_bytes()).await;
                        let _ = stdin.flush().await;

                        // THE FIX: Trap `stdin` and force it to stay open by awaiting the child process INSIDE the scope
                        let exit_status = child.wait().await;
                        println!("[Polkit] Transaction complete. PAM exit status: {:?}", exit_status);
                    }
                }
            }
        }
    }

    async fn cancel_authentication(&mut self, _cookie: String) {}
}

#[dbus_proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn register_authentication_agent(
        &self,
        subject: &(String, std::collections::HashMap<&str, zbus::zvariant::Value<'_>>),
        locale: &str,
        object_path: &str,
    ) -> zbus::Result<()>;
}

// --------------------------------------------------------
// MAIN DAEMON LOOP
// --------------------------------------------------------
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Crawling Vaults (Apps)...");
    let mut apps = crawl_desktop_entries().await;
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    println!("Vault locked: {} apps.", apps.len());
    let state = Arc::new(Mutex::new(DaemonState { apps }));

    let dbus_conn = ConnectionBuilder::system()?
        .serve_at("/org/freedesktop/PolicyKit1/AuthenticationAgent", PolkitAgent)?
        .build()
        .await?;

    let authority = AuthorityProxy::new(&dbus_conn).await?;
    let mut subject_details = std::collections::HashMap::new();
    let session_id = env::var("XDG_SESSION_ID").unwrap_or_default();

    let subject = if !session_id.is_empty() {
        subject_details.insert("session-id", Value::from(session_id));
        ("unix-session".to_string(), subject_details)
    } else {
        subject_details.insert("pid", Value::U32(std::process::id()));
        subject_details.insert("start-time", Value::U64(0));
        ("unix-process".to_string(), subject_details)
    };

    authority.register_authentication_agent(&subject, "en_US.UTF-8", "/org/freedesktop/PolicyKit1/AuthenticationAgent").await?;
    println!("Native Polkit Agent registered on the system bus.");

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
                    };
                    let payload = serde_json::to_string(&payload_data).unwrap() + "\n";
                    let _ = writer.write_all(payload.as_bytes()).await;
                    drop(vault);

                    let mut buf_reader = BufReader::new(reader);
                    let mut command_line = String::new();

                    while let Ok(bytes_read) = buf_reader.read_line(&mut command_line).await {
                        if bytes_read == 0 { break; }
                        let received = command_line.trim();
                        let vault = state_clone.lock().await;

                        if let Some(app_name) = received.strip_prefix("EXEC_APP:") {
                            if let Some(app) = vault.apps.iter().find(|a| a.name == app_name) {
                                let mut parts = app.exec.split_whitespace();
                                if let Some(cmd) = parts.next() {
                                    let args: Vec<&str> = parts.collect();
                                    if app.needs_terminal {
                                        let mut term_args = vec!["-e", cmd];
                                        term_args.extend(args);
                                        let _ = std::process::Command::new("footclient").args(term_args).spawn();
                                    } else {
                                        let _ = std::process::Command::new(cmd).args(args).spawn();
                                    }
                                }
                            }
                        }
                        command_line.clear();
                    }
                });
            }
            Err(e) => eprintln!("Connection failed: {}", e),
        }
    }
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
        else if line.starts_with("Exec=") && exec.is_empty() { exec = line[5..].split_whitespace().filter(|&w| !w.starts_with('%')).collect::<Vec<_>>().join(" "); }
        else if line.starts_with("Icon=") && icon.is_none() { icon = find_icon(line[5..].trim()); }
        else if line.starts_with("NoDisplay=true") || line.starts_with("Hidden=true") { no_display = true; }
        else if line.starts_with("Terminal=true") { needs_terminal = true; }
    }
    if no_display || name.is_empty() || exec.is_empty() { None }
    else { Some(AppEntry { name, exec, icon, needs_terminal }) }
}
