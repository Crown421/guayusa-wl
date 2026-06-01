mod dbus;
mod systemd;

use anyhow::Result;
use std::sync::Arc;
use tokio::{signal, sync::Notify, sync::watch};
use zbus::Connection;

use systemd::{Mode, SystemdController};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Starting Guayusa D-Bus service");

    let dbus_connection = Connection::session().await?;
    let (mode_sender, mode_receiver) = watch::channel(Mode::Unavailable);
    let shutdown_notify = Arc::new(Notify::new());

    let controller = Arc::new(SystemdController::new(
        dbus_connection.clone(),
        mode_sender.clone(),
    ));

    if let Err(e) = controller.refresh_mode().await {
        log::warn!("Unable to infer initial mode: {}", e);
    }

    let dbus_connection = dbus::setup_dbus_service(
        dbus_connection,
        Arc::clone(&controller),
        mode_receiver.clone(),
    )
    .await?;

    let signal_shutdown_notify = Arc::clone(&shutdown_notify);
    tokio::spawn(async move {
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();

        tokio::select! {
            _ = sigint.recv() => log::info!("Received SIGINT, shutting down"),
            _ = sigterm.recv() => log::info!("Received SIGTERM, shutting down"),
        }

        signal_shutdown_notify.notify_waiters();
    });

    let dbus_task = tokio::spawn(dbus::dbus_connection_task(
        dbus_connection.clone(),
        Arc::clone(&shutdown_notify),
    ));

    let status_monitor_task = tokio::spawn(dbus::status_monitor_task(
        dbus_connection.clone(),
        mode_receiver,
        Arc::clone(&shutdown_notify),
    ));

    let state_poll_task = tokio::spawn(state_poll_task(
        Arc::clone(&controller),
        Arc::clone(&shutdown_notify),
    ));

    tokio::select! {
        result = dbus_task => {
            if let Err(e) = result {
                log::error!("D-Bus task join error: {}", e);
            }
        }
        result = status_monitor_task => {
            if let Err(e) = result {
                log::error!("Status monitor task join error: {}", e);
            }
        }
        result = state_poll_task => {
            if let Err(e) = result {
                log::error!("State poll task join error: {}", e);
            }
        }
    }

    log::info!("Guayusa service stopped");
    Ok(())
}

async fn state_poll_task(controller: Arc<SystemdController>, shutdown_notify: Arc<Notify>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = controller.refresh_mode().await {
                    log::warn!("Unable to refresh mode: {}", e);
                }
            }
            _ = shutdown_notify.notified() => {
                log::info!("State poll task shutting down");
                break;
            }
        }
    }
}
