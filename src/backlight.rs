use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use zbus::blocking::Connection;

pub struct BacklightDevice {
    pub name: String,
    pub path: PathBuf,
    pub brightness_file: File,
    pub hw_max_brightness: f64,
    pub min_allowed_raw: f64,
    pub max_allowed_raw: f64,
}

impl BacklightDevice {
    pub fn discover(
        preferred_name: Option<&str>,
        min_percent: f64,
        max_percent: f64,
    ) -> Result<Self, String> {
        let bl_dir = Path::new("/sys/class/backlight");
        if !bl_dir.is_dir() {
            return Err("No /sys/class/backlight directory found".to_string());
        }

        let mut candidates = Vec::new();
        let entries = fs::read_dir(bl_dir).map_err(|e| e.to_string())?;

        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            let max_path = path.join("max_brightness");
            let cur_path = path.join("brightness");

            if max_path.is_file() && cur_path.is_file() {
                if let Ok(mut max_file) = File::open(&max_path) {
                    let mut buf = [0u8; 32];
                    if let Some(max_val) = read_sysfs_buffer(&mut max_file, &mut buf) {
                        if max_val > 0 {
                            // Determine device priority
                            let priority = if let Some(pref) = preferred_name {
                                if name == pref {
                                    100
                                } else {
                                    0
                                }
                            } else {
                                // Prefer native GPU backlights over platform / ACPI
                                let dev_type = fs::read_to_string(path.join("type"))
                                    .unwrap_or_default()
                                    .trim()
                                    .to_string();

                                if dev_type == "raw"
                                    || name.contains("intel")
                                    || name.contains("amdgpu")
                                    || name.contains("nvidia")
                                {
                                    30
                                } else if dev_type == "platform" {
                                    20
                                } else {
                                    10 // acpi_video, firmware
                                }
                            };

                            candidates.push((priority, name, path, max_val as f64));
                        }
                    }
                }
            }
        }

        if candidates.is_empty() {
            return Err("No valid backlight controllers found in /sys/class/backlight".to_string());
        }

        // Sort by priority descending
        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        let (_, name, path, hw_max) = candidates.remove(0);

        let cur_file = File::open(path.join("brightness"))
            .map_err(|e| format!("Failed to open brightness file: {}", e))?;

        let min_raw = ((hw_max * (min_percent / 100.0)).round()).max(1.0);
        let max_raw = ((hw_max * (max_percent / 100.0)).round()).min(hw_max);

        Ok(Self {
            name,
            path,
            brightness_file: cur_file,
            hw_max_brightness: hw_max,
            min_allowed_raw: min_raw,
            max_allowed_raw: max_raw,
        })
    }

    pub fn set_brightness(
        &self,
        dbus_conn: Option<&Connection>,
        val: u32,
    ) -> Result<(), String> {
        let clamped = (val as f64)
            .clamp(self.min_allowed_raw, self.max_allowed_raw)
            .round() as u32;

        // 1. Try systemd logind SetBrightness
        if let Some(conn) = dbus_conn {
            let res = conn.call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1/session/auto",
                Some("org.freedesktop.login1.Session"),
                "SetBrightness",
                &("backlight", self.name.as_str(), clamped),
            );
            if res.is_ok() {
                return Ok(());
            }
        }

        // 2. Direct sysfs write fallback (e.g. if running with udev permissions or root)
        let br_path = self.path.join("brightness");
        if let Ok(mut f) = OpenOptions::new().write(true).open(&br_path) {
            let content = format!("{}\n", clamped);
            if f.write_all(content.as_bytes()).is_ok() {
                return Ok(());
            }
        }

        Err(format!("Failed to set brightness for {}", self.name))
    }

    /// Converts a normalized curve brightness fraction [0.0, 1.0] into an integer hardware value
    pub fn curve_to_hw(&self, curve_br: f64) -> i64 {
        let span = (self.max_allowed_raw - self.min_allowed_raw).max(1.0);
        let raw = self.min_allowed_raw + curve_br.clamp(0.0, 1.0) * span;
        (raw.round() as i64).clamp(self.min_allowed_raw as i64, self.max_allowed_raw as i64)
    }

    /// Converts an actual hardware brightness value into a normalized curve fraction [0.0, 1.0]
    pub fn hw_to_curve(&self, hw_br: i64) -> f64 {
        let span = (self.max_allowed_raw - self.min_allowed_raw).max(1.0);
        ((hw_br as f64 - self.min_allowed_raw) / span).clamp(0.0, 1.0)
    }

    /// Converts an actual hardware brightness value into percentage of hardware max [0.0, 100.0]
    pub fn hw_to_percent(&self, hw_val: i64) -> f64 {
        if self.hw_max_brightness > 0.0 {
            ((hw_val.max(0) as f64 / self.hw_max_brightness) * 100.0).round().clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    /// Reads current hardware brightness from sysfs, self-healing the file descriptor if invalidated
    pub fn read_current_hw(&mut self, buf: &mut [u8; 32]) -> Option<i64> {
        if let Some(val) = read_sysfs_buffer(&mut self.brightness_file, buf) {
            return Some(val);
        }
        // If seek/read failed (e.g. fd invalidated across suspend or driver rebind), re-open
        if let Ok(new_file) = File::open(self.path.join("brightness")) {
            self.brightness_file = new_file;
            return read_sysfs_buffer(&mut self.brightness_file, buf);
        }
        None
    }
}

#[inline]
pub fn read_sysfs_buffer(file: &mut File, buf: &mut [u8; 32]) -> Option<i64> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let count = file.read(buf).ok()?;
    if count == 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..count]).ok()?;
    s.trim().parse::<i64>().ok()
}
