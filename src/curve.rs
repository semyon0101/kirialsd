#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub struct ClightCurve {
    pub points: Vec<Point>,
    pub max_lux: f64,
    pub shutter_threshold: f64,
    slopes: Vec<f64>,
}

impl ClightCurve {
    pub fn new(x_lux: &[f64], y_coords: &[f64], max_lux: f64, shutter_threshold: f64) -> Self {
        assert_eq!(x_lux.len(), y_coords.len());
        assert!(
            x_lux.len() >= 2,
            "Curve requires at least 2 points for interpolation"
        );

        let safe_max = if max_lux > 0.0 { max_lux } else { 1000.0 };
        let mut points = Vec::with_capacity(x_lux.len());
        for (&x, &y) in x_lux.iter().zip(y_coords.iter()) {
            points.push(Point {
                x: (x / safe_max).clamp(0.0, 1.0),
                y: y.clamp(0.01, 1.0),
            });
        }
        points.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

        let mut curve = Self {
            points,
            max_lux: safe_max,
            shutter_threshold,
            slopes: Vec::new(),
        };
        curve.enforce_global_monotonicity();
        curve.recompute();
        curve
    }

    pub fn normalize_lux(&self, lux: f64) -> f64 {
        if lux.is_nan() || self.max_lux <= 0.0 {
            return 0.0;
        }
        (lux / self.max_lux).clamp(0.0, 1.0)
    }

    /// Evaluates target brightness (0.01 .. 1.0) using cubic Hermite spline interpolation.
    pub fn evaluate(&self, lux: f64) -> f64 {
        let x = self.normalize_lux(lux);

        if self.points.is_empty() {
            return 0.05;
        }
        if x <= self.points[0].x {
            return self.points[0].y;
        }
        if x >= self.points[self.points.len() - 1].x {
            return self.points[self.points.len() - 1].y;
        }

        let i = match self
            .points
            .binary_search_by(|p| p.x.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(idx) => return self.points[idx].y,
            Err(idx) => idx.saturating_sub(1),
        };

        let h = self.points[i + 1].x - self.points[i].x;
        if h <= 0.0 {
            return self.points[i].y;
        }

        let t = (x - self.points[i].x) / h;
        let t2 = t * t;
        let t3 = t2 * t;

        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;

        let y = h00 * self.points[i].y
            + h10 * h * self.slopes[i]
            + h01 * self.points[i + 1].y
            + h11 * h * self.slopes[i + 1];

        y.clamp(0.01, 1.0)
    }

    /// Learns a user calibration point if user manually adjusted brightness.
    pub fn on_user_brightness_change(&mut self, lux: f64, user_br: f64) -> bool {
        let n = self.points.len();
        if n < 2 || lux.is_nan() || user_br.is_nan() {
            return false;
        }

        let raw_x = if self.max_lux > 0.0 { lux / self.max_lux } else { 0.0 };
        let current_y = self.evaluate(lux);
        let target_y = user_br.clamp(0.01, 1.0);

        let delta = target_y - current_y;
        if delta.abs() < self.shutter_threshold {
            return false;
        }

        // Neighborhood boundary check: don't calibrate if lux is far outside curve domain
        if raw_x > 1.5 {
            return false;
        }

        // Find closest point to target lux
        let mut pivot = 0;
        let mut min_dist = f64::MAX;
        let norm_x = raw_x.clamp(0.0, 1.0);
        for (idx, pt) in self.points.iter().enumerate() {
            let dist = (pt.x - norm_x).abs();
            if dist < min_dist {
                min_dist = dist;
                pivot = idx;
            }
        }

        let orig_y: Vec<f64> = self.points.iter().map(|p| p.y).collect();

        // Localized kernel in perceptual log-lux space:
        // Compact tricube kernel with radius R = 1.5 in log-lux space.
        // For distance d = |ln(1 + pt_lux) - ln(1 + lux)|:
        // u = d / R; if u < 1.0 { w = (1 - u^2)^2 } else { w = 0.0 }
        let radius = 1.5;
        let log_target_lux = (1.0 + lux.max(0.0)).ln();

        for i in 0..n {
            let pt_lux = self.points[i].x * self.max_lux;
            let log_pt_lux = (1.0 + pt_lux).ln();
            let d = (log_pt_lux - log_target_lux).abs();
            if d < radius {
                let u = d / radius;
                let w = (1.0 - u * u) * (1.0 - u * u);
                self.points[i].y = (self.points[i].y + delta * w).clamp(0.01, 1.0);
            }
        }

        // Ensure pivot is explicitly set to target_y
        self.points[pivot].y = target_y;

        // Direction-aware monotonicity enforcement:
        if delta < 0.0 {
            // Downward adjustment: propagate ceiling backwards to preceding points
            for i in (0..pivot).rev() {
                if self.points[i].y > self.points[i + 1].y {
                    let orig_ratio = if orig_y[i + 1] > 1e-6 {
                        orig_y[i] / orig_y[i + 1]
                    } else {
                        1.0
                    };
                    if orig_ratio < 1.0 {
                        self.points[i].y = (self.points[i + 1].y * orig_ratio).clamp(0.01, self.points[i + 1].y);
                    } else {
                        self.points[i].y = self.points[i + 1].y;
                    }
                }
            }
            // Propagate forward to subsequent points if needed
            for i in (pivot + 1)..n {
                if self.points[i].y < self.points[i - 1].y {
                    self.points[i].y = self.points[i - 1].y;
                }
            }
        } else {
            // Upward adjustment: propagate floor forward to subsequent points
            for i in (pivot + 1)..n {
                if self.points[i].y < self.points[i - 1].y {
                    let prev_headroom = 1.0 - orig_y[i - 1];
                    let curr_headroom = 1.0 - orig_y[i];
                    let orig_headroom_ratio = if prev_headroom > 1e-6 {
                        curr_headroom / prev_headroom
                    } else {
                        1.0
                    };
                    if orig_headroom_ratio < 1.0 {
                        let new_headroom = (1.0 - self.points[i - 1].y) * orig_headroom_ratio;
                        self.points[i].y = (1.0 - new_headroom).clamp(self.points[i - 1].y, 1.0);
                    } else {
                        self.points[i].y = self.points[i - 1].y;
                    }
                }
            }
            // Propagate backward to preceding points if needed
            for i in (0..pivot).rev() {
                if self.points[i].y > self.points[i + 1].y {
                    self.points[i].y = self.points[i + 1].y;
                }
            }
        }

        self.enforce_global_monotonicity();
        self.recompute();
        true
    }

    pub fn enforce_global_monotonicity(&mut self) {
        for i in 1..self.points.len() {
            if self.points[i].y < self.points[i - 1].y {
                self.points[i].y = self.points[i - 1].y;
            }
        }
    }

    pub fn recompute(&mut self) {
        let n = self.points.len();
        if n < 2 {
            self.slopes = vec![0.0; n];
            return;
        }

        let mut deltas = vec![0.0; n - 1];
        let mut slopes = vec![0.0; n];

        for i in 0..(n - 1) {
            let h = self.points[i + 1].x - self.points[i].x;
            deltas[i] = if h > 0.0 {
                (self.points[i + 1].y - self.points[i].y) / h
            } else {
                0.0
            };
        }

        slopes[0] = deltas[0];
        for i in 1..(n - 1) {
            slopes[i] = (deltas[i - 1] + deltas[i]) * 0.5;
        }
        slopes[n - 1] = deltas[n - 2];

        // Fritsch-Carlson monotonicity condition
        for i in 0..(n - 1) {
            if deltas[i].abs() < 1e-9 {
                slopes[i] = 0.0;
                slopes[i + 1] = 0.0;
            } else {
                let alpha = slopes[i] / deltas[i];
                let beta = slopes[i + 1] / deltas[i];
                let dist = alpha * alpha + beta * beta;
                if dist > 9.0 {
                    let tau = 3.0 / dist.sqrt();
                    slopes[i] = tau * alpha * deltas[i];
                    slopes[i + 1] = tau * beta * deltas[i];
                }
            }
        }

        self.slopes = slopes;
    }

    pub fn get_lux_points(&self) -> Vec<f64> {
        self.points.iter().map(|p| (p.x * self.max_lux).round()).collect()
    }

    pub fn get_y_values(&self) -> Vec<f64> {
        self.points.iter().map(|p| p.y).collect()
    }
}
