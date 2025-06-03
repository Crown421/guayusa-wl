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
    // Only flush when needed and return early on error
    if needs_flush && event_queue.flush().is_err() {
        return Err(anyhow::anyhow!("Failed to flush Wayland connection"));
    }

    // Immediately return dispatch results without extra error processing
    event_queue
        .dispatch_pending(state)
        .map_err(|e| anyhow::anyhow!("Dispatch error: {}", e))
        .map(|_| ()) // Convert usize result to ()
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
        self.destroy_inhibitor();
        if let Some(surface) = self.surface.take() {
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

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Shutdown signal received, cleaning up Wayland resources");
            break;
        }

        tokio::select! {
            // Only wake up when Wayland has events to process
            wayland_ready = wayland_async_fd.readable() => {
                match wayland_ready {
                    Ok(mut guard) => {
                        log::debug!("Processing Wayland event");
                        let state_lock = mutex_state.clone();
                        let mut state_guard = state_lock.lock();
                        let (queue, state) = &mut *state_guard;

                        // Process available events without blocking
                        if let Err(e) = queue.dispatch_pending(state) {
                            log::error!("Error dispatching Wayland events: {}", e);
                        }

                        // Clear readiness and prepare for next event
                        guard.clear_ready();
                    }
                    Err(e) => {
                        log::error!("Error waiting for Wayland events: {}", e);
                        break;
                    }
                }
            }

            // Process messages from the receiver
            Some(message) = receiver.recv() => {
                let state_lock = mutex_state.clone();
                let mut state_guard = state_lock.lock();
                let (queue, state) = &mut *state_guard;

                match message {
                    InhibitorMessage::Enable => {
                        if !state.is_inhibited() {
                            if let Err(e) = state.create_inhibitor(&qh) {
                                log::error!("Failed to create inhibitor: {}", e);
                            } else {
                                let _ = flush_and_dispatch(queue, state, true);
                                status.store(true, Ordering::Relaxed);
                                log::info!("Idle inhibition enabled");
                            }
                        }
                    }
                    InhibitorMessage::Disable => {
                        if state.is_inhibited() {
                            state.destroy_inhibitor();
                            let _ = flush_and_dispatch(queue, state, true);
                            status.store(false, Ordering::Relaxed);
                            log::info!("Idle inhibition disabled");
                        }
                    }
                }
            }

            else => break,
        }
    }

    // Clean up resources
    let mut final_state_guard = mutex_state.lock();
    final_state_guard.1.cleanup();

    Ok(())
}
