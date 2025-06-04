use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use std::{
    os::unix::io::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{io::unix::AsyncFd, sync::mpsc};

use wayland_client::{
    Connection as WaylandConnection, Dispatch, QueueHandle,
    protocol::{wl_compositor, wl_registry, wl_surface},
};

// Use the idle inhibit protocol from wayland-protocols crate
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};

use crate::InhibitorMessage;

// Optimized helper function to flush and dispatch Wayland events
fn flush_and_dispatch(
    event_queue: &mut wayland_client::EventQueue<GuayusaWaylandState>,
    state: &mut GuayusaWaylandState,
    needs_flush: bool,
) -> Result<()> {
    if needs_flush && event_queue.flush().is_err() {
        return Err(anyhow::anyhow!("Failed to flush Wayland connection"));
    }

    event_queue
        .dispatch_pending(state)
        .map(|_| ()) // Directly convert to ()
        .map_err(|e| anyhow::anyhow!("Dispatch error: {}", e))
}

// Batch process Wayland events for better efficiency
fn flush_and_dispatch_batch(
    event_queue: &mut wayland_client::EventQueue<GuayusaWaylandState>,
    state: &mut GuayusaWaylandState,
    needs_flush: bool,
    max_events: usize,
) -> Result<()> {
    if needs_flush && event_queue.flush().is_err() {
        return Err(anyhow::anyhow!("Failed to flush Wayland connection"));
    }

    let mut processed = 0;
    while processed < max_events {
        match event_queue.dispatch_pending(state) {
            Ok(0) => break, // No more events
            Ok(_) => processed += 1,
            Err(e) => return Err(anyhow::anyhow!("Dispatch error: {}", e)),
        }
    }

    Ok(())
}

// Wayland state structure
pub struct GuayusaWaylandState {
    compositor: Option<wl_compositor::WlCompositor>,
    idle_inhibit_manager: Option<ZwpIdleInhibitManagerV1>,
    surface: Option<wl_surface::WlSurface>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
}

impl GuayusaWaylandState {
    pub fn new() -> Self {
        GuayusaWaylandState {
            compositor: None,
            idle_inhibit_manager: None,
            surface: None,
            inhibitor: None,
        }
    }

    pub fn create_inhibitor(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if self.inhibitor.is_some() {
            log::debug!("Inhibitor already exists.");
            return Ok(());
        }
        let manager = self
            .idle_inhibit_manager
            .as_ref()
            .context("Idle inhibit manager not available")?;
        let surface = self.surface.as_ref().context("Surface not available")?;

        log::debug!("Creating idle inhibitor.");
        self.inhibitor = Some(manager.create_inhibitor(surface, qh, ()));
        // Commit the surface after creating the inhibitor (matching C implementation)
        surface.commit();
        Ok(())
    }

    pub fn destroy_inhibitor(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            log::debug!("Destroying idle inhibitor.");
            inhibitor.destroy();
            if let Some(surface) = &self.surface {
                surface.commit();
            }
        }
    }

    pub fn cleanup(&mut self) {
        log::debug!("Cleaning up Wayland resources.");

        // Take the inhibitor first to avoid duplicate surface commits
        if let Some(inhibitor) = self.inhibitor.take() {
            log::debug!("Destroying idle inhibitor.");
            inhibitor.destroy();
        }

        // Only commit and destroy surface once
        if let Some(surface) = self.surface.take() {
            surface.commit();
            surface.destroy();
        }
    }

    pub fn is_inhibited(&self) -> bool {
        self.inhibitor.is_some()
    }
}

// Implement Dispatch for Wayland objects we care about
impl Dispatch<wl_registry::WlRegistry, ()> for GuayusaWaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &WaylandConnection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            log::debug!(target: "guayusa_rust::wayland", "Global: name={}, interface={}, version={}", name, interface, version);
            match interface.as_str() {
                "wl_compositor" => {
                    log::info!("Found wl_compositor");
                    let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(4),
                        qh,
                        (),
                    );
                    state.compositor = Some(compositor);
                }
                "zwp_idle_inhibit_manager_v1" => {
                    log::info!("Found zwp_idle_inhibit_manager_v1");
                    let manager = registry.bind::<ZwpIdleInhibitManagerV1, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    );
                    state.idle_inhibit_manager = Some(manager);
                }
                _ => {}
            }
        }
    }
}

// Dummy dispatch for objects we bind but don't need to handle events for
impl Dispatch<wl_compositor::WlCompositor, ()> for GuayusaWaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &WaylandConnection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for GuayusaWaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &WaylandConnection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for GuayusaWaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpIdleInhibitManagerV1,
        _event: zwp_idle_inhibit_manager_v1::Event,
        _data: &(),
        _conn: &WaylandConnection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitorV1, ()> for GuayusaWaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpIdleInhibitorV1,
        _event: zwp_idle_inhibitor_v1::Event,
        _data: &(),
        _conn: &WaylandConnection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

pub async fn setup_wayland() -> Result<(
    WaylandConnection,
    wayland_client::EventQueue<GuayusaWaylandState>,
    GuayusaWaylandState,
)> {
    // Use a oneshot channel for better error propagation
    let (tx, rx) = tokio::sync::oneshot::channel();

    // Spawn just one thread for the blocking operations
    std::thread::spawn(move || {
        let result = (|| {
            let conn = WaylandConnection::connect_to_env()
                .context("Failed to connect to Wayland display")?;

            log::debug!("Connected to Wayland display");

            let display = conn.display();
            let mut event_queue = conn.new_event_queue();
            let qh = event_queue.handle();

            log::debug!("Created event queue");

            let _registry = display.get_registry(&qh, ());
            let mut state = GuayusaWaylandState::new();

            // Do the blocking dispatch in this dedicated thread
            event_queue.blocking_dispatch(&mut state)?;

            log::debug!("Registry roundtrip completed");

            if state.compositor.is_none() {
                bail!("Failed to get wl_compositor");
            }
            if state.idle_inhibit_manager.is_none() {
                bail!(
                    "Failed to get zwp_idle_inhibit_manager_v1. Is your compositor supporting this protocol?"
                );
            }

            let surface = state.compositor.as_ref().unwrap().create_surface(&qh, ());
            state.surface = Some(surface);

            log::debug!("Surface created");

            // Flush and process immediate responses
            if let Err(e) = event_queue.flush() {
                bail!("Failed to flush Wayland connection: {}", e);
            }

            // Process any immediate responses without blocking
            if let Err(e) = event_queue.dispatch_pending(&mut state) {
                log::warn!("Error dispatching pending events during setup: {}", e);
            }

            log::debug!("Surface setup completed");

            Ok((conn, event_queue, state))
        })();

        let _ = tx.send(result);
    });

    // Wait for the thread to complete
    rx.await.context("Wayland setup thread panicked")?
}

// Event-driven Wayland event loop using AsyncFd for true event-driven processing
pub async fn wayland_event_loop(
    connection: WaylandConnection,
    event_queue: wayland_client::EventQueue<GuayusaWaylandState>,
    guayusa_state: GuayusaWaylandState,
    mut receiver: mpsc::UnboundedReceiver<InhibitorMessage>,
    status: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let qh = event_queue.handle();
    log::info!("Starting Wayland event loop");

    // Get Wayland's connection file descriptor for async monitoring
    let wayland_fd = connection.backend().poll_fd().as_raw_fd();

    // Create async watcher for the Wayland file descriptor
    let wayland_async_fd = AsyncFd::new(wayland_fd)?;

    // Use parking_lot mutex for better performance
    let mutex_state = Arc::new(Mutex::new((event_queue, guayusa_state)));

    // Heartbeat setup - integrated into select! for efficiency
    let heartbeat_duration = std::time::Duration::from_secs(300);
    let mut heartbeat_interval = tokio::time::interval_at(
        tokio::time::Instant::now() + heartbeat_duration,
        heartbeat_duration,
    );

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Shutdown signal received, cleaning up Wayland resources");
            break;
        }

        tokio::select! {
            // Wayland events - no timeout, purely event-driven for maximum efficiency
            result = wayland_async_fd.readable() => {
                match result {
                    Ok(mut fd_guard) => {
                        log::debug!("Processing Wayland event");
                        let mut state_guard = mutex_state.lock();
                        let (queue, state) = &mut *state_guard;

                        // Process available events with batch processing for efficiency
                        if let Err(e) = flush_and_dispatch_batch(queue, state, false, 5) {
                            log::error!("Error dispatching Wayland events: {}", e);
                        }

                        // Clear readiness and prepare for next event
                        fd_guard.clear_ready();
                    }
                    Err(e) => {
                        log::error!("Error waiting for Wayland events: {}", e);
                        break;
                    }
                }
            }

            // Process messages from the receiver - optimized mutex usage
            Some(message) = receiver.recv() => {
                let mut state_guard = mutex_state.lock(); // Lock once for the whole operation
                let (queue, state) = &mut *state_guard;

                match message {
                    InhibitorMessage::Enable => {
                        if !state.is_inhibited() {
                            if let Err(e) = state.create_inhibitor(&qh) {
                                log::error!("Failed to create inhibitor: {}", e);
                            } else {
                                if let Err(e) = flush_and_dispatch(queue, state, true) {
                                    log::error!("Error flushing after creating inhibitor: {}", e);
                                } else {
                                    status.store(true, Ordering::Relaxed);
                                    log::info!("Idle inhibition enabled");
                                }
                            }
                        } else {
                            log::debug!("Inhibitor already enabled, no action taken.");
                        }
                    }
                    InhibitorMessage::Disable => {
                        if state.is_inhibited() {
                            state.destroy_inhibitor();
                            if let Err(e) = flush_and_dispatch(queue, state, true) {
                                log::error!("Error flushing after destroying inhibitor: {}", e);
                            } else {
                                status.store(false, Ordering::Relaxed);
                                log::info!("Idle inhibition disabled");
                            }
                        } else {
                            log::debug!("Inhibitor already disabled, no action taken.");
                        }
                    }
                    InhibitorMessage::Toggle => {
                        let currently_inhibited = state.is_inhibited();
                        log::debug!("Toggle called, current state: inhibited={}", currently_inhibited);

                        if currently_inhibited {
                            // Currently enabled, so disable
                            state.destroy_inhibitor();
                            if let Err(e) = flush_and_dispatch(queue, state, true) {
                                log::error!("Error flushing after destroying inhibitor: {}", e);
                            } else {
                                status.store(false, Ordering::Relaxed);
                                log::info!("Idle inhibition toggled: disabled");
                            }
                        } else {
                            // Currently disabled, so enable
                            if let Err(e) = state.create_inhibitor(&qh) {
                                log::error!("Failed to create inhibitor: {}", e);
                            } else {
                                if let Err(e) = flush_and_dispatch(queue, state, true) {
                                    log::error!("Error flushing after creating inhibitor: {}", e);
                                } else {
                                    status.store(true, Ordering::Relaxed);
                                    log::info!("Idle inhibition toggled: enabled");
                                }
                            }
                        }
                    }
                }
            }

            // Heartbeat timer - integrated into select! for efficiency
            _ = heartbeat_interval.tick() => {
                log::debug!("Performing Wayland connection heartbeat check");
                if connection.backend().flush().is_err() {
                    log::error!("Wayland connection unhealthy (flush failed), exiting event loop");
                    break;
                }
                log::debug!("Wayland connection heartbeat OK");
            }

            // This branch handles the case where receiver is closed
            else => {
                log::info!("Command receiver channel closed, exiting event loop.");
                break;
            }
        }
    }

    // Clean up resources
    log::info!("Cleaning up Wayland resources before exiting event loop.");
    let mut final_state_guard = mutex_state.lock();
    let (queue, state) = &mut *final_state_guard;
    state.cleanup();
    // Ensure any final messages (like destroy) from cleanup are sent
    if let Err(e) = queue.flush() {
        log::warn!("Failed to flush Wayland queue during final cleanup: {}", e);
    }
    drop(final_state_guard);

    Ok(())
}
