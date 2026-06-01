use anyhow::{Result, bail};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Notify, sync::watch};
use zbus::{Connection, interface, object_server::SignalEmitter};

use crate::systemd::{Mode, SystemdController};

const DBUS_OBJECT_PATH: &str = "/";
const DBUS_SERVICE_NAME: &str = "org.guayusa.IdleInhibitor";

pub struct IdleInhibitorInterface {
    controller: Arc<SystemdController>,
    mode_watch_rx: watch::Receiver<Mode>,
}

impl IdleInhibitorInterface {
    pub fn new(controller: Arc<SystemdController>, mode_watch_rx: watch::Receiver<Mode>) -> Self {
        Self {
            controller,
            mode_watch_rx,
        }
    }

    async fn set_controller_mode(&self, mode: Mode) -> zbus::fdo::Result<Mode> {
        self.controller
            .set_mode(mode)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}

#[interface(name = "org.guayusa.Idle")]
impl IdleInhibitorInterface {
    async fn enable(&self) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: Enable method called");
        self.set_controller_mode(Mode::NoIdle).await?;
        Ok(())
    }

    async fn disable(&self) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: Disable method called");
        self.set_controller_mode(Mode::Normal).await?;
        Ok(())
    }

    async fn set_inhibit(&self, enable: bool) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: SetInhibit method called with enable={}", enable);
        let mode = if enable { Mode::NoIdle } else { Mode::Normal };
        self.set_controller_mode(mode).await?;
        Ok(())
    }

    async fn set_mode(&self, mode: &str) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: SetMode method called with mode={}", mode);
        let mode = Mode::parse_primary(mode).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs(format!(
                "invalid mode {mode}; expected normal, no-suspend, or no-idle"
            ))
        })?;
        self.set_controller_mode(mode).await?;
        Ok(())
    }

    async fn toggle(&self) -> zbus::fdo::Result<bool> {
        log::debug!("D-Bus: Toggle method called");
        let current_mode = *self.mode_watch_rx.borrow();
        let next_mode = if current_mode == Mode::Normal {
            Mode::NoIdle
        } else {
            Mode::Normal
        };
        let mode = self.set_controller_mode(next_mode).await?;
        Ok(mode.is_inhibited())
    }

    async fn cycle_mode(&self) -> zbus::fdo::Result<String> {
        log::debug!("D-Bus: CycleMode method called");
        let mode = self
            .controller
            .cycle_mode()
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(mode.as_str().to_string())
    }

    #[zbus(property)]
    fn status(&self) -> bool {
        self.mode_watch_rx.borrow().is_inhibited()
    }

    #[zbus(property)]
    fn mode(&self) -> String {
        self.mode_watch_rx.borrow().as_str().to_string()
    }

    #[zbus(signal)]
    #[zbus(name = "ModeChanged")]
    async fn mode_changed_signal(emitter: &SignalEmitter<'_>, mode: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    #[zbus(name = "StatusChanged")]
    async fn status_changed_signal(emitter: &SignalEmitter<'_>, status: bool) -> zbus::Result<()>;
}

pub async fn setup_dbus_service(
    connection: Connection,
    controller: Arc<SystemdController>,
    mode_watch_rx: watch::Receiver<Mode>,
) -> Result<Connection> {
    let idle_inhibitor = IdleInhibitorInterface::new(controller, mode_watch_rx);

    let dbus_connection = match tokio::time::timeout(Duration::from_secs(5), async {
        connection
            .object_server()
            .at(DBUS_OBJECT_PATH, idle_inhibitor)
            .await?;
        connection.request_name(DBUS_SERVICE_NAME).await?;
        Ok::<_, zbus::Error>(connection)
    })
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            log::error!("D-Bus setup timed out after 5 seconds");
            bail!("D-Bus setup timed out");
        }
    };

    log::info!(
        "D-Bus service started at {} on {}",
        DBUS_SERVICE_NAME,
        DBUS_OBJECT_PATH
    );
    Ok(dbus_connection)
}

pub async fn dbus_connection_task(_connection: Connection, shutdown_notify: Arc<Notify>) {
    log::info!("D-Bus connection task started, waiting for shutdown signal");
    shutdown_notify.notified().await;
    log::info!("D-Bus connection task shutting down");
}

pub async fn status_monitor_task(
    connection: Connection,
    mut mode_receiver: watch::Receiver<Mode>,
    shutdown_notify: Arc<Notify>,
) {
    log::info!("Status monitor task started");
    let mut last_status = mode_receiver.borrow_and_update().is_inhibited();
    let signal_emitter = match SignalEmitter::new(&connection, DBUS_OBJECT_PATH) {
        Ok(emitter) => emitter,
        Err(e) => {
            log::error!("Unable to create D-Bus signal emitter: {}", e);
            return;
        }
    };

    loop {
        tokio::select! {
            result = mode_receiver.changed() => {
                match result {
                    Ok(()) => {
                        let current_mode = *mode_receiver.borrow_and_update();
                        let mode_name = current_mode.as_str().to_string();
                        let current_status = current_mode.is_inhibited();

                        if let Err(e) = IdleInhibitorInterface::mode_changed_signal(&signal_emitter, &mode_name).await {
                            log::error!("Failed to emit ModeChanged signal: {}", e);
                        }
                        if current_status != last_status {
                            if let Err(e) = IdleInhibitorInterface::status_changed_signal(&signal_emitter, current_status).await {
                                log::error!("Failed to emit StatusChanged signal: {}", e);
                            }
                            last_status = current_status;
                        }
                    }
                    Err(_) => {
                        log::info!("Mode watch channel closed, shutting down status monitor");
                        break;
                    }
                }
            }
            _ = shutdown_notify.notified() => {
                log::info!("Status monitor task shutting down");
                break;
            }
        }
    }
}
