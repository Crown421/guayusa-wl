use anyhow::{bail, Result};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::sleep, time::timeout};
use zbus::{interface, ConnectionBuilder};

use crate::InhibitorMessage;

const DBUS_OBJECT_PATH: &str = "/org/matcha/IdleInhibitor";
const DBUS_SERVICE_NAME: &str = "org.matcha.IdleInhibitor";

// D-Bus interface for the idle inhibitor
pub struct IdleInhibitorInterface {
    sender: mpsc::UnboundedSender<InhibitorMessage>,
    status: Arc<AtomicBool>,
}

impl IdleInhibitorInterface {
    pub fn new(sender: mpsc::UnboundedSender<InhibitorMessage>, status: Arc<AtomicBool>) -> Self {
        Self { sender, status }
    }
}

#[interface(name = "org.matcha.IdleInhibitor")]
impl IdleInhibitorInterface {
    /// Enable idle inhibition
    fn enable(&self) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: Enable method called");
        if let Err(e) = self.sender.send(InhibitorMessage::Enable) {
            log::error!("Failed to send enable message: {}", e);
            return Err(zbus::fdo::Error::Failed(format!("Failed to send enable message: {}", e)));
        }
        Ok(())
    }

    /// Disable idle inhibition
    fn disable(&self) -> zbus::fdo::Result<()> {
        log::debug!("D-Bus: Disable method called");
        if let Err(e) = self.sender.send(InhibitorMessage::Disable) {
            log::error!("Failed to send disable message: {}", e);
            return Err(zbus::fdo::Error::Failed(format!("Failed to send disable message: {}", e)));
        }
        Ok(())
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

    let dbus_connection = match timeout(
        Duration::from_secs(5), 
        ConnectionBuilder::session()?
            .name(DBUS_SERVICE_NAME)?
            .serve_at(DBUS_OBJECT_PATH, idle_inhibitor)?
            .build()
    ).await {
        Ok(result) => result?,
        Err(_) => {
            log::error!("D-Bus connection timed out after 5 seconds");
            bail!("D-Bus connection timed out");
        }
    };

    log::info!("D-Bus service started at {} on {}", DBUS_SERVICE_NAME, DBUS_OBJECT_PATH);
    Ok(dbus_connection)
}

pub async fn dbus_connection_task(
    connection: zbus::Connection,
    shutdown: Arc<AtomicBool>,
) {
    // Keep the connection alive by waiting for it to close
    let _connection = connection;
    log::info!("D-Bus connection established successfully");

    // Wait for shutdown signal
    while !shutdown.load(Ordering::Relaxed) {
        // Don't call any methods that would close the connection
        // Just sleep and keep the connection in scope
        sleep(Duration::from_millis(100)).await;
    }
    log::info!("D-Bus connection task shutting down");
}
