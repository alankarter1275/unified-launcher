slint::include_modules!();

use std::io::{self, Write};

fn write_password(password: &str) -> io::Result<()> {
    // The daemon starts this client with stdout connected to a private pipe.
    // Keeping the secret in that pipe avoids any filesystem handoff.
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(password.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let message = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "Authentication Required".to_string());

    let ui = PolkitWindow::new()?;
    ui.set_polkit_message(message.into());

    ui.on_action_submit(move |password| {
        if write_password(password.as_str()).is_err() {
            std::process::exit(1);
        }
        let _ = slint::quit_event_loop();
    });

    ui.on_action_cancel(move || {
        std::process::exit(1);
    });

    ui.run()?;
    Ok(())
}
