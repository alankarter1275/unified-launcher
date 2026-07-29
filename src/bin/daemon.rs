use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use zbus::{dbus_interface, dbus_proxy, zvariant::Value, ConnectionBuilder};
use log::{info, warn, error};

use unified_launcher::cache::{load_cache, save_cache};
use unified_launcher::desktop::crawl_desktop_entries;
use unified_launcher::power::handle_power_action;
use unified_launcher::types::{AppEntry, AppInit, DaemonState, InitPayload, socket_path};

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
        _details: HashMap<String, String>,
        cookie: String,
        _identities: Vec<(String, HashMap<String, zbus::zvariant::OwnedValue>)>,
    ) {
        let mut client_exe = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("polkit-client"));
        client_exe.pop();
        client_exe.push("polkit-client");

        // Secure temp file for password transport
        let tmp_path = format!("/tmp/polkit_pass_{}", std::process::id());
        let tmp_path_b = tmp_path.clone();

        let output = tokio::process::Command::new(client_exe)
            .arg(&message)
            .arg(&tmp_path_b)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .output()
            .await;

        let password: Option<String> = if let Ok(out) = output {
            if out.status.success() {
                fs::read_to_string(&tmp_path).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        } else {
            None
        };
        let _ = fs::remove_file(&tmp_path);

        if let Some(password) = password {
            if password.is_empty() {
                return;
            }

            let user = env::var("USER").unwrap_or_else(|_| "root".to_string());

            use tokio::process::Command;
            use std::process::Stdio;

            if let Ok(mut child) = Command::new("/usr/lib/polkit-1/polkit-agent-helper-1")
                .arg(&user)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
            {
                if let (Some(mut stdin), Some(mut stdout)) =
                    (child.stdin.take(), child.stdout.take())
                {
                    let _ = stdin
                        .write_all(format!("{}\n", cookie).as_bytes())
                        .await;
                    let _ = stdin.flush().await;

                    use tokio::io::AsyncReadExt;
                    let mut prompt = String::new();
                    let mut buf = [0; 1];

                    info!("PolKit handshake initiated. Waiting for PAM...");
                    loop {
                        match stdout.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(_) => {
                                let c = buf[0] as char;
                                prompt.push(c);
                                print!("{}", c);
                                let _ = std::io::Write::flush(&mut std::io::stdout());

                                let lower = prompt.to_lowercase();
                                if lower.ends_with("password: ")
                                    || lower.ends_with("password:")
                                {
                                    info!("PolKit system ready. Sending password...");
                                    break;
                                }
                            }
                            Err(e) => {
                                error!("Error reading PAM output: {}", e);
                                break;
                            }
                        }
                    }

                    let _ = stdin
                        .write_all(format!("{}\n", password).as_bytes())
                        .await;
                    let _ = stdin.flush().await;

                    let exit_status = child.wait().await;
                    info!("PolKit transaction complete. PAM exit status: {:?}", exit_status);
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
        subject: &(String, HashMap<&str, zbus::zvariant::Value<'_>>),
        locale: &str,
        object_path: &str,
    ) -> zbus::Result<()>;
}

// --------------------------------------------------------
// MAIN
// --------------------------------------------------------
#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .format_timestamp_millis()
    .init();

    // ---- LOAD / CRAWL APPS (with caching) ----
    let apps: Vec<AppEntry> = if let Some(cached) = load_cache() {
        info!("Loaded {} apps from cache.", cached.len());
        cached
    } else {
        info!("Crawling desktop entries...");
        let mut apps = crawl_desktop_entries().await;
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        info!("Found {} apps.", apps.len());
        save_cache(&apps);
        apps
    };
    let state = Arc::new(Mutex::new(DaemonState { apps }));

    // ---- Register PolKit authentication agent (non-fatal) ----
    let dbus_conn = match ConnectionBuilder::system() {
        Ok(builder) => {
            match builder.serve_at(
                "/org/freedesktop/PolicyKit1/AuthenticationAgent",
                PolkitAgent,
            ) {
                Ok(ready) => match ready.build().await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("Could not connect to system D-Bus: {}", e);
                        return;
                    }
                },
                Err(e) => {
                    warn!("Could not serve PolKit agent: {}", e);
                    return;
                }
            }
        }
        Err(e) => {
            warn!("Could not create D-Bus connection builder: {}", e);
            return;
        }
    };

    match AuthorityProxy::new(&dbus_conn).await {
        Ok(authority) => {
            let mut subject_details = HashMap::new();
            let session_id = env::var("XDG_SESSION_ID").unwrap_or_default();

            let subject = if !session_id.is_empty() {
                subject_details.insert("session-id", Value::from(session_id));
                ("unix-session".to_string(), subject_details)
            } else {
                subject_details.insert("pid", Value::U32(std::process::id()));
                subject_details.insert("start-time", Value::U64(0));
                ("unix-process".to_string(), subject_details)
            };

            match authority
                .register_authentication_agent(
                    &subject,
                    "en_US.UTF-8",
                    "/org/freedesktop/PolicyKit1/AuthenticationAgent",
                )
                .await
            {
                Ok(_) => info!("PolKit agent registered on the system bus."),
                Err(e) => warn!("Could not register PolKit agent: {}", e),
            }
        }
        Err(e) => warn!("Could not connect to PolKit Authority: {}", e),
    }

    // ---- Unix socket listener ----
    let sock_path = socket_path();
    let _ = fs::remove_file(&sock_path);
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            error!("Could not bind socket {}: {}", sock_path, e);
            return;
        }
    };
    info!("Listening on {}", sock_path);

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state_clone = Arc::clone(&state);
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let vault = state_clone.lock().await;

                    let payload_data = InitPayload {
                        apps: vault
                            .apps
                            .iter()
                            .map(|a| AppInit {
                                name: a.name.clone(),
                                icon: a.icon.clone(),
                            })
                            .collect(),
                    };
                    let payload =
                        serde_json::to_string(&payload_data).unwrap() + "\n";
                    let _ = writer.write_all(payload.as_bytes()).await;
                    drop(vault);

                    let mut buf_reader = BufReader::new(reader);
                    let mut command_line = String::new();

                    while let Ok(bytes_read) = buf_reader.read_line(&mut command_line).await {
                        if bytes_read == 0 {
                            break;
                        }
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
                                        let _ = std::process::Command::new("footclient")
                                            .args(term_args)
                                            .spawn();
                                    } else {
                                        let _ = std::process::Command::new(cmd)
                                            .args(args)
                                            .spawn();
                                    }
                                }
                            }
                        } else if let Some(action) = received.strip_prefix("POWER_ACTION:") {
                            handle_power_action(action);
                        }
                        command_line.clear();
                    }
                });
            }
            Err(e) => error!("Connection failed: {}", e),
        }
    }
}
