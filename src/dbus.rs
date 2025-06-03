use anyhow::{Result, bail};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::sleep, time::timeout};
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

pub async fn dbus_connection_task(connection: zbus::Connection, shutdown: Arc<AtomicBool>) {
    // Keep the connection alive by waiting for it to close
    let _connection = connection;
    log::info!("D-Bus connection established successfully");

    // Wait for shutdown signal
    while !shutdown.load(Ordering::Relaxed) {
        // Don't call any methods that would close the connection
        // Just sleep and keep the connection in scope
        sleep(Duration::from_millis(500)).await;
    }
    log::info!("D-Bus connection task shutting down");
}
