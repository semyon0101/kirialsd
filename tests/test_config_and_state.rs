use kirialsd::config::DaemonConfig;
use kirialsd::state::StateManager;
use std::fs;

#[test]
fn test_config_defaults_and_serialization() {
    let cfg = DaemonConfig::default();
    assert_eq!(cfg.effective_max_lux(), 1000.0);
    assert_eq!(cfg.lux_points.len(), 8);
    assert_eq!(cfg.min_brightness_percent, 2.0);
    assert_eq!(cfg.max_brightness_percent, 100.0);
    assert_eq!(cfg.poll_interval_ms, 1000);
    assert!(cfg.default_learning);
    assert!(cfg.default_adjust_enabled);

    let toml_str = toml::to_string_pretty(&cfg).expect("Serialize to TOML");
    let parsed: DaemonConfig = toml::from_str(&toml_str).expect("Deserialize from TOML");

    assert_eq!(parsed.effective_max_lux(), cfg.effective_max_lux());
    assert_eq!(parsed.lux_points, cfg.lux_points);
    assert_eq!(parsed.lux_hysteresis_ratio, cfg.lux_hysteresis_ratio);
    assert_eq!(parsed.auto_adjust_delay_ms, cfg.auto_adjust_delay_ms);
    assert_eq!(parsed.poll_interval_ms, 1000);

    // Test disabling poll with -1
    let disabled_toml = toml_str.replace("poll_interval_ms = 1000", "poll_interval_ms = -1");
    let parsed_disabled: DaemonConfig = toml::from_str(&disabled_toml).expect("Deserialize disabled poll");
    assert_eq!(parsed_disabled.poll_interval_ms, -1);
}

#[test]
fn test_config_load_or_create() {
    let tmp_path = std::env::temp_dir().join("kirialsd_test_cfg").join("config.toml");
    let _ = fs::remove_file(&tmp_path);

    // Should create file with default config
    let cfg = DaemonConfig::load_or_create(&tmp_path);
    assert!(tmp_path.exists());
    assert_eq!(cfg.effective_max_lux(), 1000.0);

    // Modify a field in file and reload
    let mut modified_toml = fs::read_to_string(&tmp_path).expect("Read config");
    modified_toml = modified_toml.replace("transition_duration_ms = 1500", "transition_duration_ms = 3000");
    fs::write(&tmp_path, modified_toml).expect("Write modified config");

    let reloaded = DaemonConfig::load_or_create(&tmp_path);
    assert_eq!(reloaded.transition_duration_ms, 3000);

    let _ = fs::remove_file(&tmp_path);
}

#[test]
fn test_state_saving_and_loading() {
    let tmp_dir = std::env::temp_dir().join("kirialsd_test_state");
    let _ = fs::create_dir_all(&tmp_dir);
    let state_file = tmp_dir.join("curve_y.conf");

    // Remove if previously existed
    let _ = fs::remove_file(&state_file);

    let state_mgr = StateManager {
        state_path: state_file.clone(),
    };

    let lux_points_8 = vec![0.0, 10.0, 30.0, 70.0, 150.0, 300.0, 600.0, 1000.0];
    let initial_y = state_mgr.load_or_init(&lux_points_8);
    assert_eq!(initial_y.len(), 8);
    assert!(state_file.exists());

    // Modify values and save
    let mut modified_y = initial_y.clone();
    modified_y[2] = 0.42; // At 30.0 lux
    state_mgr.save(&lux_points_8, &modified_y).expect("Save state");

    // Reload and check
    let reloaded = state_mgr.load_or_init(&lux_points_8);
    assert_eq!(reloaded.len(), 8);
    assert!((reloaded[2] - 0.42).abs() < 1e-4);

    // Test resampling when point count changes: from 8 to 12 points
    let lux_points_12 = vec![
        0.0, 5.0, 10.0, 20.0, 30.0, 50.0, 70.0, 100.0, 150.0, 300.0, 600.0, 1000.0,
    ];
    let resampled = state_mgr.load_or_init(&lux_points_12);
    assert_eq!(resampled.len(), 12);
    // File must NOT be deleted, user calibration is preserved and resampled
    assert!(state_file.exists());
    assert!((resampled[0] - reloaded[0]).abs() < 1e-3);
    // Point 4 in 12-point array corresponds to 30.0 lux, where user calibrated 0.42
    assert!((resampled[4] - 0.42).abs() < 1e-3);
    assert!((resampled[11] - reloaded[7]).abs() < 1e-3);

    let _ = fs::remove_dir_all(&tmp_dir);
}
