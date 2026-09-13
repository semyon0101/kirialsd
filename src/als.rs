use async_io::Timer;
use futures_util::{pin_mut, select_biased, FutureExt, StreamExt};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::zvariant::Value;
use zbus::{MatchRule, MessageStream};

use crate::config::LogLevel;
use crate::{log_err, log_msg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuxSource {
    Signal,
    Poll,
}

#[derive(Debug, Clone)]
pub enum DaemonEvent {
    LuxChanged {
        lux: f64,
        source: LuxSource,
    },
    PrepareForSleep {
        going_to_sleep: bool,
        resume_lux: Option<f64>,
    },
}

pub struct AlsManager {
    dbus_conn: Option<Connection>,
    has_claimed_sensor_proxy: bool,
    listener_thread: Option<thread::JoinHandle<()>>,
    shutdown_tx: Option<async_channel::Sender<()>>,
}

impl AlsManager {
    pub fn new(log_level: LogLevel) -> Self {
        let dbus_conn = Connection::system().ok();
        if dbus_conn.is_none() {
            log_err!(
                LogLevel::Info,
                log_level,
                "[-] Warning: Failed to connect to system D-Bus"
            );
        }

        Self {
            dbus_conn,
            has_claimed_sensor_proxy: false,
            listener_thread: None,
            shutdown_tx: None,
        }
    }

    pub fn get_initial_lux(&mut self) -> Option<f64> {
        self.read_live_lux()
    }

    pub fn read_live_lux(&mut self) -> Option<f64> {
        let conn = self.dbus_conn.as_ref()?;
        let claimed_here = if !self.has_claimed_sensor_proxy {
            if claim_als_proxy(conn).is_ok() {
                true
            } else {
                return None;
            }
        } else {
            false
        };

        // Allow sensor hardware to wake up and perform ADC conversion
        std::thread::sleep(Duration::from_millis(150));

        let lux = get_dbus_lux(conn);

        if claimed_here {
            let _ = release_als_proxy(conn);
        }

        lux
    }

    pub fn start_listeners(
        &mut self,
        tx: Sender<DaemonEvent>,
        shutdown: Arc<AtomicBool>,
        poll_interval: Arc<AtomicI64>,
        log_level: LogLevel,
    ) {
        let (shutdown_tx, shutdown_rx) = async_channel::bounded::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // Start unified async event loop with signal streaming and heartbeat polling
        let handle = thread::Builder::new()
            .name("als-dbus-listener".to_string())
            .spawn(move || {
                zbus::block_on(async move {
                    let async_conn = match zbus::Connection::system().await {
                        Ok(c) => c,
                        Err(e) => {
                            log_err!(
                                LogLevel::Info,
                                log_level,
                                "[-] Failed to connect async system D-Bus: {}",
                                e
                            );
                            return;
                        }
                    };

                    // Claim sensor on async_conn
                    if let Err(e) = async_conn
                        .call_method(
                            Some("net.hadess.SensorProxy"),
                            "/net/hadess/SensorProxy",
                            Some("net.hadess.SensorProxy"),
                            "ClaimLight",
                            &(),
                        )
                        .await
                    {
                        log_err!(
                            LogLevel::Info,
                            log_level,
                            "[-] Warning: Failed to claim net.hadess.SensorProxy: {}",
                            e
                        );
                        return;
                    }

                    log_msg!(
                        LogLevel::Info,
                        log_level,
                        "[+] Claimed SensorProxy Light sensor"
                    );

                    // Match rule for PropertiesChanged on /net/hadess/SensorProxy
                    // Note: sender is omitted because D-Bus daemon emits signals with unique name (:1.xxx)
                    let sensor_rule = MatchRule::builder()
                        .msg_type(zbus::message::Type::Signal)
                        .interface("org.freedesktop.DBus.Properties")
                        .expect("Valid interface")
                        .member("PropertiesChanged")
                        .expect("Valid member")
                        .path("/net/hadess/SensorProxy")
                        .expect("Valid path")
                        .build();

                    // Match rule for PrepareForSleep
                    let sleep_rule = MatchRule::builder()
                        .msg_type(zbus::message::Type::Signal)
                        .interface("org.freedesktop.login1.Manager")
                        .expect("Valid interface")
                        .member("PrepareForSleep")
                        .expect("Valid member")
                        .path("/org/freedesktop/login1")
                        .expect("Valid path")
                        .build();

                    let sensor_stream =
                        match MessageStream::for_match_rule(sensor_rule, &async_conn, Some(64))
                            .await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                log_err!(LogLevel::Info, log_level, "[-] Failed sensor stream: {}", e);
                                return;
                            }
                        };

                    let sleep_stream =
                        match MessageStream::for_match_rule(sleep_rule, &async_conn, Some(16)).await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                log_err!(LogLevel::Info, log_level, "[-] Failed sleep stream: {}", e);
                                return;
                            }
                        };

                    // Send initial sensor reading immediately
                    if let Ok(reply) = async_conn
                        .call_method(
                            Some("net.hadess.SensorProxy"),
                            "/net/hadess/SensorProxy",
                            Some("org.freedesktop.DBus.Properties"),
                            "Get",
                            &("net.hadess.SensorProxy", "LightLevel"),
                        )
                        .await
                    {
                        if let Ok(owned_val) = reply.body().deserialize::<zbus::zvariant::OwnedValue>() {
                            if let Ok(val) = owned_val.downcast_ref::<zbus::zvariant::Value>() {
                                if let Some(lux) = extract_f64_from_value(&val) {
                                    let _ = tx.send(DaemonEvent::LuxChanged {
                                        lux,
                                        source: LuxSource::Poll,
                                    });
                                }
                            }
                        }
                    }

                    let sensor_stream = sensor_stream.fuse();
                    let sleep_stream = sleep_stream.fuse();
                    pin_mut!(sensor_stream);
                    pin_mut!(sleep_stream);

                    let mut is_sleeping = false;

                    while !shutdown.load(Ordering::Relaxed) {
                        if shutdown_rx.is_closed() {
                            break;
                        }

                        let current_poll_ms = poll_interval.load(Ordering::Relaxed);
                        let poll_enabled = current_poll_ms > 0;
                        let poll_duration = if poll_enabled && !is_sleeping {
                            Duration::from_millis(current_poll_ms as u64)
                        } else {
                            Duration::from_secs(3600)
                        };

                        let timer = FutureExt::fuse(Timer::after(poll_duration));
                        let shutdown_fut = FutureExt::fuse(shutdown_rx.recv());
                        pin_mut!(timer);
                        pin_mut!(shutdown_fut);

                        select_biased! {
                            _ = shutdown_fut => {
                                break;
                            }
                            msg = sleep_stream.next() => {
                                match msg {
                                    Some(Ok(msg)) => {
                                        if let Some(member) = msg.header().member() {
                                            if member.as_str() == "PrepareForSleep" {
                                                if let Ok(going_to_sleep) = msg.body().deserialize::<bool>() {
                                                    is_sleeping = going_to_sleep;

                                                    // If resuming, query fresh LightLevel immediately before sending event
                                                    let resume_lux = if !going_to_sleep {
                                                        if let Ok(reply) = async_conn
                                                            .call_method(
                                                                Some("net.hadess.SensorProxy"),
                                                                "/net/hadess/SensorProxy",
                                                                Some("org.freedesktop.DBus.Properties"),
                                                                "Get",
                                                                &("net.hadess.SensorProxy", "LightLevel"),
                                                            )
                                                            .await
                                                        {
                                                            if let Ok(owned_val) = reply.body().deserialize::<zbus::zvariant::OwnedValue>() {
                                                                if let Ok(val) = owned_val.downcast_ref::<zbus::zvariant::Value>() {
                                                                    extract_f64_from_value(&val)
                                                                } else {
                                                                    None
                                                                }
                                                            } else {
                                                                None
                                                            }
                                                        } else {
                                                            None
                                                        }
                                                    } else {
                                                        None
                                                    };

                                                    if tx.send(DaemonEvent::PrepareForSleep {
                                                        going_to_sleep,
                                                        resume_lux,
                                                    }).is_err() {
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Some(Err(e)) => {
                                        log_err!(LogLevel::Debug, log_level, "[-] Sleep D-Bus stream error: {}", e);
                                    }
                                    None => {
                                        log_err!(LogLevel::Info, log_level, "[-] Sleep D-Bus stream disconnected");
                                        break;
                                    }
                                }
                            }
                            msg = sensor_stream.next() => {
                                match msg {
                                    Some(Ok(msg)) => {
                                        if !is_sleeping {
                                            if let Some(member) = msg.header().member() {
                                                if member.as_str() == "PropertiesChanged" {
                                                    if let Some(lux) = extract_light_level_from_signal(&msg) {
                                                        if tx.send(DaemonEvent::LuxChanged {
                                                            lux,
                                                            source: LuxSource::Signal,
                                                        }).is_err() {
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Some(Err(e)) => {
                                        log_err!(LogLevel::Debug, log_level, "[-] Sensor D-Bus stream error: {}", e);
                                    }
                                    None => {
                                        log_err!(LogLevel::Info, log_level, "[-] Sensor D-Bus stream disconnected");
                                        break;
                                    }
                                }
                            }
                            _ = timer => {
                                if poll_enabled && !is_sleeping {
                                    if let Ok(reply) = async_conn
                                        .call_method(
                                            Some("net.hadess.SensorProxy"),
                                            "/net/hadess/SensorProxy",
                                            Some("org.freedesktop.DBus.Properties"),
                                            "Get",
                                            &("net.hadess.SensorProxy", "LightLevel"),
                                        )
                                        .await
                                    {
                                        if let Ok(owned_val) = reply.body().deserialize::<zbus::zvariant::OwnedValue>() {
                                            if let Ok(val) = owned_val.downcast_ref::<zbus::zvariant::Value>() {
                                                if let Some(lux) = extract_f64_from_value(&val) {
                                                    if tx.send(DaemonEvent::LuxChanged {
                                                        lux,
                                                        source: LuxSource::Poll,
                                                    }).is_err() {
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Explicitly release sensor on the exact same connection that claimed it
                    // Wrapped in a 500ms timeout to avoid deadlocking shutdown() if iio-sensor-proxy crashed
                    let release_call = FutureExt::fuse(async_conn.call_method(
                        Some("net.hadess.SensorProxy"),
                        "/net/hadess/SensorProxy",
                        Some("net.hadess.SensorProxy"),
                        "ReleaseLight",
                        &(),
                    ));
                    let release_timer = FutureExt::fuse(Timer::after(Duration::from_millis(500)));
                    pin_mut!(release_call);
                    pin_mut!(release_timer);

                    select_biased! {
                        _ = release_call => {
                            log_msg!(
                                LogLevel::Info,
                                log_level,
                                "[*] Released SensorProxy ALS on async connection"
                            );
                        }
                        _ = release_timer => {
                            log_err!(
                                LogLevel::Debug,
                                log_level,
                                "[-] Timeout releasing SensorProxy ALS (daemon may have crashed)"
                            );
                        }
                    }
                });
            })
            .expect("Failed to spawn unified D-Bus listener thread");

        self.listener_thread = Some(handle);
    }

    pub fn shutdown(&mut self, log_level: LogLevel) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.close();
        }
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
        if self.has_claimed_sensor_proxy {
            if let Some(conn) = &self.dbus_conn {
                log_msg!(
                    LogLevel::Info,
                    log_level,
                    "[*] Releasing SensorProxy ALS..."
                );
                let _ = release_als_proxy(conn);
            }
            self.has_claimed_sensor_proxy = false;
        }
    }
}

impl Drop for AlsManager {
    fn drop(&mut self) {
        self.shutdown(LogLevel::Off);
    }
}

pub fn claim_als_proxy(conn: &Connection) -> Result<(), zbus::Error> {
    conn.call_method(
        Some("net.hadess.SensorProxy"),
        "/net/hadess/SensorProxy",
        Some("net.hadess.SensorProxy"),
        "ClaimLight",
        &(),
    )?;
    Ok(())
}

pub fn release_als_proxy(conn: &Connection) -> Result<(), zbus::Error> {
    conn.call_method(
        Some("net.hadess.SensorProxy"),
        "/net/hadess/SensorProxy",
        Some("net.hadess.SensorProxy"),
        "ReleaseLight",
        &(),
    )?;
    Ok(())
}

pub fn extract_f64_from_value(val: &Value) -> Option<f64> {
    match val {
        Value::F64(v) => Some(*v),
        Value::U64(v) => Some(*v as f64),
        Value::I64(v) => Some(*v as f64),
        Value::U32(v) => Some(*v as f64),
        Value::I32(v) => Some(*v as f64),
        Value::Value(inner) => extract_f64_from_value(inner),
        _ => None,
    }
}

pub fn get_dbus_lux(conn: &Connection) -> Option<f64> {
    let reply = conn
        .call_method(
            Some("net.hadess.SensorProxy"),
            "/net/hadess/SensorProxy",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("net.hadess.SensorProxy", "LightLevel"),
        )
        .ok()?;

    let owned_val: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    match owned_val.downcast_ref::<Value>() {
        Ok(v) => extract_f64_from_value(&v),
        Err(_) => None,
    }
}

pub fn extract_light_level_from_signal(msg: &zbus::message::Message) -> Option<f64> {
    let body = msg.body();
    let (_, changed_props, _): (String, HashMap<String, Value>, Vec<String>) =
        body.deserialize().ok()?;
    if let Some(val) = changed_props.get("LightLevel") {
        return extract_f64_from_value(val);
    }
    None
}
