use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::curve::ClightCurve;

pub struct StateManager {
    pub state_path: PathBuf,
}

impl Default for StateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StateManager {
    pub fn new() -> Self {
        let state_dir = env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local/state")
            });
        Self {
            state_path: state_dir.join("kirialsd/curve_y.conf"),
        }
    }

    pub fn load_or_init(&self, lux_points: &[f64]) -> Vec<f64> {
        let safe_count = lux_points.len().max(2);
        if let Ok(content) = fs::read_to_string(&self.state_path) {
            let mut parsed_points: Vec<(f64, f64)> = Vec::new();
            let mut y_only_values: Vec<f64> = Vec::new();

            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let (Ok(lux), Ok(y)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                        if !lux.is_nan() && !y.is_nan() {
                            parsed_points.push((lux.max(0.0), y.clamp(0.01, 1.0)));
                        }
                    }
                } else if parts.len() == 1 {
                    if let Ok(y) = parts[0].parse::<f64>() {
                        if !y.is_nan() {
                            y_only_values.push(y.clamp(0.01, 1.0));
                        }
                    }
                }
            }

            // 1. If modern (lux, y) pairs are stored in the state file
            if !parsed_points.is_empty() {
                let same_len = parsed_points.len() == lux_points.len();
                let same_lux = same_len
                    && parsed_points
                        .iter()
                        .zip(lux_points.iter())
                        .all(|(p, &l)| (p.0 - l).abs() < 0.01);

                if same_lux {
                    return parsed_points.into_iter().map(|p| p.1).collect();
                }

                // Resample from the true historical (lux, y) coordinates
                let (old_lux, old_y): (Vec<f64>, Vec<f64>) = parsed_points.into_iter().unzip();
                let max_lux = *lux_points.last().unwrap_or(&1000.0);
                let old_curve = ClightCurve::new(&old_lux, &old_y, max_lux, 0.05);
                let resampled: Vec<f64> = lux_points
                    .iter()
                    .map(|&lux| {
                        let y = old_curve.evaluate(lux);
                        (y * 1000.0).round() / 1000.0
                    })
                    .collect();

                let _ = self.save(lux_points, &resampled);
                return resampled;
            }

            // 2. Backwards compatibility: legacy files storing only Y coordinates
            if !y_only_values.is_empty() {
                if y_only_values.len() == safe_count {
                    let _ = self.save(lux_points, &y_only_values);
                    return y_only_values;
                }

                // Resample legacy points using logarithmic lux distribution
                let max_lux = *lux_points.last().unwrap_or(&1000.0);
                let old_count = y_only_values.len();
                let log_max = (1.0 + max_lux).ln();
                let old_lux: Vec<f64> = (0..old_count)
                    .map(|i| {
                        let factor = i as f64 / (old_count - 1) as f64;
                        (factor * log_max).exp() - 1.0
                    })
                    .collect();

                let old_curve = ClightCurve::new(&old_lux, &y_only_values, max_lux, 0.05);
                let resampled: Vec<f64> = lux_points
                    .iter()
                    .map(|&lux| {
                        let y = old_curve.evaluate(lux);
                        (y * 1000.0).round() / 1000.0
                    })
                    .collect();

                let _ = self.save(lux_points, &resampled);
                return resampled;
            }
        }

        let defaults = Self::generate_default_y(safe_count);
        let _ = self.save(lux_points, &defaults);
        defaults
    }

    fn generate_default_y(count: usize) -> Vec<f64> {
        if count == 0 {
            return vec![0.05, 1.0];
        }
        if count == 1 {
            return vec![0.5];
        }
        let mut res = Vec::with_capacity(count);
        let min_br = 0.05;
        let max_br = 1.00;
        for i in 0..count {
            let factor = i as f64 / (count - 1) as f64;
            let y = min_br + (max_br - min_br) * factor.powf(1.8);
            res.push((y * 1000.0).round() / 1000.0);
        }
        res
    }

    pub fn save(&self, lux_points: &[f64], y_values: &[f64]) -> std::io::Result<()> {
        let parent = self.state_path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let tmp_path = self.state_path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;

        writeln!(
            file,
            "# kirialsd calibrated curve: <lux> <brightness_fraction (0.01-1.00)>"
        )?;
        for (lux, y) in lux_points.iter().zip(y_values.iter()) {
            writeln!(file, "{:.1} {:.4}", lux, y)?;
        }
        file.flush()?;
        drop(file);

        fs::rename(tmp_path, &self.state_path)
    }
}
