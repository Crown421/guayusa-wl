use anyhow::{Result, bail};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, sync::mpsc, time::timeout};
use zbus::{Connection, interface};

use crate::InhibitorMessage;

const DBUS_OBJECT_PATH: &str = "/";
const DBUS_SERVICE_NAME: &str = "org.guayusa.IdleInhibitor";

// D-Bus interface for the idle inhibitor
pub struct IdleInhibitorInterface {
    sender: mpsc::UnboundedSender<InhibitorMessage>,
    status: Arc<AtomicBool>,
}

impl IdleInhibitorInterface {
    pub fn new(sender: mpsc::UnboundedSender<InhibitorMessage>, status: Arc<AtomicBool>) -> Self {
        Self { sender, status }
    }

    /// Helper function to send messages with consistent error handling
    fn send_message(&self, message: InhibitorMessage, context: &str) -> zbus::fdo::Result<()> {
        if let Err(e) = self.sender.send(message) {
            log::error!("Failed to send {} message: {}", context, e);
            return Err(zbus::fdo::Error::Failed(format!(
                "Failed to send {} message: {}",
                context, e
            )));
        }
        Ok(())
    }
}

#[interface(name = "org.guayusa.Idle")]
impl IdleInhibitorInterface {
    /// Enable idle inhibition
    fn enable(&self) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: Enable method called");
        self.send_message(InhibitorMessage::Enable, "enable")
    }

    /// Disable idle inhibition
    fn disable(&self) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: Disable method called");
        self.send_message(InhibitorMessage::Disable, "disable")
    }

    /// Set idle inhibition state (true = enable, false = disable)
    fn set_inhibit(&self, enable: bool) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: SetInhibit method called with enable={}", enable);
        let message = if enable {
            InhibitorMessage::Enable
        } else {
            InhibitorMessage::Disable
        };
        let context = if enable { "enable" } else { "disable" };
        self.send_message(message, context)
    }

    /// Toggle idle inhibition state and return the new state
    fn toggle(&self) -> zbus::fdo::Result<bool> {
        log::debug!("D-Bus: Toggle method called");
        self.send_message(InhibitorMessage::Toggle, "toggle")?;
        // Return the new state after toggling
        Ok(!self.status.load(Ordering::Relaxed))
    }

    /// Get the current status of idle inhibition
    #[zbus(property)]
    fn status(&self) -> bool {
        self.status.load(Ordering::Relaxed)
    }
}

pub async fn setup_dbus_service(
    sender: mpsc::UnboundedSender<InhibitorMessage>,
    status: Arc<AtomicBool>,
) -> Result<zbus::Connection> {
    let idle_inhibitor = IdleInhibitorInterface::new(sender, status);

    let dbus_connection = match timeout(Duration::from_secs(5), async {
        let connection = Connection::session().await?;
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
            log::error!("D-Bus connection timed out after 5 seconds");
            bail!("D-Bus connection timed out");
        }
    };

    log::info!(
        "D-Bus service started at {} on {}",
        DBUS_SERVICE_NAME,
        DBUS_OBJECT_PATH
    );
    Ok(dbus_connection)
}

pub async fn dbus_connection_task(connection: zbus::Connection, shutdown_notify: Arc<Notify>) {
    // Keep the connection alive by keeping it in scope
    let _connection = connection;
    log::info!("D-Bus connection task started, waiting for shutdown signal");

    // Wait for the shutdown notification. This is very CPU efficient.
    shutdown_notify.notified().await;

    log::info!("D-Bus connection task shutting down");
}

/// Task that monitors status changes and emits D-Bus signals
pub async fn status_monitor_task(
    connection: zbus::Connection,
    status: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
) {
    log::info!("Status monitor task started");

    let mut last_status = status.load(Ordering::Relaxed);
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let current_status = status.load(Ordering::Relaxed);
                if current_status != last_status {
                    log::debug!("Status changed from {} to {}", last_status, current_status);

                    // Emit the D-Bus signal using a simple approach
                    let signal_msg = zbus::Message::signal(
                        DBUS_OBJECT_PATH,
                        "org.guayusa.Idle",
                        "StatusChanged"
                    )
                    .unwrap()
                    .build(&current_status)
                    .unwrap();

                    if let Err(e) = connection.send(&signal_msg).await {
                        log::error!("Failed to emit status change signal: {}", e);
                    } else {
                        log::info!("Emitted status change signal: enabled={}", current_status);
                    }

                    last_status = current_status;
                }
            }
            _ = shutdown_notify.notified() => {
                log::info!("Status monitor task shutting down");
                break;
            }
        }
    }
}
