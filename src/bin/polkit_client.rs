slint::include_modules!();
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // First arg = polkit message, Second arg = temp file path for password
    let message = if args.len() > 1 {
        args[1].clone()
    } else {
        "Authentication Required".to_string()
    };

    let pass_path = if args.len() > 2 {
        args[2].clone()
    } else {
        eprintln!("[polkit-client] Missing temp file path argument. Usage: polkit-client <message> <output_path>");
        std::process::exit(1);
    };

    let ui = PolkitWindow::new()?;
    ui.set_polkit_message(message.into());

    let pass_path_submit = pass_path.clone();
    ui.on_action_submit(move |password| {
        // Write password directly to the temp file (0600 permissions set by daemon)
        if let Ok(mut f) = std::fs::File::create(&pass_path_submit) {
            let _ = f.write_all(password.as_bytes());
            let _ = f.flush();
        }
        let _ = slint::quit_event_loop();
    });

    ui.on_action_cancel(move || {
        std::process::exit(1);
    });

    ui.run()?;
    Ok(())
}
