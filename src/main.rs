mod dbus;
mod wayland;

use anyhow::Result;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{signal, sync::mpsc};

// Message types for internal communication
#[derive(Debug, Clone)]
pub enum InhibitorMessage {
    Enable,
    Disable,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Starting Guayusa Idle Inhibitor D-Bus service");

    // Set up Wayland
    let (_wayland_conn, event_queue, guayusa_state) = wayland::setup_wayland().await?;

    log::debug!("Wayland setup done");

    // Set up communication channels
    let (sender, receiver) = mpsc::unbounded_channel();
    let status = Arc::new(AtomicBool::new(false)); // Start with inhibition OFF
    let shutdown = Arc::new(AtomicBool::new(false));

    log::debug!("Comm channel setup done");

    // Set up D-Bus service
    let dbus_connection = dbus::setup_dbus_service(sender.clone(), Arc::clone(&status)).await?;

    log::debug!("D-Bus service setup done");

    // Set up signal handling
    let shutdown_clone = Arc::clone(&shutdown);
    
    tokio::spawn(async move {
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
        
        tokio::select! {
            _ = sigint.recv() => log::info!("Received SIGINT, shutting down"),
            _ = sigterm.recv() => log::info!("Received SIGTERM, shutting down"),
        }
        
        shutdown_clone.store(true, Ordering::Relaxed);
    });

    // Run the Wayland event loop
    let wayland_task = tokio::spawn(wayland::wayland_event_loop(
        event_queue,
        guayusa_state,
        receiver,
        status,
        Arc::clone(&shutdown),
    ));

    // Keep the D-Bus connection alive by spawning a task
    let shutdown_clone_dbus = Arc::clone(&shutdown);
    let dbus_task = tokio::spawn(dbus::dbus_connection_task(
        dbus_connection,
        shutdown_clone_dbus,
    ));

    // Wait for either task to finish or a shutdown signal
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
    }

    log::info!("Guayusa Idle Inhibitor service stopped");
    Ok(())
}
