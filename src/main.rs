mod dbus;
mod wayland;

use anyhow::Result;
use futures::StreamExt;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook_tokio::Signals;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::mpsc;

// Message types for internal communication
#[derive(Debug, Clone)]
pub enum InhibitorMessage {
    Enable,
    Disable,
    Shutdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Starting Matcha Idle Inhibitor D-Bus service");

    // Set up Wayland
    let (_wayland_conn, event_queue, matcha_state) = wayland::setup_wayland().await?;

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
    let signals = Signals::new(&[SIGINT, SIGTERM])?;
    let shutdown_clone = Arc::clone(&shutdown);
    let sender_clone = sender.clone();

    tokio::spawn(async move {
        signals.for_each(|signal| {
            let shutdown_clone = shutdown_clone.clone();
            let sender_clone = sender_clone.clone();
            async move {
                match signal {
                    SIGINT | SIGTERM => {
                        log::info!("Received termination signal, shutting down");
                        shutdown_clone.store(true, Ordering::Relaxed);
                        let _ = sender_clone.send(InhibitorMessage::Shutdown);
                    }
                    _ => {}
                }
            }
        }).await;
    });

    // Run the Wayland event loop
    let wayland_task = tokio::spawn(wayland::wayland_event_loop(
        event_queue,
        matcha_state,
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

    log::info!("Matcha Idle Inhibitor service stopped");
    Ok(())
}
