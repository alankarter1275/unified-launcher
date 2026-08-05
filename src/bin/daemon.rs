use std::collections::HashMap;
use std::env;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use log::{error, info, warn};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::Mutex;
use zbus::{dbus_interface, dbus_proxy, zvariant::Value, Connection, ConnectionBuilder};

use unified_launcher::cache::{load_cache, save_cache};
use unified_launcher::desktop::crawl_desktop_entries;
use unified_launcher::paths::{make_socket_private, remove_stale_socket, socket_path};
use unified_launcher::power::handle_power_action;
use unified_launcher::state::{
    load as load_launcher_state, save as save_launcher_state, LauncherState, PIN_SLOT_COUNT,
};
use unified_launcher::types::{
    json_line, AppEntry, AppInit, ClientMessage, DaemonState, ServerMessage,
};

const POLKIT_AGENT_PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";
const POLKIT_HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

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
        let Some(password) = request_password(&message).await else {
            return;
        };

        if password.is_empty() {
            return;
        }

        authenticate_with_polkit_helper(&cookie, &password).await;
    }

    async fn cancel_authentication(&mut self, _cookie: String) {}
}

/// Ask the separate Slint process for a password over an anonymous stdout pipe.
/// No password is written to a temporary file or passed as a process argument.
async fn request_password(message: &str) -> Option<String> {
    let mut client_executable =
        env::current_exe().unwrap_or_else(|_| PathBuf::from("polkit-client"));
    client_executable.pop();
    client_executable.push("polkit-client");

    let output = match Command::new(client_executable)
        .arg(message)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            warn!("PolKit prompt exited with status {}", output.status);
            return None;
        }
        Err(error) => {
            warn!("Could not start the PolKit prompt: {error}");
            return None;
        }
    };

    let mut password = match String::from_utf8(output.stdout) {
        Ok(password) => password,
        Err(error) => {
            warn!("PolKit prompt returned invalid UTF-8: {error}");
            return None;
        }
    };

    // The prompt appends one line ending. Do not use trim(), because leading or
    // trailing spaces may be part of a valid password.
    if password.ends_with('\n') {
        password.pop();
        if password.ends_with('\r') {
            password.pop();
        }
    }

    Some(password)
}

async fn authenticate_with_polkit_helper(cookie: &str, password: &str) {
    let user = env::var("USER").unwrap_or_else(|_| "root".to_string());
    let mut child = match Command::new(POLKIT_HELPER)
        .arg(&user)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            warn!("Could not start the PolKit helper: {error}");
            return;
        }
    };

    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        warn!("PolKit helper did not expose the expected standard streams");
        return;
    };

    let cookie_line = format!("{cookie}\n");
    if let Err(error) = stdin.write_all(cookie_line.as_bytes()).await {
        warn!("Could not send the PolKit cookie: {error}");
        return;
    }
    if let Err(error) = stdin.flush().await {
        warn!("Could not flush the PolKit cookie: {error}");
        return;
    }

    let mut prompt = String::new();
    let mut buffer = [0_u8; 1];
    let mut password_requested = false;

    info!("PolKit handshake initiated. Waiting for PAM password prompt.");
    loop {
        match stdout.read(&mut buffer).await {
            Ok(0) => break,
            Ok(_) => {
                prompt.push(buffer[0] as char);
                if prompt.chars().count() > 128 {
                    prompt = prompt.chars().skip(64).collect();
                }

                let lower = prompt.to_lowercase();
                if lower.ends_with("password: ") || lower.ends_with("password:") {
                    password_requested = true;
                    break;
                }
            }
            Err(error) => {
                warn!("Could not read the PolKit helper prompt: {error}");
                break;
            }
        }
    }

    if !password_requested {
        warn!("PolKit helper ended before requesting a password");
        let _ = child.wait().await;
        return;
    }

    let password_line = format!("{password}\n");
    if let Err(error) = stdin.write_all(password_line.as_bytes()).await {
        warn!("Could not send the PolKit password: {error}");
        return;
    }
    if let Err(error) = stdin.flush().await {
        warn!("Could not flush the PolKit password: {error}");
        return;
    }
    drop(stdin);

    match child.wait().await {
        Ok(status) => info!("PolKit transaction complete with status {status}"),
        Err(error) => warn!("Could not wait for the PolKit helper: {error}"),
    }
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

/// Register the authentication agent when system D-Bus is available.
///
/// Failure is deliberately non-fatal: the normal launcher daemon must keep
/// serving app launches even on systems without a working PolKit service.
async fn register_polkit_agent() -> Option<Connection> {
    let builder = match ConnectionBuilder::system() {
        Ok(builder) => builder,
        Err(error) => {
            warn!("Could not create the PolKit D-Bus connection: {error}");
            return None;
        }
    };

    let ready_connection = match builder.serve_at(POLKIT_AGENT_PATH, PolkitAgent) {
        Ok(connection) => connection,
        Err(error) => {
            warn!("Could not serve the PolKit agent: {error}");
            return None;
        }
    };

    let connection = match ready_connection.build().await {
        Ok(connection) => connection,
        Err(error) => {
            warn!("Could not connect to system D-Bus for PolKit: {error}");
            return None;
        }
    };

    match AuthorityProxy::new(&connection).await {
        Ok(authority) => {
            let mut details = HashMap::new();
            let session_id = env::var("XDG_SESSION_ID").unwrap_or_default();
            let subject = if session_id.is_empty() {
                details.insert("pid", Value::U32(std::process::id()));
                details.insert("start-time", Value::U64(0));
                ("unix-process".to_string(), details)
            } else {
                details.insert("session-id", Value::from(session_id));
                ("unix-session".to_string(), details)
            };
            let locale = env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_string());

            match authority
                .register_authentication_agent(&subject, &locale, POLKIT_AGENT_PATH)
                .await
            {
                Ok(()) => info!("PolKit authentication agent registered."),
                Err(error) => warn!("Could not register the PolKit agent: {error}"),
            }
        }
        Err(error) => warn!("Could not access the PolKit authority: {error}"),
    }

    Some(connection)
}

// --------------------------------------------------------
// IPC
// --------------------------------------------------------
fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

async fn write_server_message(
    writer: &mut OwnedWriteHalf,
    message: &ServerMessage,
) -> io::Result<()> {
    let payload = json_line(message).map_err(json_error)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.flush().await
}

fn launch_app(app: &AppEntry) -> io::Result<()> {
    let mut parts = app.exec.split_whitespace();
    let command = parts.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("desktop entry {} has no executable", app.id),
        )
    })?;
    let arguments: Vec<&str> = parts.collect();

    if app.needs_terminal {
        let mut terminal_arguments = vec!["-e", command];
        terminal_arguments.extend(arguments);
        std::process::Command::new("footclient")
            .args(terminal_arguments)
            .spawn()
            .map(|_| ())
    } else {
        std::process::Command::new(command)
            .args(arguments)
            .spawn()
            .map(|_| ())
    }
}

async fn set_app_pin(
    state: &Arc<Mutex<DaemonState>>,
    slot: u8,
    app_id: String,
) -> Result<String, String> {
    let slot_index = usize::from(slot);
    if slot_index >= PIN_SLOT_COUNT {
        return Err(format!("Invalid app-pin slot: {}", slot_index + 1));
    }

    let mut state = state.lock().await;
    let app_name = state
        .apps
        .iter()
        .find(|app| app.id == app_id)
        .map(|app| app.name.clone())
        .ok_or_else(|| format!("Unknown application id: {app_id}"))?;

    state
        .launcher_state
        .set_pinned_app(slot_index, app_id)
        .map_err(|error| format!("Could not update app pin: {error}"))?;
    save_launcher_state(&state.launcher_state)
        .map_err(|error| format!("Could not save app pins: {error}"))?;

    Ok(format!("Pinned {app_name} to slot {}", slot_index + 1))
}

async fn handle_client(stream: UnixStream, state: Arc<Mutex<DaemonState>>) {
    let (reader, mut writer) = stream.into_split();
    let (apps, pinned_app_ids): (Vec<AppInit>, Vec<Option<String>>) = {
        let state = state.lock().await;
        let apps = state
            .apps
            .iter()
            .map(|app| AppInit {
                id: app.id.clone(),
                name: app.name.clone(),
                icon: app.icon.clone(),
            })
            .collect();
        (apps, state.launcher_state.pinned_apps.clone())
    };

    if let Err(error) = write_server_message(
        &mut writer,
        &ServerMessage::Init {
            apps,
            pinned_app_ids,
        },
    )
    .await
    {
        warn!("Could not initialize launcher client: {error}");
        return;
    }

    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = match reader.read_line(&mut line).await {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                warn!("Could not read launcher client request: {error}");
                return;
            }
        };
        if bytes_read == 0 {
            return;
        }

        let request = match serde_json::from_str::<ClientMessage>(line.trim_end()) {
            Ok(request) => request,
            Err(error) => {
                let _ = write_server_message(
                    &mut writer,
                    &ServerMessage::Error {
                        message: format!("Invalid launcher request: {error}"),
                    },
                )
                .await;
                continue;
            }
        };

        let result = match request {
            ClientMessage::LaunchApp { app_id } => {
                let app = {
                    let state = state.lock().await;
                    state.apps.iter().find(|app| app.id == app_id).cloned()
                };

                match app {
                    Some(app) => launch_app(&app)
                        .map(|()| format!("Launched {}", app.name))
                        .map_err(|error| format!("Could not launch {}: {error}", app.name)),
                    None => Err(format!("Unknown application id: {app_id}")),
                }
            }
            ClientMessage::SetAppPin { slot, app_id } => set_app_pin(&state, slot, app_id).await,
            ClientMessage::PowerAction { action } => handle_power_action(action)
                .map(|()| "Power action started".to_string())
                .map_err(|error| format!("Could not start power action: {error}")),
        };

        let response = match result {
            Ok(message) => ServerMessage::ActionResult {
                success: true,
                message,
            },
            Err(message) => ServerMessage::ActionResult {
                success: false,
                message,
            },
        };
        let _ = write_server_message(&mut writer, &response).await;
    }
}

// --------------------------------------------------------
// MAIN
// --------------------------------------------------------
#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let apps: Vec<AppEntry> = if let Some(cached) = load_cache() {
        info!("Loaded {} apps from cache.", cached.len());
        cached
    } else {
        info!("Crawling desktop entries...");
        let mut apps = crawl_desktop_entries().await;
        apps.sort_by(|left, right| left.name.cmp(&right.name));
        info!("Found {} apps.", apps.len());
        if let Err(error) = save_cache(&apps) {
            warn!("Could not save the application cache: {error}");
        }
        apps
    };
    let launcher_state = match load_launcher_state() {
        Ok(state) => state,
        Err(error) => {
            warn!("Could not load launcher state; using defaults: {error}");
            LauncherState::default()
        }
    };
    let state = Arc::new(Mutex::new(DaemonState {
        apps,
        launcher_state,
    }));

    // Keep a successful D-Bus connection alive for the lifetime of the daemon,
    // but never make PolKit availability a requirement for launching apps.
    let _polkit_connection = register_polkit_agent().await;

    let socket_path = match socket_path() {
        Ok(path) => path,
        Err(error) => {
            error!("Could not resolve launcher socket path: {error}");
            return;
        }
    };
    if let Err(error) = remove_stale_socket(&socket_path) {
        error!(
            "Could not prepare launcher socket {}: {error}",
            socket_path.display()
        );
        return;
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            error!(
                "Could not bind launcher socket {}: {error}",
                socket_path.display()
            );
            return;
        }
    };
    if let Err(error) = make_socket_private(&socket_path) {
        error!(
            "Could not secure launcher socket {}: {error}",
            socket_path.display()
        );
        let _ = std::fs::remove_file(&socket_path);
        return;
    }
    info!("Listening on {}", socket_path.display());

    loop {
        match listener.accept().await {
            Ok((stream, _address)) => {
                let state = Arc::clone(&state);
                tokio::spawn(handle_client(stream, state));
            }
            Err(error) => error!("Launcher client connection failed: {error}"),
        }
    }
}
