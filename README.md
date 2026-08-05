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
│  search     │  JSON-line IPC   │  • PolKit agent (D-Bus)      │
│             │  launch_app      │  • Power/session actions     │
│  Quick      │  power_action    │                              │
│  sidebar    │                  │  • Flatpak + Snap support    │
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

### 4. Pinned applications

When the app search is empty, the launcher shows six persistent app icons in a
single horizontal row.

| Shortcut | Action |
|----------|--------|
| `Alt+1` … `Alt+6` | Launch the corresponding pinned app, even while searching |
| `Ctrl+Alt+1` … `Ctrl+Alt+6` | Pin the highlighted or hovered app to the corresponding slot |

Pins use stable desktop-entry IDs rather than display names, so duplicate app
names do not make a shortcut ambiguous. Pinned entries show their `Alt+N`
shortcut in a pill at the right end of the app list. Assigning an occupied slot
replaces it immediately and shows an in-window confirmation.

### 5. Sidebar views

The five non-power sidebar buttons now switch between inline launcher views for
Quick Settings, Clock & Calendar, Notes, Folders, and File Finder. Their content
is introduced incrementally in later feature phases; the Power view remains
fully available now.

### 6. Quick Settings

The Quick Settings view is intentionally small and Sway-oriented:

| Control | Integration |
|---------|-------------|
| Wi-Fi toggle | `iwctl device <device> set-property Powered on/off` |
| Wi-Fi manager | `footclient -e impala` |
| Bluetooth toggle | `bluetoothctl power on/off` |
| Bluetooth manager | `footclient -e bluetui` |
| Keep screen awake | `systemd-inhibit --what=idle` |
| Prevent suspend | `systemd-inhibit --what=sleep` |
| TLP profile | `pkexec tlp start`, `performance`, `balanced`, or `power-saver` |

Successful and failed toggles send standard desktop notifications, which Dunst
displays. Opening Impala or bluetui deliberately does not send a notification.
TLP actions use PolKit through `pkexec`, so authentication may be requested.

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
| `UNIFIED_LAUNCHER_SOCKET`   | `$XDG_RUNTIME_DIR/unified-launcher.sock` | Absolute Unix socket override for IPC |

## PolKit Authentication

The daemon registers itself as a PolKit authentication agent so that privilege-escalation
dialogs (e.g., from `pkexec`, GParted, system settings) show a native password prompt
instead of failing silently.

The password is returned from the GUI client to the daemon over a private process pipe.
It is never written to a temporary filesystem path.

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
