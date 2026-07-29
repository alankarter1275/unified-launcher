#![allow(warnings)]
slint::include_modules!();
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let message = if args.len() > 1 {
        args[1].clone()
    } else {
        "Authentication Required".to_string()
    };

    let ui = PolkitWindow::new()?;
    ui.set_polkit_message(message.into());

    ui.on_action_submit(move |password| {
        // THE FIX: Wrap the password in specific tags to isolate it from GUI log spam
        println!("__PASS__{}__END__", password.as_str());
        let _ = std::io::stdout().flush();
        let _ = slint::quit_event_loop();
    });

    ui.on_action_cancel(move || {
        std::process::exit(1);
    });

    ui.run()?;
    Ok(())
}
