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

use chrono::{Local, NaiveDate};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use slint::{Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use unified_launcher::calendar::{local_clock_text, month_grid, month_start, shift_month};
use unified_launcher::notes::{Note, NoteSummary};
use unified_launcher::paths::socket_path;
use unified_launcher::state::{FolderPin, PIN_SLOT_COUNT};
use unified_launcher::types::{json_line, ClientMessage, PowerAction, ServerMessage};

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

fn render_folders(ui: &LauncherWindow, folders: &[FolderPin]) {
    let items: Vec<FolderItem> = folders
        .iter()
        .map(|folder| FolderItem {
            id: folder.id.clone().into(),
            label: folder.label.clone().into(),
            path: folder.path.clone().into(),
        })
        .collect();
    ui.set_folder_items(ModelRc::from(Rc::new(VecModel::from(items))));
}

fn render_folder_suggestions(ui: &LauncherWindow, suggestions: Vec<String>) {
    let suggestions: Vec<SharedString> = suggestions.into_iter().map(Into::into).collect();
    ui.set_folder_suggestions(ModelRc::from(Rc::new(VecModel::from(suggestions))));
}

fn render_notes(ui: &LauncherWindow, notes: &[NoteSummary]) {
    let items: Vec<NoteItem> = notes
        .iter()
        .map(|note| NoteItem {
            id: note.id.clone().into(),
            title: note.title.clone().into(),
        })
        .collect();
    ui.set_note_items(ModelRc::from(Rc::new(VecModel::from(items))));
}

fn set_current_note(ui: &LauncherWindow, note: &Note) {
    ui.set_selected_note_id(note.id.clone().into());
    ui.set_note_title(note.title.clone().into());
    ui.set_note_content(note.content.clone().into());
    ui.set_note_dirty(false);
}

fn clear_current_note(ui: &LauncherWindow) {
    ui.set_selected_note_id("".into());
    ui.set_note_title("".into());
    ui.set_note_content("".into());
    ui.set_note_dirty(false);
}

fn show_toast(ui: &LauncherWindow, message: impl Into<SharedString>) {
    ui.set_toast_message(message.into());
}

fn render_vault(
    ui: &LauncherWindow,
    vault_items: &[unified_launcher::vault::VaultItem],
    vault_locked: bool,
) {
    let items: Vec<VaultItem> = vault_items
        .iter()
        .map(|item| VaultItem {
            id: item.id.clone().into(),
            name: item.name.clone().into(),
            secret: item.secret.clone().into(),
            note: item.note.clone().into(),
        })
        .collect();
    ui.set_vault_items(ModelRc::from(Rc::new(VecModel::from(items))));
    ui.set_vault_locked(vault_locked);
}

fn handle_vault_response(ui: &LauncherWindow, response: Result<ServerMessage, String>) {
    match response {
        Ok(ServerMessage::ActionResult {
            message,
            vault_items: Some(vault_items),
            vault_locked: Some(vault_locked),
            ..
        }) => {
            render_vault(ui, &vault_items, vault_locked);
            show_toast(ui, message);
        }
        Ok(ServerMessage::ActionResult {
            message,
            vault_items: Some(vault_items),
            ..
        }) => {
            render_vault(ui, &vault_items, false);
            show_toast(ui, message);
        }
        Ok(ServerMessage::ActionResult { message, .. }) | Ok(ServerMessage::Error { message }) => {
            show_toast(ui, message)
        }
        Ok(ServerMessage::FolderPathSuggestions { .. })
        | Ok(ServerMessage::NoteLoaded { .. })
        | Ok(ServerMessage::Init { .. }) => {
            show_toast(ui, "Daemon returned an unexpected response");
        }
        Err(error) => show_toast(ui, format!("Vault action failed: {error}")),
    }
}

fn handle_folder_response(ui: &LauncherWindow, response: Result<ServerMessage, String>) {
    match response {
        Ok(ServerMessage::ActionResult {
            message,
            folder_pins: Some(folder_pins),
            ..
        }) => {
            render_folders(ui, &folder_pins);
            ui.set_folder_form_visible(false);
            ui.set_folder_name("".into());
            ui.set_folder_path("".into());
            render_folder_suggestions(ui, Vec::new());
            show_toast(ui, message);
        }
        Ok(ServerMessage::ActionResult { message, .. }) | Ok(ServerMessage::Error { message }) => {
            show_toast(ui, message)
        }
        Ok(ServerMessage::FolderPathSuggestions { .. })
        | Ok(ServerMessage::NoteLoaded { .. })
        | Ok(ServerMessage::Init { .. }) => {
            show_toast(ui, "Daemon returned an unexpected response");
        }
        Err(error) => show_toast(ui, format!("Folders failed: {error}")),
    }
}

fn handle_note_response(
    ui: &LauncherWindow,
    response: Result<ServerMessage, String>,
    show_success_message: bool,
) {
    match response {
        Ok(ServerMessage::ActionResult {
            success,
            message,
            notes,
            note,
            ..
        }) => {
            if let Some(notes) = notes {
                render_notes(ui, &notes);
            }
            if let Some(note) = note {
                set_current_note(ui, &note);
            }
            if show_success_message || !success {
                show_toast(ui, message);
            }
        }
        Ok(ServerMessage::NoteLoaded { note }) => set_current_note(ui, &note),
        Ok(ServerMessage::Error { message }) => show_toast(ui, message),
        Ok(ServerMessage::FolderPathSuggestions { .. }) | Ok(ServerMessage::Init { .. }) => {
            show_toast(ui, "Daemon returned an unexpected response");
        }
        Err(error) => show_toast(ui, format!("Notes failed: {error}")),
    }
}

fn render_calendar(ui: &LauncherWindow, month: NaiveDate) {
    let today = Local::now().date_naive();
    let days: Vec<CalendarDay> = month_grid(month, today)
        .into_iter()
        .map(|cell| CalendarDay {
            label: cell.day.to_string().into(),
            in_current_month: cell.in_current_month,
            is_today: cell.is_today,
        })
        .collect();
    ui.set_calendar_month(month.format("%B %Y").to_string().into());
    ui.set_calendar_days(ModelRc::from(Rc::new(VecModel::from(days))));
}

fn refresh_clock(ui: &LauncherWindow) {
    let (clock, date) = local_clock_text();
    ui.set_clock_text(clock.into());
    ui.set_clock_date(date.into());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = socket_path()?;
    let stream = connect_or_start_daemon(&socket_path)?;

    let ui = LauncherWindow::new()?;
    let matcher = Arc::new(SkimMatcherV2::default());
    let calendar_month = Rc::new(RefCell::new(month_start(Local::now().date_naive())));
    refresh_clock(&ui);
    render_calendar(&ui, *calendar_month.borrow());

    let ui_clock = ui.as_weak();
    let clock_timer = Timer::default();
    clock_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        if let Some(ui) = ui_clock.upgrade() {
            refresh_clock(&ui);
        }
    });

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut init_line = String::new();
    reader.read_line(&mut init_line)?;
    let initial_message: ServerMessage = serde_json::from_str(&init_line)?;
    let (apps, pinned_app_ids, folder_pins, notes) = match initial_message {
        ServerMessage::Init {
            apps,
            pinned_app_ids,
            folder_pins,
            notes,
        } => (apps, pinned_app_ids, folder_pins, notes),
        ServerMessage::Error { message } => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, message).into());
        }
        ServerMessage::ActionResult { .. }
        | ServerMessage::FolderPathSuggestions { .. }
        | ServerMessage::NoteLoaded { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "daemon sent a non-initialization message before initialization",
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
    render_folders(&ui, &folder_pins);
    render_notes(&ui, &notes);

    // Initial empty, locked vault state. The vault doesn't unlock until user interaction.
    render_vault(&ui, &[], true);

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
            Ok(ServerMessage::FolderPathSuggestions { .. })
            | Ok(ServerMessage::NoteLoaded { .. })
            | Ok(ServerMessage::Init { .. }) => {
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

    let writer_open_file = Arc::clone(&writer);
    ui.on_open_file(move |path| {
        send_request(
            &writer_open_file,
            &ClientMessage::OpenFile {
                path: path.to_string(),
            },
        );
        std::process::exit(0);
    });

    let writer_open_file_yazi = Arc::clone(&writer);
    ui.on_open_file_in_yazi(move |path| {
        send_request(
            &writer_open_file_yazi,
            &ClientMessage::OpenFileInYazi {
                path: path.to_string(),
            },
        );
        std::process::exit(0);
    });

    let writer_create_note = Arc::clone(&writer);
    let reader_create_note = Arc::clone(&reader);
    let ui_create_note = ui.as_weak();
    ui.on_create_note(move || {
        let ui = ui_create_note.unwrap();
        let response = request_response(
            &writer_create_note,
            &reader_create_note,
            &ClientMessage::CreateNote {
                title: "Untitled".to_string(),
            },
        );
        handle_note_response(&ui, response, true);
    });

    let writer_load_note = Arc::clone(&writer);
    let reader_load_note = Arc::clone(&reader);
    let ui_load_note = ui.as_weak();
    ui.on_load_note(move |id| {
        let ui = ui_load_note.unwrap();
        let response = request_response(
            &writer_load_note,
            &reader_load_note,
            &ClientMessage::LoadNote { id: id.to_string() },
        );
        handle_note_response(&ui, response, false);
    });

    let writer_save_note = Arc::clone(&writer);
    let reader_save_note = Arc::clone(&reader);
    let ui_save_note = ui.as_weak();
    ui.on_save_note(move |id, title, content| {
        if id.is_empty() {
            return;
        }
        let ui = ui_save_note.unwrap();
        let response = request_response(
            &writer_save_note,
            &reader_save_note,
            &ClientMessage::SaveNote {
                id: id.to_string(),
                title: title.to_string(),
                content: content.to_string(),
            },
        );
        handle_note_response(&ui, response, false);
    });

    let writer_delete_note = Arc::clone(&writer);
    let reader_delete_note = Arc::clone(&reader);
    let ui_delete_note = ui.as_weak();
    ui.on_delete_note(move |id| {
        let ui = ui_delete_note.unwrap();
        let was_selected = ui.get_selected_note_id().as_str() == id.as_str();
        let response = request_response(
            &writer_delete_note,
            &reader_delete_note,
            &ClientMessage::DeleteNote { id: id.to_string() },
        );
        let deleted = matches!(
            &response,
            Ok(ServerMessage::ActionResult { success: true, .. })
        );
        handle_note_response(&ui, response, true);
        if deleted && was_selected {
            clear_current_note(&ui);
        }
    });

    let calendar_month_previous = Rc::clone(&calendar_month);
    let ui_calendar_previous = ui.as_weak();
    ui.on_calendar_previous_month(move || {
        let ui = ui_calendar_previous.unwrap();
        let mut month = calendar_month_previous.borrow_mut();
        *month = shift_month(*month, -1);
        render_calendar(&ui, *month);
    });

    let calendar_month_next = Rc::clone(&calendar_month);
    let ui_calendar_next = ui.as_weak();
    ui.on_calendar_next_month(move || {
        let ui = ui_calendar_next.unwrap();
        let mut month = calendar_month_next.borrow_mut();
        *month = shift_month(*month, 1);
        render_calendar(&ui, *month);
    });

    let calendar_month_today = Rc::clone(&calendar_month);
    let ui_calendar_today = ui.as_weak();
    ui.on_calendar_today(move || {
        let ui = ui_calendar_today.unwrap();
        let mut month = calendar_month_today.borrow_mut();
        *month = month_start(Local::now().date_naive());
        render_calendar(&ui, *month);
    });

    let writer_vault_unlock = Arc::clone(&writer);
    let reader_vault_unlock = Arc::clone(&reader);
    let ui_vault_unlock = ui.as_weak();
    ui.on_vault_unlock(move |password| {
        let ui = ui_vault_unlock.unwrap();
        let response = request_response(
            &writer_vault_unlock,
            &reader_vault_unlock,
            &ClientMessage::VaultUnlock {
                password: password.to_string(),
            },
        );
        handle_vault_response(&ui, response);
        ui.set_vault_password("".into());
    });

    let writer_vault_lock = Arc::clone(&writer);
    let reader_vault_lock = Arc::clone(&reader);
    let ui_vault_lock = ui.as_weak();
    ui.on_vault_lock(move || {
        let ui = ui_vault_lock.unwrap();
        let response = request_response(
            &writer_vault_lock,
            &reader_vault_lock,
            &ClientMessage::VaultLock,
        );
        handle_vault_response(&ui, response);
    });

    let writer_vault_add = Arc::clone(&writer);
    let reader_vault_add = Arc::clone(&reader);
    let ui_vault_add = ui.as_weak();
    ui.on_vault_add(move |name, secret, note| {
        let ui = ui_vault_add.unwrap();
        if name.as_str().trim().is_empty() || secret.as_str().trim().is_empty() {
            show_toast(&ui, "Enter both a name and a secret");
            return;
        }
        let response = request_response(
            &writer_vault_add,
            &reader_vault_add,
            &ClientMessage::VaultAdd {
                name: name.to_string(),
                secret: secret.to_string(),
                note: note.to_string(),
            },
        );
        handle_vault_response(&ui, response);
    });

    let writer_vault_delete = Arc::clone(&writer);
    let reader_vault_delete = Arc::clone(&reader);
    let ui_vault_delete = ui.as_weak();
    ui.on_vault_delete(move |id| {
        let ui = ui_vault_delete.unwrap();
        let response = request_response(
            &writer_vault_delete,
            &reader_vault_delete,
            &ClientMessage::VaultDelete { id: id.to_string() },
        );
        handle_vault_response(&ui, response);
    });

    let writer_folder_suggestions = Arc::clone(&writer);
    let reader_folder_suggestions = Arc::clone(&reader);
    let ui_folder_suggestions = ui.as_weak();
    ui.on_folder_path_changed(move |path| {
        let ui = ui_folder_suggestions.unwrap();
        let response = request_response(
            &writer_folder_suggestions,
            &reader_folder_suggestions,
            &ClientMessage::FolderPathSuggestions {
                path: path.to_string(),
            },
        );
        match response {
            Ok(ServerMessage::FolderPathSuggestions { suggestions }) => {
                render_folder_suggestions(&ui, suggestions);
            }
            Ok(ServerMessage::Error { .. }) | Err(_) => {
                // A partial path often has no readable parent yet; simply hide
                // completion until the user reaches one.
                render_folder_suggestions(&ui, Vec::new());
            }
            Ok(_) => show_toast(&ui, "Daemon returned an unexpected response"),
        }
    });

    let ui_folder_suggestion = ui.as_weak();
    ui.on_folder_suggestion_selected(move |path| {
        let ui = ui_folder_suggestion.unwrap();
        ui.set_folder_path(path);
        render_folder_suggestions(&ui, Vec::new());
    });

    let writer_add_folder = Arc::clone(&writer);
    let reader_add_folder = Arc::clone(&reader);
    let ui_add_folder = ui.as_weak();
    ui.on_add_folder(move |label, path| {
        let ui = ui_add_folder.unwrap();
        if label.as_str().trim().is_empty() || path.as_str().trim().is_empty() {
            show_toast(&ui, "Enter both a name and a path");
            return;
        }
        let response = request_response(
            &writer_add_folder,
            &reader_add_folder,
            &ClientMessage::CreateFolderPin {
                label: label.to_string(),
                path: path.to_string(),
            },
        );
        handle_folder_response(&ui, response);
    });

    let writer_delete_folder = Arc::clone(&writer);
    let reader_delete_folder = Arc::clone(&reader);
    let ui_delete_folder = ui.as_weak();
    ui.on_delete_folder(move |id| {
        let ui = ui_delete_folder.unwrap();
        let response = request_response(
            &writer_delete_folder,
            &reader_delete_folder,
            &ClientMessage::DeleteFolderPin { id: id.to_string() },
        );
        handle_folder_response(&ui, response);
    });

    let writer_open_folder = Arc::clone(&writer);
    ui.on_open_folder(move |id| {
        send_request(
            &writer_open_folder,
            &ClientMessage::OpenFolder { id: id.to_string() },
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
    clock_timer.stop();
    Ok(())
}
