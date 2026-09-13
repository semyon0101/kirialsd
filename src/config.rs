use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Off = 0,
    Info = 1,
    Debug = 2,
    Trace = 3,
}

impl LogLevel {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "info" => LogLevel::Info,
            "debug" => LogLevel::Debug,
            "trace" => LogLevel::Trace,
            _ => LogLevel::Off,
        }
    }
}

#[macro_export]
macro_rules! log_msg {
    ($level:expr, $current:expr, $($arg:tt)*) => {
        if $current >= $level {
            println!($($arg)*);
        }
    };
}

#[macro_export]
macro_rules! log_err {
    ($level:expr, $current:expr, $($arg:tt)*) => {
        if $current >= $level {
            eprintln!($($arg)*);
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Reference points along the ambient light curve in lux
    #[serde(default = "default_lux_points")]
    pub lux_points: Vec<f64>,

    /// Maximum lux on the calibration curve.
    /// If omitted, automatically defaults to the last element of `lux_points`.
    #[serde(default)]
    pub max_lux: Option<f64>,

    /// Relative hysteresis threshold (e.g. 0.15 = 15% change required to initiate readjustment)
    #[serde(default = "default_lux_hysteresis_ratio")]
    pub lux_hysteresis_ratio: f64,

    /// Absolute hysteresis threshold in lux (e.g. 5.0 lux). Prevents small noise oscillations in the dark.
    #[serde(default = "default_lux_hysteresis_abs")]
    pub lux_hysteresis_abs: f64,

    /// Hold delay in milliseconds that lux must remain beyond the hysteresis band before starting adjustment
    #[serde(default = "default_auto_adjust_delay_ms")]
    pub auto_adjust_delay_ms: u64,

    /// Base / maximum duration of the smoothstep transition animation in milliseconds
    #[serde(default = "default_transition_duration_ms")]
    pub transition_duration_ms: u64,

    /// Minimum transition duration in milliseconds when adaptive scaling is enabled
    #[serde(default = "default_min_transition_duration_ms")]
    pub min_transition_duration_ms: u64,

    /// Whether transition animation duration scales adaptively with the step size
    #[serde(default = "default_true")]
    pub adaptive_transition: bool,

    /// Minimum difference between expected curve brightness and user brightness to trigger learning
    #[serde(default = "default_shutter_threshold")]
    pub shutter_threshold: f64,

    /// Whether learning from user manual adjustments is enabled by default
    #[serde(default = "default_true")]
    pub default_learning: bool,

    /// Whether automatic brightness adjustment is enabled by default
    #[serde(default = "default_true")]
    pub default_adjust_enabled: bool,

    /// Minimum backlight brightness percentage to prevent black screen (e.g. 2.0%)
    #[serde(default = "default_min_brightness")]
    pub min_brightness_percent: f64,

    /// Maximum backlight brightness percentage (e.g. 100.0%)
    #[serde(default = "default_max_brightness")]
    pub max_brightness_percent: f64,

    /// Specific backlight controller name (None = auto-detect best GPU controller)
    #[serde(default)]
    pub backlight_device: Option<String>,

    /// Periodic sensor polling interval in milliseconds (e.g. 1000). Set to -1 to disable active polling and rely solely on D-Bus signals.
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: i64,
}

fn default_lux_points() -> Vec<f64> {
    vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0]
}
fn default_lux_hysteresis_ratio() -> f64 { 0.15 }
fn default_lux_hysteresis_abs() -> f64 { 5.0 }
fn default_auto_adjust_delay_ms() -> u64 { 1000 }
fn default_transition_duration_ms() -> u64 { 1500 }
fn default_min_transition_duration_ms() -> u64 { 200 }
fn default_shutter_threshold() -> f64 { 0.05 }
fn default_true() -> bool { true }
fn default_min_brightness() -> f64 { 2.0 }
fn default_max_brightness() -> f64 { 100.0 }
fn default_poll_interval_ms() -> i64 { 1000 }

impl DaemonConfig {
    /// Returns the configured max_lux, or falls back to the highest value in lux_points
    pub fn effective_max_lux(&self) -> f64 {
        self.max_lux
            .unwrap_or_else(|| *self.lux_points.last().unwrap_or(&1000.0))
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            lux_points: default_lux_points(),
            max_lux: None,
            lux_hysteresis_ratio: default_lux_hysteresis_ratio(),
            lux_hysteresis_abs: default_lux_hysteresis_abs(),
            auto_adjust_delay_ms: default_auto_adjust_delay_ms(),
            transition_duration_ms: default_transition_duration_ms(),
            min_transition_duration_ms: default_min_transition_duration_ms(),
            adaptive_transition: true,
            shutter_threshold: default_shutter_threshold(),
            default_learning: true,
            default_adjust_enabled: true,
            min_brightness_percent: default_min_brightness(),
            max_brightness_percent: default_max_brightness(),
            backlight_device: None,
            poll_interval_ms: default_poll_interval_ms(),
        }
    }
}

pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# ==============================================================================
# kirialsd configuration file
# Ambient Light Sensor Backlight Daemon
# Default location: ~/.config/kirialsd/config.toml
# ==============================================================================

# Reference ambient light points (in lux) used to construct the interpolation curve.
# Default values cover dark rooms (0-15 lx) up to bright daylight (1000 lx).
lux_points = [0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0]

# Maximum lux on the calibration curve.
# Optional: if omitted or commented out, automatically uses the last element of lux_points.
# max_lux = 1000.0

# Relative hysteresis threshold (0.15 = 15%).
# Change in lux relative to current target must exceed this ratio to initiate readjustment.
lux_hysteresis_ratio = 0.15

# Absolute hysteresis threshold (in lux).
# Prevents small sensor noise fluctuations from triggering brightness shifts in low-light.
lux_hysteresis_abs = 5.0

# Hold delay in milliseconds.
# Ambient light must stay past the hysteresis band continuously for this duration
# before the daemon starts adjusting the display (filters out transient shadows).
auto_adjust_delay_ms = 1000

# Total duration of the smoothstep transition animation in milliseconds.
# Represents the base/maximum transition duration for full brightness sweeps.
transition_duration_ms = 1500

# Minimum transition duration in milliseconds when adaptive scaling is enabled.
# Prevents micro-steps (e.g. 1-2 brightness levels) from taking the full transition time.
min_transition_duration_ms = 200

# Whether transition animation duration scales adaptively with the step size.
# True: small brightness adjustments finish swiftly, large adjustments transition smoothly over full duration.
adaptive_transition = true

# Minimum allowed screen brightness percentage (0.0 - 100.0).
# Prevents the screen from going completely pitch-black in dark environments.
min_brightness_percent = 2.0

# Maximum allowed screen brightness percentage (0.0 - 100.0).
max_brightness_percent = 100.0

# Shutter threshold (0.05 = 5%).
# Minimum difference between expected curve brightness and manual user brightness
# required to register a user calibration point.
shutter_threshold = 0.05

# Enable curve learning on daemon startup.
# Can be toggled at runtime by sending SIGUSR1 signal.
default_learning = true

# Enable automatic brightness adjustment on daemon startup.
# Can be toggled at runtime by sending SIGUSR2 signal.
default_adjust_enabled = true

# Specific backlight controller name in /sys/class/backlight (e.g. "intel_backlight", "amdgpu_bl0").
# Optional: if omitted or commented out, auto-detects the best GPU controller.
# backlight_device = "intel_backlight"

# Periodic sensor polling interval in milliseconds (e.g. 1000).
# Set to -1 to disable active periodic polling and rely solely on D-Bus PropertiesChanged signals.
poll_interval_ms = 1000
"#;

impl DaemonConfig {
    pub fn validate(&mut self) {
        self.lux_points.retain(|&x| !x.is_nan() && x >= 0.0);
        self.lux_points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.lux_points.dedup();
        if self.lux_points.len() < 2 {
            self.lux_points = default_lux_points();
        }

        if let Some(m) = self.max_lux {
            if m.is_nan() || m <= 0.0 {
                self.max_lux = None;
            }
        }

        self.lux_hysteresis_ratio = self.lux_hysteresis_ratio.clamp(0.01, 1.0);
        self.lux_hysteresis_abs = self.lux_hysteresis_abs.max(0.0);
        self.auto_adjust_delay_ms = self.auto_adjust_delay_ms.max(50);
        self.transition_duration_ms = self.transition_duration_ms.max(50);
        self.min_transition_duration_ms = self.min_transition_duration_ms.clamp(20, self.transition_duration_ms);
        self.shutter_threshold = self.shutter_threshold.clamp(0.001, 1.0);
        self.min_brightness_percent = self.min_brightness_percent.clamp(0.0, 100.0);
        self.max_brightness_percent = self.max_brightness_percent.clamp(self.min_brightness_percent, 100.0);
    }

    pub fn load_or_create(path: &Path) -> Self {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(path) {
                match toml::from_str::<DaemonConfig>(&content) {
                    Ok(mut cfg) => {
                        cfg.validate();
                        return cfg;
                    }
                    Err(e) => {
                        eprintln!(
                            "[-] Warning: Failed to parse config file {:?}: {}. Using default configuration.",
                            path, e
                        );
                    }
                }
            }
        }
        let mut cfg = Self::default();
        cfg.validate();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, DEFAULT_CONFIG_TEMPLATE);
        cfg
    }
}

pub fn get_default_config_path() -> PathBuf {
    let config_dir = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("kirialsd/config.toml")
}

pub struct CliArgs {
    pub config_path: PathBuf,
    pub log_level: LogLevel,
    pub dry_run: bool,
    pub status_mode: bool,
    pub show_help: bool,
    pub show_version: bool,
}

impl CliArgs {
    pub fn parse() -> Self {
        let mut config_path = get_default_config_path();
        let mut log_level = LogLevel::Info;
        let mut dry_run = false;
        let mut status_mode = false;
        let mut show_help = false;
        let mut show_version = false;

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => show_help = true,
                "-V" | "--version" => show_version = true,
                "-c" | "--config" => {
                    if let Some(p) = args.next() {
                        config_path = PathBuf::from(p);
                    }
                }
                "--dry-run" => dry_run = true,
                "-s" | "--status" | "--get-lux" => status_mode = true,
                "-v" => log_level = LogLevel::Debug,
                "-vv" | "-vvv" | "--trace" => log_level = LogLevel::Trace,
                "-q" | "--quiet" => log_level = LogLevel::Off,
                other => {
                    let level = LogLevel::from_str(other);
                    if level != LogLevel::Off {
                        log_level = level;
                    }
                }
            }
        }

        Self {
            config_path,
            log_level,
            dry_run,
            status_mode,
            show_help,
            show_version,
        }
    }

    pub fn print_help() {
        println!(
            "\
kirialsd - Ambient Light Daemon for Linux

USAGE:
    kirialsd [OPTIONS]

OPTIONS:
    -c, --config <PATH>      Path to configuration file (default: ~/.config/kirialsd/config.toml)
    -v                       Debug logging output
    -vv, --trace             Trace logging output (all sensor events)
    -q, --quiet              Quiet mode (suppress regular output)
    --dry-run                Calculate adjustments without writing to hardware
    -s, --status, --get-lux  One-shot read: print current lux & evaluated brightness, then exit
    -h, --help               Print this help message
    -V, --version            Print daemon version

SIGNALS:
    SIGTERM, SIGINT (Ctrl+C) Clean shutdown and release of ALS sensor
    SIGUSR1                  Toggle curve learning on/off
    SIGUSR2                  Toggle auto-brightness adjustment on/off
    SIGHUP                   Reload configuration file
"
        );
    }
}
