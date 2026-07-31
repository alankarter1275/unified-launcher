# Unified Launcher

A lightweight application launcher + PolKit authentication agent with a client/daemon architecture, built in **Rust + Slint**.

Designed for low-spec machines (4GB RAM, HDD) — the daemon caches app entries so the client launches near-instantly.

## Architecture

```
┌─────────────┐   Unix Socket    ┌──────────────────────────────┐
│   client    │ ◄──────────────► │          daemon              │
│  (Slint UI) │                  │                              │
│             │   JSON protocol  │  • Crawls .desktop files     │
│  Fuzzy      │                  │  • Launches apps on request  │
│  search     │  EXEC_APP:name   │  • PolKit agent (D-Bus)     │
│             │  POWER_ACTION:   │  • Power/session actions     │
│  Quick      │    lock|logout   │                              │
│  sidebar    │    shutdown|     │  • Flatpak + Snap support    │
│             │    reboot        │                              │
└─────────────┘                  └──────────────────────────────┘
```

## Building

You need Rust 1.70+ and the following system libraries:

```bash
# Arch Linux
sudo pacman -S rust slint wayland

# Debian/Ubuntu
sudo apt install rustc cargo libslint-dev

# Fedora
sudo dnf install rust cargo slint-devel
```

Then build:

```bash
git clone https://github.com/yourusername/unified-launcher.git
cd unified-launcher
cargo build --release
```

The release profile strips debug info, optimizes for size, and uses LTO.

## Usage

### 1. Start the daemon (background service)

```bash
./target/release/daemon &
```

The daemon will:
- Crawl `.desktop` files from `/usr/share/applications`, `~/.local/share/applications`,
  Flatpak exports, and Snap desktop entries
- Register a PolKit authentication agent on the system D-Bus
- Listen on a Unix socket for client connections

### 2. Launch the client

```bash
./target/release/client
```

Press **Escape** or select an app to close the launcher.

### 3. Power menu

Click the power icon (⏻) in the sidebar to toggle the power view:

| Action     | Command                          |
|------------|----------------------------------|
| 🔒 Lock    | `swaylock -f -c 000000`         |
| 🚪 Logout  | `swaymsg exit`                  |
| ⏻ Shutdown | `systemctl poweroff`            |
| 🔄 Reboot  | `systemctl reboot`              |

> **Note:** These are optimized for **Sway**. If you use a different compositor/DE,
> edit `handle_power_action()` in `src/bin/daemon.rs`.

### 4. Sidebar quick-launch (Alt+1..5)

| Key     | App                                                |
|---------|----------------------------------------------------|
| Alt+1   | Zen Browser                                        |
| Alt+2   | MPV (pseudo-GUI mode)                              |
| Alt+3   | Yazi file manager (in foot terminal)               |
| Alt+4   | Btop system monitor (in foot terminal)             |
| Alt+5   | Neovim (in foot terminal)                          |

## Sway Integration

For proper overlay positioning, add this to your Sway config:

```
for_window [app_id="unified-launcher"] floating enable, move position center
```

If you want a keybinding to open the launcher:

```
bindsym $mod+space exec /path/to/client
```

## Environment Variables

| Variable                    | Default                     | Description                    |
|-----------------------------|-----------------------------|--------------------------------|
| `UNIFIED_LAUNCHER_SOCKET`   | `/tmp/unified_launcher.sock` | Unix socket path for IPC       |

## PolKit Authentication

The daemon registers itself as a PolKit authentication agent so that privilege-escalation
dialogs (e.g., from `pkexec`, GParted, system settings) show a native password prompt
instead of failing silently.

The password is passed from the GUI client back to the daemon via a secure temporary file
with `0600` permissions — no stdout scraping or magic markers.

## Technical Details

- **Language:** Rust (edition 2021)
- **UI Framework:** Slint 1.8
- **Async Runtime:** Tokio (full features)
- **D-Bus Binding:** zbus 3.14
- **Fuzzy Matching:** skim (fuzzy-matcher crate)
- **IPC:** Unix domain sockets with JSON protocol
- **Binary size:** ~3–5 MB stripped with release profile

## Why Not...

- **rofi/dmenu?** Requires scripting for PolKit integration. Also not instant on HDD.
- **ulauncher?** Python-based, ~50MB+ resident memory.
- **Alfred (macOS)?** Not available on Linux.
- **krunner?** KDE-specific, pulls in half of Plasma.

Unified Launcher is built for **one machine, one owner** — lean, fast, no bloat.

## License

MIT
