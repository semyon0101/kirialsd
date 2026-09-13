# kirialsd ☀️🌙

[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Linux-blue?logo=linux)](https://kernel.org)
[![License](https://img.shields.io/badge/License-MIT%2FApache--2.0-green)](#license)

An ultra-lightweight, intelligent Ambient Light Sensor (ALS) daemon for Linux laptops and desktop workstations.

**`kirialsd`** bridges ambient light sensors exposed via `iio-sensor-proxy` directly to Linux `/sys/class/backlight` controllers. It features self-learning curve calibration, adaptive smoothstep transitions, hardware clamping to eliminate black screens, and seamless suspend/resume handling.

---

## Table of Contents

- [Features](#features)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
  - [1. Building from Source](#1-building-from-source)
  - [2. Backlight Permissions (udev)](#2-backlight-permissions-udev)
  - [3. Running as a Systemd Service](#3-running-as-a-systemd-service)
- [Command-Line Options](#command-line-options)
- [Runtime Signals (IPC)](#runtime-signals-ipc)
- [Configuration Reference](#configuration-reference)
- [Self-Learning Calibration](#self-learning-calibration)
- [Waybar & Status Bar Integration](#waybar--status-bar-integration)
- [Troubleshooting](#troubleshooting)
- [License](#license)

---

## Features

- ⚡ **Event-Driven & Hybrid Polling:** Subscribes to D-Bus `PropertiesChanged` signals from `net.hadess.SensorProxy` for immediate response, with configurable periodic fallback polling.
- 🌊 **Adaptive Smoothstep Animations:** Uses cubic Hermite smoothstep interpolation ($3t^2 - 2t^3$) for natural, eye-friendly transitions without stepping artifacts. Transition durations adapt dynamically to step magnitude (micro-steps finish swiftly; large ambient shifts glide smoothly).
- 🧠 **Monotonic Self-Learning Curve:** Adjusting brightness manually (via Fn keys or desktop sliders) teaches `kirialsd` your lighting preferences. The daemon smoothly warps surrounding control points while strictly enforcing mathematical monotonicity ($y_i \le y_{i+1}$).
- 🛡️ **Sensor Noise Suppression & Dual Hysteresis:** Combines a 3-sample rolling median filter (`MedianFilter3`) to reject ADC spikes with both relative and absolute hysteresis thresholds and an ambient hold timer to filter transient shadows.
- 💤 **systemd-logind Sleep/Resume Awareness:** Subscribes to `org.freedesktop.login1.Manager.PrepareForSleep`. Pauses brightness changes before suspend and immediately samples fresh lux on wake before resuming adjustments.
- 🔒 **Zero-Blackscreen Hardware Clamping:** Clamps minimum brightness to a safe percentage to prevent your display backlight from switching off entirely in darkness.
- 📊 **One-Shot Status Mode (`-s`):** Fast, non-daemon JSON status readout for status bars (Waybar, Polybar, i3blocks) and shell scripts.
- 🎛️ **Zero Dependencies at Runtime:** Pure native Rust utilizing `zbus` and direct sysfs I/O—no heavy external toolkits or runtime interpreters.

---

## Architecture

```
                      +-----------------------------+
                      |   iio-sensor-proxy (D-Bus)  |
                      +--------------+--------------+
                                     | (PropertiesChanged / Polling)
                                     v
+-------------------+      +-------------------+
|  systemd-logind   | ---> |  AlsManager       |
| (PrepareForSleep) |      | (D-Bus listener)  |
+-------------------+      +---------+---------+
                                     |
                                     v
                      +-----------------------------+
                      |   MedianFilter3 (Spikes)    |
                      +--------------+--------------+
                                     |
                                     v
                      +-----------------------------+
                      | Dual Hysteresis & Hold Time |
                      +--------------+--------------+
                                     |
                                     v
                      +-----------------------------+
                      | ClightCurve (Monotonic Lerp)| <--- Manual user inputs
                      +--------------+--------------+      (Self-learning)
                                     |
                                     v
                      +-----------------------------+
                      | Adaptive Smoothstep Engine  |
                      +--------------+--------------+
                                     |
                                     v
                      +-----------------------------+
                      | /sys/class/backlight/<dev>  |
                      +-----------------------------+
```

---

## Prerequisites

- **Linux** with `sysfs` backlight support (`/sys/class/backlight/`).
- **`iio-sensor-proxy`** running and managing your ambient light sensor (standard on Fedora, Ubuntu, Arch, Debian, openSUSE).
  ```bash
  systemctl status iio-sensor-proxy
  # Verify reading:
  monitor-sensor
  ```
- **Rust toolchain** (MSRV: 1.85 / 2024 edition) for compilation.

---

## Installation

### 1. Building from Source

```bash
git clone https://github.com/semyon0101/als-daemon.git kirialsd
cd kirialsd

# Build optimized release binary
cargo build --release

# Install binary to ~/.cargo/bin or /usr/local/bin
install -Dm755 target/release/kirialsd ~/.cargo/bin/kirialsd
```

### 2. Permissions & Backlight Access

- **Standard Linux with systemd (Default):**  
  **No extra permissions, root access, or `video` group membership are required!**  
  `kirialsd` automatically controls backlight brightness out-of-the-box through the unprivileged `org.freedesktop.login1.Session.SetBrightness` D-Bus API provided by `systemd-logind` for your active user session.

- **Optional Fallback (Non-systemd / minimal inits / elogind):**  
  If your system runs without `systemd-logind`, you can configure direct `sysfs` write access via a `udev` rule:
  ```bash
  sudo tee /etc/udev/rules.d/90-backlight.rules << 'EOF'
  ACTION=="add", SUBSYSTEM=="backlight", RUN+="/bin/chgrp video /sys/class/backlight/%k/brightness", RUN+="/bin/chmod g+w /sys/class/backlight/%k/brightness"
  EOF

  sudo udevadm control --reload-rules && sudo udevadm trigger
  sudo usermod -aG video "$USER"
  ```

### 3. Running as a Systemd Service

A preconfigured systemd user service unit is included in the repository:

```bash
# Copy systemd unit to user units directory
mkdir -p ~/.config/systemd/user/
cp kirialsd.service ~/.config/systemd/user/

# Reload systemd user daemon and enable service
systemctl --user daemon-reload
systemctl --user enable --now kirialsd.service
```

Check the service status and logs:
```bash
systemctl --user status kirialsd
journalctl --user -u kirialsd -f
```

---

## Command-Line Options

```
kirialsd [OPTIONS]
```

| Option | Description |
|---|---|
| `-c, --config <PATH>` | Custom configuration file path (default: `~/.config/kirialsd/config.toml`) |
| `-s, --status, --get-lux` | **One-shot status mode**: outputs JSON with current lux, target %, and hardware values, then exits |
| `-v` | Enable debug logging output |
| `-vv, --trace` | Enable trace logging output (logs all sensor events & transition ticks) |
| `-q, --quiet` | Suppress all regular log output |
| `--dry-run` | Calculate adjustments without writing to `/sys/class/backlight` |
| `-h, --help` | Display command-line help message |
| `-V, --version` | Display version information |

### Example One-Shot Output:
```bash
kirialsd -s
```
```json
{
  "device": "intel_backlight",
  "lux": 22.0,
  "current_brightness_percent": 22,
  "target_brightness_percent": 11,
  "hw_brightness": 107,
  "hw_max_brightness": 496
}
```

---

## Runtime Signals (IPC)

Control the running daemon on the fly using standard POSIX signals without restarting:

| Signal | Action | Command Example |
|---|---|---|
| `SIGUSR1` | Toggle **Curve Learning** on/off | `pkill -USR1 kirialsd` |
| `SIGUSR2` | Toggle **Auto-Adjustment** on/off (pause/resume) | `pkill -USR2 kirialsd` |
| `SIGHUP` | **Reload configuration** from disk | `pkill -HUP kirialsd` |
| `SIGINT` / `SIGTERM` | Clean shutdown & release D-Bus ALS sensor | `systemctl --user stop kirialsd` |

---

## Configuration Reference

Default path: `~/.config/kirialsd/config.toml`.  
If this file does not exist, `kirialsd` creates it automatically with optimal defaults upon first launch.

```toml
# Reference ambient light points (in lux) used to construct the interpolation curve.
# Covers pitch-black conditions (0 lux) through direct daylight (1000 lux).
lux_points = [0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0]

# Maximum lux on the calibration curve.
# Optional: if omitted, automatically uses the last element of lux_points.
# max_lux = 1000.0

# Relative hysteresis threshold (0.15 = 15%).
# Lux change relative to current active target must exceed this ratio to trigger adjustment.
lux_hysteresis_ratio = 0.15

# Absolute hysteresis threshold (in lux).
# Prevents micro-oscillations and photodiode ADC noise in low-light environments.
lux_hysteresis_abs = 5.0

# Hold delay in milliseconds.
# Ambient light must hold beyond the hysteresis threshold continuously for this duration
# before adjusting display brightness (filters out shadows from walking past).
auto_adjust_delay_ms = 1000

# Total duration of the smoothstep transition animation in milliseconds.
transition_duration_ms = 1500

# Minimum transition duration in milliseconds when adaptive transition is enabled.
min_transition_duration_ms = 200

# Adaptively scale transition time based on step magnitude.
adaptive_transition = true

# Minimum allowed screen brightness percentage (0.0 - 100.0).
# Prevents screen from becoming pitch black.
min_brightness_percent = 2.0

# Maximum allowed screen brightness percentage (0.0 - 100.0).
max_brightness_percent = 100.0

# Minimum difference (5%) between expected curve brightness and manual adjustment
# required to register a user calibration point.
shutter_threshold = 0.05

# Enable curve learning on daemon startup.
default_learning = true

# Enable automatic brightness adjustments on daemon startup.
default_adjust_enabled = true

# Specific backlight controller name in /sys/class/backlight.
# Optional: if omitted, automatically discovers the best GPU controller.
# backlight_device = "intel_backlight"

# Periodic sensor polling interval in milliseconds.
# Set to -1 to disable active polling and rely solely on D-Bus PropertiesChanged signals.
poll_interval_ms = 1000
```

---

## Self-Learning Calibration

`kirialsd` implements adaptive curve learning based on Clight's piecewise linear interpolation model:

1. **How it learns:** Whenever you manually adjust your brightness (e.g. using brightness keys or a tray applet) by more than `shutter_threshold` (default: 5%), `kirialsd` registers your preferred brightness for the current ambient lux level.
2. **Proportional warping:** It adjusts neighboring reference points using a localized triangular weighting function.
3. **Strict monotonicity:** The daemon enforces $y_0 \le y_1 \le \dots \le y_n$. Lowering brightness at low lux will never cause the curve to invert or create illogical dips at higher lux.
4. **Persistence:** Calibrated Y-values are stored in:
   ```
   ~/.local/state/kirialsd/curve_y.conf
   ```
   To reset the curve back to factory defaults at any time:
   ```bash
   rm ~/.local/state/kirialsd/curve_y.conf
   pkill -HUP kirialsd
   ```

---

## Waybar & Status Bar Integration

Because `kirialsd -s` returns structured JSON in milliseconds, you can use it in Waybar or Polybar modules:

### Waybar Module (`~/.config/waybar/config.jsonc`)

```jsonc
"custom/als": {
    "format": "{icon} {percentage}%",
    "format-icons": ["󰛩", "󱩎", "󱩏", "󱩐", "󱩑", "󱩒", "󱩓", "󱩔", "󱩕", "󱩖", "󰛨"],
    "return-type": "json",
    "exec": "kirialsd -s | jq --unbuffered --compact-output '{percentage: .current_brightness_percent, tooltip: \"Sensor: \\(.lux) lx\\nTarget: \\(.target_brightness_percent)%\"}'",
    "interval": 2,
    "on-click": "pkill -USR2 kirialsd",       // Toggle auto-adjust on click
    "on-click-right": "pkill -USR1 kirialsd"  // Toggle learning on right click
}
```

---

## Troubleshooting

### Sensor not detected / `net.hadess.SensorProxy` error
Ensure `iio-sensor-proxy` is installed and running:
```bash
systemctl status iio-sensor-proxy
```
If your laptop hardware uses ACPI or custom IIO drivers, check `dmesg | grep -i iio`.

### Permission denied writing to backlight
Verify that your user is in the `video` group and that udev rules are applied:
```bash
groups | grep video
ls -l /sys/class/backlight/*/brightness
```

### Inspect real-time transitions and sensor logs
Run the daemon manually with trace-level logging enabled:
```bash
kirialsd -vv
```

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
