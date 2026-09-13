use kirialsd::curve::ClightCurve;

#[test]
fn test_curve_monotonicity() {
    let lux_points = vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0];
    let y_coords = vec![0.05, 0.08, 0.15, 0.25, 0.35, 0.55, 0.75, 1.00];

    let curve = ClightCurve::new(&lux_points, &y_coords, 1000.0, 0.05);

    // Verify boundary values
    assert!((curve.evaluate(0.0) - 0.05).abs() < 1e-4);
    assert!((curve.evaluate(1000.0) - 1.00).abs() < 1e-4);
    assert!((curve.evaluate(2000.0) - 1.00).abs() < 1e-4); // clamped beyond max_lux
    assert!((curve.evaluate(-50.0) - 0.05).abs() < 1e-4);  // clamped below min_lux

    // Verify global monotonicity at 100 sample points
    let mut prev = curve.evaluate(0.0);
    for i in 1..=100 {
        let lux = (i as f64) * 10.0;
        let br = curve.evaluate(lux);
        assert!(
            br >= prev - 1e-6,
            "Monotonicity violated at {} lux: {} < {}",
            lux,
            br,
            prev
        );
        prev = br;
    }
}

#[test]
fn test_user_calibration_learning_and_proportional_scaling() {
    let lux_points = vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0];
    let y_coords = vec![0.05, 0.08, 0.15, 0.25, 0.35, 0.55, 0.75, 1.00];

    let mut curve = ClightCurve::new(&lux_points, &y_coords, 1000.0, 0.05);

    // Initial value at 70 lux is 0.25
    assert!((curve.evaluate(70.0) - 0.25).abs() < 1e-4);

    // Small change within shutter threshold (0.05) should be ignored
    let modified = curve.on_user_brightness_change(70.0, 0.27);
    assert!(!modified, "Should ignore changes below shutter threshold");

    // Significant user change at 70 lux: user raises brightness to 0.50
    let modified = curve.on_user_brightness_change(70.0, 0.50);
    assert!(modified, "Should accept user calibration");

    // Check that 70 lux is now ~0.50
    assert!((curve.evaluate(70.0) - 0.50).abs() < 1e-3);

    // Check that monotonicity is strictly maintained
    let mut prev = curve.evaluate(0.0);
    for i in 1..=100 {
        let lux = (i as f64) * 10.0;
        let br = curve.evaluate(lux);
        assert!(
            br >= prev - 1e-6,
            "Monotonicity violated after calibration at {} lux: {} < {}",
            lux,
            br,
            prev
        );
        prev = br;
    }

    // Check proportional scaling: lower pivot at 70 lux to 0.10
    let modified_lower = curve.on_user_brightness_change(70.0, 0.10);
    assert!(modified_lower);
    assert!((curve.evaluate(70.0) - 0.10).abs() < 1e-3);

    // Preceding points must NOT be a flat plateau: evaluate(15) must be less than evaluate(35)
    let y15 = curve.evaluate(15.0);
    let y35 = curve.evaluate(35.0);
    let y70 = curve.evaluate(70.0);
    assert!(y15 < y35, "Points should scale proportionally, not flatten: {} < {}", y15, y35);
    assert!(y35 <= y70);
}

#[test]
fn test_out_of_bounds_lux_does_not_corrupt_curve() {
    let lux_points = vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0];
    let y_coords = vec![0.05, 0.08, 0.15, 0.25, 0.35, 0.55, 0.75, 1.00];

    let mut curve = ClightCurve::new(&lux_points, &y_coords, 1000.0, 0.05);

    // Attempt to calibrate at 2500 lux (far beyond 1000 lux max)
    let modified = curve.on_user_brightness_change(2500.0, 0.60);
    assert!(!modified, "Should not calibrate when lux is far outside curve neighborhood");

    // The 1000 lux point must remain unchanged at 1.00
    assert!((curve.evaluate(1000.0) - 1.00).abs() < 1e-4);
}

#[test]
fn test_localized_learning_preserves_unrelated_points() {
    let lux_points = vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0];
    let y_coords = vec![0.05, 0.08, 0.15, 0.25, 0.35, 0.55, 0.75, 1.00];

    let mut curve = ClightCurve::new(&lux_points, &y_coords, 1000.0, 0.05);

    let initial_day_br = curve.evaluate(600.0);
    assert!((initial_day_br - 0.75).abs() < 1e-4);

    // User calibrates night setting at 15 lux: raises 15 lux from 0.08 to 0.14 (delta 0.06 > 0.05 shutter_threshold)
    let modified = curve.on_user_brightness_change(15.0, 0.14);
    assert!(modified);
    assert!((curve.evaluate(15.0) - 0.14).abs() < 1e-3);

    // Crucial check: daytime brightness at 600 lux MUST remain 0.75!
    let day_br_after = curve.evaluate(600.0);
    assert!(
        (day_br_after - initial_day_br).abs() < 1e-4,
        "Calibrating night brightness (15 lx) corrupted daytime brightness (600 lx): {} -> {}",
        initial_day_br, day_br_after
    );

    // Check that max lux point (1000 lx) remains 1.00
    assert!((curve.evaluate(1000.0) - 1.00).abs() < 1e-4);

    // Now calibrate daytime at 600 lux: change from 0.75 to 0.82 (delta 0.07 > 0.05 shutter_threshold)
    let modified_day = curve.on_user_brightness_change(600.0, 0.82);
    assert!(modified_day);
    assert!((curve.evaluate(600.0) - 0.82).abs() < 1e-3);

    // Crucial check: night brightness at 15 lux MUST remain 0.14!
    let night_br_after = curve.evaluate(15.0);
    assert!(
        (night_br_after - 0.14).abs() < 1e-3,
        "Calibrating day brightness (600 lx) corrupted night brightness (15 lx): {} -> {}",
        0.14, night_br_after
    );
}

#[test]
fn test_calibration_at_extreme_boundaries() {
    let lux_points = vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0];
    let y_coords = vec![0.05, 0.08, 0.15, 0.25, 0.35, 0.55, 0.75, 1.00];

    let mut curve = ClightCurve::new(&lux_points, &y_coords, 1000.0, 0.05);

    // User sets 100% brightness (1.0) at 70 lux
    let modified = curve.on_user_brightness_change(70.0, 1.00);
    assert!(modified);

    // All subsequent points must be 1.0 without panic or NaN
    for pt in &curve.points {
        assert!(!pt.y.is_nan(), "Point y must not be NaN");
        assert!(pt.y >= 0.01 && pt.y <= 1.00);
    }
    assert!((curve.evaluate(70.0) - 1.00).abs() < 1e-3);
    assert!((curve.evaluate(100.0) - 1.00).abs() < 1e-3);
    assert!((curve.evaluate(1000.0) - 1.00).abs() < 1e-3);

    // Monotonicity verification
    let mut prev = curve.evaluate(0.0);
    for i in 1..=100 {
        let lux = (i as f64) * 10.0;
        let br = curve.evaluate(lux);
        assert!(!br.is_nan());
        assert!(br >= prev - 1e-6);
        prev = br;
    }

    // Now test lowering to minimum (0.01) at 300 lux
    let modified_low = curve.on_user_brightness_change(300.0, 0.01);
    assert!(modified_low);
    for pt in &curve.points {
        assert!(!pt.y.is_nan());
        assert!(pt.y >= 0.01 && pt.y <= 1.00);
    }

    let mut prev = curve.evaluate(0.0);
    for i in 1..=100 {
        let lux = (i as f64) * 10.0;
        let br = curve.evaluate(lux);
        assert!(!br.is_nan());
        assert!(br >= prev - 1e-6);
        prev = br;
    }
}

#[test]
fn test_downward_learning_clears_elevated_ceiling() {
    // User's exact scenario from real-world curve_y.conf
    let lux_points = vec![0.0, 15.0, 35.0, 70.0, 100.0, 300.0, 600.0, 1000.0];
    let y_coords = vec![0.01, 0.029, 0.436, 0.443, 0.449, 0.487, 0.680, 1.000];

    let mut curve = ClightCurve::new(&lux_points, &y_coords, 1000.0, 0.05);

    // User at ~35 lux lowers brightness from 43.6% to 18%
    let modified = curve.on_user_brightness_change(35.0, 0.18);
    assert!(modified);

    assert!((curve.evaluate(35.0) - 0.18).abs() < 1e-3);

    // Crucial: 70 lux must NOT stay at 0.443! It must be dragged down to clear the ceiling
    let br_70 = curve.evaluate(70.0);
    assert!(
        br_70 < 0.35,
        "Subsequent point at 70 lux was not lowered to clear ceiling: {}",
        br_70
    );

    // Monotonicity must strictly hold
    let mut prev = curve.evaluate(0.0);
    for i in 1..=100 {
        let lux = (i as f64) * 10.0;
        let br = curve.evaluate(lux);
        assert!(
            br >= prev - 1e-6,
            "Monotonicity violated at {} lux: {} < {}",
            lux,
            br,
            prev
        );
        prev = br;
    }

    // Checking small sensor drift: 37 lux should be close to 18%, not jumping to 44%
    let br_37 = curve.evaluate(37.0);
    assert!(
        br_37 < 0.22,
        "Slight lux drift to 37 lx jumped back up to high brightness: {}",
        br_37
    );
}

