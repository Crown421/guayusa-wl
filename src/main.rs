mod dbus;
mod wayland;

use anyhow::Result;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{signal, sync::Notify, sync::mpsc};

// Message types for internal communication
#[derive(Debug, Clone)]
pub enum InhibitorMessage {
    Enable,
    Disable,
    Toggle,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Starting Guayusa Idle Inhibitor D-Bus service");

    // Set up Wayland
    let (wayland_conn, event_queue, guayusa_state) = wayland::setup_wayland().await?;

    log::debug!("Wayland setup done");

    // Set up communication channels
    let (sender, receiver) = mpsc::unbounded_channel();
    let status = Arc::new(AtomicBool::new(false)); // Start with inhibition OFF
    let wayland_shutdown_signal = Arc::new(AtomicBool::new(false)); // For Wayland loop
    let dbus_shutdown_notify = Arc::new(Notify::new()); // For D-Bus task

    log::debug!("Comm channel setup done");

    // Set up D-Bus service
    let dbus_connection = dbus::setup_dbus_service(sender.clone(), Arc::clone(&status)).await?;

    log::debug!("D-Bus service setup done");

    // Set up signal handling
    let wayland_shutdown_signal_clone = Arc::clone(&wayland_shutdown_signal);
    let dbus_shutdown_notify_clone = Arc::clone(&dbus_shutdown_notify);

    tokio::spawn(async move {
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();

        tokio::select! {
            _ = sigint.recv() => log::info!("Received SIGINT, shutting down"),
            _ = sigterm.recv() => log::info!("Received SIGTERM, shutting down"),
        }

        wayland_shutdown_signal_clone.store(true, Ordering::Relaxed);
        dbus_shutdown_notify_clone.notify_one(); // Notify D-Bus task to shut down
    });

    // Run the Wayland event loop
    let wayland_task = tokio::spawn(wayland::wayland_event_loop(
        wayland_conn,
        event_queue,
        guayusa_state,
        receiver,
        Arc::clone(&status),
        Arc::clone(&wayland_shutdown_signal),
    ));

    // Keep the D-Bus connection alive by spawning a task
    let dbus_task = tokio::spawn(dbus::dbus_connection_task(
        dbus_connection,
        Arc::clone(&dbus_shutdown_notify),
    ));

    // Start the status monitoring task
    let status_monitor_task = tokio::spawn(dbus::status_monitor_task(
        // Create a new connection for the status monitor to avoid borrowing issues
        zbus::Connection::session().await?,
        Arc::clone(&status),
        Arc::clone(&dbus_shutdown_notify),
    ));

    // Wait for any task to finish or a shutdown signal
    tokio::select! {
        result = wayland_task => {
            match result {
                Ok(Ok(())) => log::info!("Wayland event loop finished successfully"),
                Ok(Err(e)) => log::error!("Wayland event loop error: {}", e),
                Err(e) => log::error!("Wayland task join error: {}", e),
            }
        }
        result = dbus_task => {
            match result {
                Ok(()) => log::info!("D-Bus task finished successfully"),
                Err(e) => log::error!("D-Bus task join error: {}", e),
            }
        }
        result = status_monitor_task => {
            match result {
                Ok(()) => log::info!("Status monitor task finished successfully"),
                Err(e) => log::error!("Status monitor task join error: {}", e),
            }
        }
    }

    log::info!("Guayusa Idle Inhibitor service stopped");
    Ok(())
}
