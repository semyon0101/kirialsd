use kirialsd::als::{AlsManager, DaemonEvent, LuxSource};
use kirialsd::backlight::{read_sysfs_buffer, BacklightDevice};
use kirialsd::config::{CliArgs, DaemonConfig, LogLevel};
use kirialsd::curve::ClightCurve;
use kirialsd::state::StateManager;
use kirialsd::{log_err, log_msg};

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;

// Engine internal tuning constants (not exposed in user config to avoid clutter)
const USER_ADJUST_COOLDOWN: Duration = Duration::from_millis(2000);
const STATE_SAVE_DELAY: Duration = Duration::from_millis(1500);
const MANUAL_DELTA_THRESHOLD_RATIO: f64 = 0.02;

/// 3-sample rolling median filter: eliminates transient photodiode ADC noise and single-frame spikes
/// without creating creeping drift or lagging true ambient step changes.
#[derive(Debug, Clone)]
struct MedianFilter3 {
    buf: [f64; 3],
}

impl MedianFilter3 {
    fn new(initial: f64) -> Self {
        Self {
            buf: [initial; 3],
        }
    }

    fn reset(&mut self, val: f64) {
        self.buf = [val; 3];
    }

    fn update(&mut self, sample: f64) -> f64 {
        self.buf[0] = self.buf[1];
        self.buf[1] = self.buf[2];
        self.buf[2] = sample;

        let mut sorted = self.buf;
        if sorted[0] > sorted[1] {
            sorted.swap(0, 1);
        }
        if sorted[1] > sorted[2] {
            sorted.swap(1, 2);
        }
        if sorted[0] > sorted[1] {
            sorted.swap(0, 1);
        }

        sorted[1]
    }
}

fn main() {
    let args = CliArgs::parse();

    if args.show_help {
        CliArgs::print_help();
        return;
    }

    if args.show_version {
        println!("kirialsd v{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let log_level = args.log_level;
    let mut config = DaemonConfig::load_or_create(&args.config_path);

    // One-shot query / status mode
    if args.status_mode {
        let mut als_mgr = AlsManager::new(LogLevel::Off);
        let maybe_lux = als_mgr.read_live_lux();

        let state_mgr = StateManager::new();
        let y_coords = state_mgr.load_or_init(&config.lux_points);
        let curve = ClightCurve::new(
            &config.lux_points,
            &y_coords,
            config.effective_max_lux(),
            config.shutter_threshold,
        );

        let backlight_info = BacklightDevice::discover(
            config.backlight_device.as_deref(),
            config.min_brightness_percent,
            config.max_brightness_percent,
        );

        let (dev_name, current_hw, hw_max, current_pct) = match backlight_info {
            Ok(mut dev) => {
                let mut buf = [0u8; 32];
                let cur = read_sysfs_buffer(&mut dev.brightness_file, &mut buf)
                    .unwrap_or(dev.hw_max_brightness as i64);
                let pct = if dev.hw_max_brightness > 0.0 {
                    (cur as f64 / dev.hw_max_brightness * 100.0).round()
                } else {
                    0.0
                };
                (dev.name, cur, dev.hw_max_brightness as i64, pct)
            }
            Err(_) => ("unknown".to_string(), 0, 0, 0.0),
        };

        match maybe_lux {
            Some(lux) => {
                let target_pct = (curve.evaluate(lux) * 100.0).round();
                println!(
                    "{{\"device\": \"{}\", \"lux\": {:.1}, \"current_brightness_percent\": {:.0}, \"target_brightness_percent\": {:.0}, \"hw_brightness\": {}, \"hw_max_brightness\": {}}}",
                    dev_name, lux, current_pct, target_pct, current_hw, hw_max
                );
            }
            None => {
                eprintln!("[-] Error: Unable to read ambient light sensor from net.hadess.SensorProxy");
                println!(
                    "{{\"device\": \"{}\", \"lux\": null, \"current_brightness_percent\": {:.0}, \"target_brightness_percent\": null, \"hw_brightness\": {}, \"hw_max_brightness\": {}}}",
                    dev_name, current_pct, current_hw, hw_max
                );
                std::process::exit(1);
            }
        }
        return;
    }

    log_msg!(
        LogLevel::Info,
        log_level,
        "[*] Starting kirialsd v{}...",
        env!("CARGO_PKG_VERSION")
    );

    // Setup OS signal handling (flag starts as false, signal arrival sets flag to true)
    let shutdown = Arc::new(AtomicBool::new(false));
    let sig_usr1 = Arc::new(AtomicBool::new(false));
    let sig_usr2 = Arc::new(AtomicBool::new(false));
    let sig_hup = Arc::new(AtomicBool::new(false));

    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone());
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown.clone());
    let _ = signal_hook::flag::register(signal_hook::consts::SIGUSR1, sig_usr1.clone());
    let _ = signal_hook::flag::register(signal_hook::consts::SIGUSR2, sig_usr2.clone());
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, sig_hup.clone());

    // Connect to system D-Bus for control calls (SetBrightness)
    let dbus_conn = Connection::system().ok();

    // Discover display backlight device
    let mut backlight = match BacklightDevice::discover(
        config.backlight_device.as_deref(),
        config.min_brightness_percent,
        config.max_brightness_percent,
    ) {
        Ok(dev) => dev,
        Err(e) => {
            log_err!(LogLevel::Info, log_level, "[-] Fatal: {}", e);
            return;
        }
    };

    log_msg!(
        LogLevel::Info,
        log_level,
        "[+] Backlight controller: '{}' (hw max: {:.0}, clamp: {:.0}-{:.0})",
        backlight.name,
        backlight.hw_max_brightness,
        backlight.min_allowed_raw,
        backlight.max_allowed_raw
    );

    // State and curve setup
    let state_mgr = StateManager::new();
    let y_coords = state_mgr.load_or_init(&config.lux_points);
    let mut curve = ClightCurve::new(
        &config.lux_points,
        &y_coords,
        config.effective_max_lux(),
        config.shutter_threshold,
    );

    log_msg!(
        LogLevel::Info,
        log_level,
        "[*] Config: {:?} (points: {}, max: {:.0} lx)",
        args.config_path,
        config.lux_points.len(),
        config.effective_max_lux()
    );
    log_msg!(
        LogLevel::Info,
        log_level,
        "[*] State file: {:?} ({} calibrated Y-points)",
        state_mgr.state_path,
        y_coords.len()
    );

    let mut learning_enabled = config.default_learning;
    let mut adjust_enabled = config.default_adjust_enabled;

    // Ambient light sensor manager (SensorProxy via D-Bus)
    let mut als_mgr = AlsManager::new(log_level);
    let mut current_lux = als_mgr.get_initial_lux().unwrap_or(100.0);
    let mut active_target_lux = current_lux;
    let mut current_target_br = curve.evaluate(current_lux);

    let poll_interval = Arc::new(AtomicI64::new(config.poll_interval_ms));

    // Event channel for D-Bus signals (SensorProxy PropertiesChanged & login1 PrepareForSleep)
    let (event_tx, event_rx) = mpsc::channel();
    als_mgr.start_listeners(
        event_tx,
        shutdown.clone(),
        poll_interval.clone(),
        log_level,
    );

    let mut is_suspended = false;
    let mut sysfs_buffer = [0u8; 32];
    let mut current_hw_br = backlight.read_current_hw(&mut sysfs_buffer)
        .unwrap_or(backlight.hw_max_brightness as i64);
    let mut last_written_br = current_hw_br;

    let mut last_user_action = Instant::now() - Duration::from_secs(10);
    let mut pending_save = false;

    // Smooth transition tracking (smoothstep LERP)
    let mut anim_start_time = Instant::now();
    let mut anim_start_br = current_hw_br;
    let mut anim_target_br = backlight.curve_to_hw(current_target_br);
    let mut is_animating = current_hw_br != anim_target_br;
    let mut last_anim_frame = Instant::now() - Duration::from_millis(25);

    // Temporal hysteresis tracking
    let mut hold_timer: Option<Instant> = None;
    let mut hold_lux: f64 = current_lux;
    let mut lux_filter = MedianFilter3::new(current_lux);

    let manual_delta_threshold =
        ((backlight.hw_max_brightness * MANUAL_DELTA_THRESHOLD_RATIO).round() as i64).max(1);

    let initial_hw_target = backlight.curve_to_hw(current_target_br);
    let initial_pct = backlight.hw_to_percent(initial_hw_target) as i64;
    log_msg!(
        LogLevel::Info,
        log_level,
        "[+] kirialsd active. Target: {}% ({}/{}) | Initial Lux: {:.1} | Smoothing: {}ms (adaptive: {}) | Polling: {}ms",
        initial_pct,
        initial_hw_target,
        backlight.hw_max_brightness as i64,
        current_lux,
        config.transition_duration_ms,
        config.adaptive_transition,
        config.poll_interval_ms
    );

    while !shutdown.load(Ordering::Relaxed) {
        // Signal: Toggle learning (SIGUSR1)
        if sig_usr1.swap(false, Ordering::Relaxed) {
            learning_enabled = !learning_enabled;
            log_msg!(
                LogLevel::Info,
                log_level,
                "[Signal] SIGUSR1: Curve learning -> {}",
                learning_enabled
            );
        }

        // Signal: Toggle auto-adjustment (SIGUSR2)
        if sig_usr2.swap(false, Ordering::Relaxed) {
            adjust_enabled = !adjust_enabled;
            log_msg!(
                LogLevel::Info,
                log_level,
                "[Signal] SIGUSR2: Auto-adjustment -> {}",
                adjust_enabled
            );
        }

        // Signal: Reload config (SIGHUP)
        if sig_hup.swap(false, Ordering::Relaxed) {
            config = DaemonConfig::load_or_create(&args.config_path);

            backlight.min_allowed_raw = ((backlight.hw_max_brightness * (config.min_brightness_percent / 100.0)).round()).max(1.0);
            backlight.max_allowed_raw = ((backlight.hw_max_brightness * (config.max_brightness_percent / 100.0)).round()).min(backlight.hw_max_brightness);

            let y_coords = state_mgr.load_or_init(&config.lux_points);
            curve = ClightCurve::new(
                &config.lux_points,
                &y_coords,
                config.effective_max_lux(),
                config.shutter_threshold,
            );

            log_msg!(
                LogLevel::Info,
                log_level,
                "[Signal] SIGHUP: Configuration reloaded from {:?}",
                args.config_path
            );
            poll_interval.store(config.poll_interval_ms, Ordering::Relaxed);
            // Re-evaluate target with reloaded config settings
            current_target_br = curve.evaluate(current_lux);
            anim_target_br = backlight.curve_to_hw(current_target_br);
            anim_start_time = Instant::now();
            anim_start_br = current_hw_br;
            is_animating = current_hw_br != anim_target_br;
            last_anim_frame = Instant::now() - Duration::from_millis(25);
        }

        // Determine maximum wait duration before next event or timer tick
        let mut wait_timeout = if is_animating {
            let time_since_frame = last_anim_frame.elapsed();
            Duration::from_millis(25).saturating_sub(time_since_frame)
        } else {
            Duration::from_millis(100)
        };

        if let Some(start_time) = hold_timer {
            let hold_duration = Duration::from_millis(config.auto_adjust_delay_ms);
            let elapsed = start_time.elapsed();
            let remaining = hold_duration.saturating_sub(elapsed);
            wait_timeout = wait_timeout.min(remaining);
        }

        if pending_save {
            let elapsed = last_user_action.elapsed();
            let remaining = STATE_SAVE_DELAY.saturating_sub(elapsed);
            wait_timeout = wait_timeout.min(remaining);
        }

        // 1. Wait for asynchronous incoming events or timer tick (zero heap allocations)
        let mut current_ev = match event_rx.recv_timeout(wait_timeout) {
            Ok(ev) => Some(ev),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        while let Some(event) = current_ev.take().or_else(|| event_rx.try_recv().ok()) {
            match event {
                DaemonEvent::PrepareForSleep { going_to_sleep, resume_lux } => {
                    if going_to_sleep {
                        is_suspended = true;
                        log_msg!(
                            LogLevel::Info,
                            log_level,
                            "[Power] Suspending system: saving pending state and pausing adjustments"
                        );
                        if pending_save {
                            let _ = state_mgr.save(&curve.get_lux_points(), &curve.get_y_values());
                            pending_save = false;
                        }
                        is_animating = false;
                        hold_timer = None;
                    } else {
                        is_suspended = false;
                        log_msg!(
                            LogLevel::Info,
                            log_level,
                            "[Power] Resumed from sleep: recalibrating backlight"
                        );
                        // Reset last_user_action on resume so immediate sensor update doesn't race
                        last_user_action = Instant::now();

                        if let Some(actual_hw) = backlight.read_current_hw(&mut sysfs_buffer) {
                            current_hw_br = actual_hw;
                            last_written_br = actual_hw;
                        }

                        if let Some(l) = resume_lux {
                            lux_filter.reset(l);
                            current_lux = l;
                        }

                        active_target_lux = current_lux;
                        current_target_br = curve.evaluate(current_lux);
                        anim_start_time = Instant::now();
                        anim_start_br = current_hw_br;
                        anim_target_br = backlight.curve_to_hw(current_target_br);
                        is_animating = current_hw_br != anim_target_br;
                        hold_timer = None;
                    }
                }
                DaemonEvent::LuxChanged { lux: event_lux, source } => {
                    let filtered_lux = lux_filter.update(event_lux);
                    current_lux = filtered_lux;

                    let tag = match source {
                        LuxSource::Signal => "[ALS Signal]",
                        LuxSource::Poll => "[ALS Poll]",
                    };

                    log_msg!(
                        LogLevel::Trace,
                        log_level,
                        "{} {:.1} lx (filtered: {:.1} lx)",
                        tag,
                        event_lux,
                        current_lux
                    );

                    if !is_suspended && adjust_enabled && last_user_action.elapsed() >= USER_ADJUST_COOLDOWN {
                        let lux_delta = (current_lux - active_target_lux).abs();
                        let relative_delta = lux_delta / active_target_lux.max(1.0);

                        if lux_delta > config.lux_hysteresis_abs
                            && relative_delta >= config.lux_hysteresis_ratio
                        {
                            let needs_reset = match hold_timer {
                                None => true,
                                Some(_) => {
                                    let drift = (current_lux - hold_lux).abs();
                                    let drift_ratio = drift / hold_lux.max(1.0);
                                    drift > config.lux_hysteresis_abs
                                        && drift_ratio >= config.lux_hysteresis_ratio
                                }
                            };

                            if needs_reset {
                                hold_timer = Some(Instant::now());
                                hold_lux = current_lux;
                                log_msg!(
                                    LogLevel::Trace,
                                    log_level,
                                    "[Delay] Lux threshold triggered/drifted ({:.1} lx), hold timer reset ({} ms)",
                                    hold_lux,
                                    config.auto_adjust_delay_ms
                                );
                            }
                        } else if hold_timer.is_some() {
                            log_msg!(
                                LogLevel::Trace,
                                log_level,
                                "[Delay] Lux reverted into active tolerance band, hold timer cancelled"
                            );
                            hold_timer = None;
                        }
                    }
                }
            }
        }

        // Check hold timer deadline for ambient light adjustment
        if !is_suspended {
            if let Some(start_time) = hold_timer {
                if start_time.elapsed() >= Duration::from_millis(config.auto_adjust_delay_ms) {
                    let next_target_br = curve.evaluate(current_lux);
                    let prev_hw = backlight.curve_to_hw(current_target_br);
                    let next_hw = backlight.curve_to_hw(next_target_br);
                    let prev_pct = backlight.hw_to_percent(prev_hw) as i64;
                    let next_pct = backlight.hw_to_percent(next_hw) as i64;

                    if prev_hw != next_hw {
                        log_msg!(
                            LogLevel::Debug,
                            log_level,
                            "[Auto Adjust] Lux held for {}ms: {:.1} -> {:.1} | Target: {}% ({}/{}) -> {}% ({}/{})",
                            config.auto_adjust_delay_ms,
                            active_target_lux,
                            current_lux,
                            prev_pct,
                            prev_hw,
                            backlight.hw_max_brightness as i64,
                            next_pct,
                            next_hw,
                            backlight.hw_max_brightness as i64,
                        );
                    }

                    active_target_lux = current_lux;
                    current_target_br = next_target_br;
                    hold_timer = None;
                }
            }
        }

        // 2. User manual adjustment detection
        // Compare against last_written_br so that manual adjustments during transitions are NOT missed
        if !is_suspended {
            if let Some(actual_hw_br) =
                backlight.read_current_hw(&mut sysfs_buffer)
            {
                let diff_from_expected = (actual_hw_br - last_written_br).abs();

                if diff_from_expected >= manual_delta_threshold {
                    // User pressed hardware brightness keys: cancel any active transition immediately
                    is_animating = false;
                    hold_timer = None;
                    last_written_br = actual_hw_br;
                    current_hw_br = actual_hw_br;
                    anim_start_br = actual_hw_br;
                    anim_target_br = actual_hw_br;
                    last_user_action = Instant::now();
                    lux_filter.reset(current_lux);

                    // Compute user fraction mapped onto the active curve range [min_allowed_raw, max_allowed_raw]
                    let user_fraction = backlight.hw_to_curve(actual_hw_br);
                    let current_curve_val = curve.evaluate(current_lux);

                    if (user_fraction - current_curve_val).abs() >= config.shutter_threshold {
                        let user_pct = backlight.hw_to_percent(actual_hw_br) as i64;
                        log_msg!(
                            LogLevel::Info,
                            log_level,
                            "[!] User calibration: {}% ({}/{}) at {:.1} lux",
                            user_pct,
                            actual_hw_br,
                            backlight.hw_max_brightness as i64,
                            current_lux
                        );

                        if learning_enabled
                            && curve.on_user_brightness_change(current_lux, user_fraction)
                        {
                            pending_save = true;
                        }
                    }

                    current_target_br = user_fraction;
                    active_target_lux = current_lux;
                } else {
                    current_hw_br = actual_hw_br;
                }
            }
        }

        // Deferred state file write (save after user stops adjusting for STATE_SAVE_DELAY)
        if pending_save && last_user_action.elapsed() >= STATE_SAVE_DELAY {
            log_msg!(LogLevel::Info, log_level, "[+] State saved to curve_y.conf");
            let _ = state_mgr.save(&curve.get_lux_points(), &curve.get_y_values());
            pending_save = false;
        }

        // 3. Time-based smooth transition (Smoothstep LERP)
        let target_raw = backlight.curve_to_hw(current_target_br);

        if !is_suspended && target_raw != anim_target_br {
            anim_start_time = Instant::now();
            anim_start_br = current_hw_br;
            anim_target_br = target_raw;
            is_animating = true;
        }

        if !is_suspended && is_animating {
            let time_since_frame = last_anim_frame.elapsed();
            let elapsed_ms = anim_start_time.elapsed().as_millis() as f64;

            let total_duration = if config.adaptive_transition {
                let span = (backlight.max_allowed_raw - backlight.min_allowed_raw).max(1.0);
                let delta = ((anim_target_br - anim_start_br).abs() as f64 / span).clamp(0.0, 1.0);
                let min_d = config.min_transition_duration_ms.max(50) as f64;
                let max_d = config.transition_duration_ms.max(min_d as u64) as f64;
                // Scale smoothly with the step size (square root of normalized delta)
                (min_d + (max_d - min_d) * delta.sqrt()).round()
            } else {
                config.transition_duration_ms.max(50) as f64
            };

            let progress = (elapsed_ms / total_duration).clamp(0.0, 1.0);

            if time_since_frame >= Duration::from_millis(25) || progress >= 1.0 {
                last_anim_frame = Instant::now();

                // Smoothstep curve: 3t^2 - 2t^3
                let eased = progress * progress * (3.0 - 2.0 * progress);

                let next_br =
                    (anim_start_br as f64 + (anim_target_br - anim_start_br) as f64 * eased).round()
                        as i64;

                if next_br != current_hw_br {
                    if !args.dry_run {
                        if let Err(e) =
                            backlight.set_brightness(dbus_conn.as_ref(), next_br as u32)
                        {
                            log_err!(
                                LogLevel::Info,
                                log_level,
                                "[-] Failed to set brightness: {}",
                                e
                            );
                        } else {
                            last_written_br = next_br;
                            current_hw_br = next_br;

                            log_msg!(
                                LogLevel::Trace,
                                log_level,
                                "[Transition] {:.0}% -> {}/{}",
                                progress * 100.0,
                                current_hw_br,
                                anim_target_br
                            );
                        }
                    } else {
                        last_written_br = next_br;
                        current_hw_br = next_br;

                        log_msg!(
                            LogLevel::Trace,
                            log_level,
                            "[Transition] {:.0}% -> {}/{}",
                            progress * 100.0,
                            current_hw_br,
                            anim_target_br
                        );
                    }
                }

                if progress >= 1.0 {
                    is_animating = false;
                    let final_pct = backlight.hw_to_percent(current_hw_br) as i64;
                    log_msg!(
                        LogLevel::Info,
                        log_level,
                        "[Display] Settled at {}% ({}/{}) | Sensor: {:.1} lux",
                        final_pct,
                        current_hw_br,
                        backlight.hw_max_brightness as i64,
                        current_lux
                    );
                }
            }
        }
    }

    log_msg!(
        LogLevel::Info,
        log_level,
        "[*] Shutting down kirialsd gracefully..."
    );
    als_mgr.shutdown(log_level);
    if pending_save {
        let _ = state_mgr.save(&curve.get_lux_points(), &curve.get_y_values());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_median_filter_noise_rejection() {
        let mut filter = MedianFilter3::new(30.0);

        // Constant input
        assert_eq!(filter.update(30.0), 30.0);

        // Single sample spike (noise)
        assert_eq!(filter.update(100.0), 30.0);

        // Return to normal
        assert_eq!(filter.update(30.0), 30.0);

        // Single sample dip (noise)
        assert_eq!(filter.update(0.0), 30.0);

        // Return to normal
        assert_eq!(filter.update(30.0), 30.0);
    }

    #[test]
    fn test_median_filter_step_transition() {
        let mut filter = MedianFilter3::new(30.0);

        // Step change from 30.0 to 10.0
        // Sample 1: [30, 30, 10] -> median 30.0 (debounces single-sample glitch)
        assert_eq!(filter.update(10.0), 30.0);

        // Sample 2: [30, 10, 10] -> median 10.0 (instant confirmation)
        assert_eq!(filter.update(10.0), 10.0);

        // Sample 3: [10, 10, 10] -> median 10.0
        assert_eq!(filter.update(10.0), 10.0);
    }
}
