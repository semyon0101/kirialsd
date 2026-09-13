use kirialsd::als::{extract_f64_from_value, extract_light_level_from_signal, AlsManager};
use kirialsd::config::{DaemonConfig, LogLevel};
use kirialsd::curve::ClightCurve;
use kirialsd::state::StateManager;
use zbus::zvariant::Value;

#[test]
fn test_als_value_extraction() {
    let val_f64 = Value::F64(42.5);
    assert_eq!(extract_f64_from_value(&val_f64), Some(42.5));

    let val_u32 = Value::U32(100);
    assert_eq!(extract_f64_from_value(&val_u32), Some(100.0));

    let val_i64 = Value::I64(-15);
    assert_eq!(extract_f64_from_value(&val_i64), Some(-15.0));

    let val_str = Value::Str("invalid".into());
    assert_eq!(extract_f64_from_value(&val_str), None);
}

#[test]
fn test_live_als_reading_and_target_evaluation() {
    let mut als_mgr = AlsManager::new(LogLevel::Off);
    let maybe_lux = als_mgr.get_initial_lux();

    println!("Live ALS reading result: {:?}", maybe_lux);

    if let Some(lux) = maybe_lux {
        assert!(lux >= 0.0, "Lux should be non-negative: {}", lux);
        assert!(!lux.is_nan(), "Lux should not be NaN");

        // Now test curve target calculation for this actual sensor reading
        let config = DaemonConfig::default();
        let state_mgr = StateManager::new();
        let y_coords = state_mgr.load_or_init(&config.lux_points);
        let curve = ClightCurve::new(
            &config.lux_points,
            &y_coords,
            config.effective_max_lux(),
            config.shutter_threshold,
        );

        let target_fraction = curve.evaluate(lux);
        let target_pct = target_fraction * 100.0;

        println!(
            "-> Live ALS test: Detected {:.1} lux -> Evaluated Backlight: {:.1}%",
            lux, target_pct
        );

        assert!(
            target_fraction >= 0.01 && target_fraction <= 1.0,
            "Target brightness fraction {} must be in [0.01, 1.0]",
            target_fraction
        );
    } else {
        println!("Note: No physical ALS or SensorProxy available in current environment");
    }
}

#[test]
fn test_sensor_events_monitoring() {
    use zbus::MatchRule;
    use zbus::MessageStream;
    use futures_lite::stream::StreamExt;
    use async_io::Timer;
    use futures_lite::future::FutureExt;
    use std::time::Duration;

    zbus::block_on(async {
        let conn = zbus::Connection::system().await.unwrap();

        let claim_res = conn.call_method(
            Some("net.hadess.SensorProxy"),
            "/net/hadess/SensorProxy",
            Some("net.hadess.SensorProxy"),
            "ClaimLight",
            &(),
        ).await;
        if claim_res.is_err() {
            println!("Note: net.hadess.SensorProxy not available in this environment, skipping live monitoring test");
            return;
        }

        // 2. SensorProxy rule WITHOUT sender so :1.xxx matches
        let sensor_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus.Properties").unwrap()
            .member("PropertiesChanged").unwrap()
            .path("/net/hadess/SensorProxy").unwrap()
            .build();

        let mut sensor_stream = MessageStream::for_match_rule(sensor_rule, &conn, Some(64)).await.unwrap();

        println!("Testing live sensor updates for 3 seconds...");
        let start = std::time::Instant::now();
        let mut count = 0;
        while start.elapsed() < Duration::from_secs(3) {
            let next_sensor = sensor_stream.next();
            let timeout_fut = async {
                Timer::after(Duration::from_millis(1000)).await;
                None
            };

            let res = next_sensor.or(timeout_fut).await;
            if let Some(Ok(msg)) = res {
                if let Some(lux) = extract_light_level_from_signal(&msg) {
                    println!("-> Signal received: {:.1} lux", lux);
                    count += 1;
                }
            } else {
                // Heartbeat poll
                if let Ok(reply) = conn.call_method(
                    Some("net.hadess.SensorProxy"),
                    "/net/hadess/SensorProxy",
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &("net.hadess.SensorProxy", "LightLevel"),
                ).await {
                    if let Ok(owned_val) = reply.body().deserialize::<zbus::zvariant::OwnedValue>() {
                        if let Ok(val) = owned_val.downcast_ref::<zbus::zvariant::Value>() {
                            if let Some(lux) = extract_f64_from_value(&val) {
                                println!("-> Heartbeat polled: {:.1} lux", lux);
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
        println!("Total updates received: {}", count);
        assert!(count > 0, "Must receive at least one sensor update");
    });
}
