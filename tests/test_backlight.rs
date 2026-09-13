#[test]
fn test_smoothstep_lerp() {
    let smoothstep = |t: f64| -> f64 {
        let t_clamped = t.clamp(0.0, 1.0);
        t_clamped * t_clamped * (3.0 - 2.0 * t_clamped)
    };

    assert!((smoothstep(0.0) - 0.0).abs() < 1e-6);
    assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
    assert!((smoothstep(1.0) - 1.0).abs() < 1e-6);

    // Verify S-curve acceleration and deceleration
    assert!(smoothstep(0.25) < 0.25);
    assert!(smoothstep(0.75) > 0.75);
}

#[test]
fn test_brightness_clamping_limits() {
    let hw_max: f64 = 1000.0;
    let min_percent: f64 = 2.0;
    let max_percent: f64 = 80.0; // User capped max brightness to 80%

    let min_raw = ((hw_max * (min_percent / 100.0)).round()).max(1.0);
    let max_raw = ((hw_max * (max_percent / 100.0)).round()).min(hw_max);

    assert_eq!(min_raw, 20.0);
    assert_eq!(max_raw, 800.0);

    // Verify user fraction is calculated against true hw_max
    let actual_hw_br: f64 = 800.0;
    let user_fraction = (actual_hw_br / hw_max).clamp(0.01, 1.0);
    assert_eq!(user_fraction, 0.80); // Correct 80%, NOT 100%

    // Target calculation clamped to [min_raw, max_raw]
    let target_br = 0.95; // Curve requests 95%
    let target_raw = ((target_br * hw_max).round() as i64)
        .clamp(min_raw as i64, max_raw as i64);
    assert_eq!(target_raw, 800); // Clamped cleanly to max_raw
}

#[test]
fn test_backlight_curve_conversions() {
    let dev = kirialsd::backlight::BacklightDevice {
        name: "test_dev".to_string(),
        path: std::path::PathBuf::from("/dev/null"),
        brightness_file: std::fs::File::open("/dev/null").unwrap(),
        hw_max_brightness: 500.0,
        min_allowed_raw: 10.0,
        max_allowed_raw: 500.0,
    };

    // Test 0.0 -> min_allowed_raw
    assert_eq!(dev.curve_to_hw(0.0), 10);
    // Test 1.0 -> max_allowed_raw
    assert_eq!(dev.curve_to_hw(1.0), 500);
    // Test inverse mapping
    assert!((dev.hw_to_curve(10) - 0.0).abs() < 1e-6);
    assert!((dev.hw_to_curve(500) - 1.0).abs() < 1e-6);
    assert!((dev.hw_to_curve(255) - 0.5).abs() < 0.01);

    // Percentage
    assert_eq!(dev.hw_to_percent(250) as i64, 50);
}
