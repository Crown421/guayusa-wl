use anyhow::{Context, Result, bail};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::sleep};

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

// Helper function to flush and dispatch Wayland events
fn flush_and_dispatch(
    event_queue: &mut wayland_client::EventQueue<GuayusaWaylandState>,
    state: &mut GuayusaWaylandState,
    context: &str,
) {
    if let Err(e) = event_queue.flush() {
        log::error!("Error flushing Wayland connection after {}: {}", context, e);
    }
    if let Err(e) = event_queue.dispatch_pending(state) {
        log::error!("Error dispatching events after {}: {}", context, e);
    }
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
    // Try connecting with better error context
    let conn = tokio::task::spawn_blocking(move || {
        WaylandConnection::connect_to_env().context("Failed to connect to Wayland display")
    })
    .await
    .context("Wayland connection task panicked")?
    .context("Failed to establish Wayland connection")?;

    log::debug!("Connected to Wayland display");

    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    log::debug!("Created event queue");

    let _registry = display.get_registry(&qh, ());
    let mut guayusa_state = GuayusaWaylandState::new();

    // Initial roundtrip to get globals with timeout protection
    let (event_queue, guayusa_state) = tokio::task::spawn_blocking({
        move || {
            event_queue.blocking_dispatch(&mut guayusa_state)?;
            Ok::<_, anyhow::Error>((event_queue, guayusa_state))
        }
    })
    .await
    .context("Registry roundtrip task panicked")?
    .context("Failed during registry roundtrip")?;

    log::debug!("Registry roundtrip completed");

    if guayusa_state.compositor.is_none() {
        bail!("Failed to get wl_compositor");
    }
    if guayusa_state.idle_inhibit_manager.is_none() {
        bail!(
            "Failed to get zwp_idle_inhibit_manager_v1. Is your compositor supporting this protocol?"
        );
    }

    let surface = guayusa_state
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let mut guayusa_state = guayusa_state; // Make it mutable again
    guayusa_state.surface = Some(surface);

    log::debug!("Surface created");

    // Use flush and dispatch_pending instead of blocking_dispatch for better async compatibility
    let mut event_queue = event_queue; // Make it mutable again
    if let Err(e) = event_queue.flush() {
        bail!("Failed to flush Wayland connection: {}", e);
    }

    // Process any immediate responses without blocking
    if let Err(e) = event_queue.dispatch_pending(&mut guayusa_state) {
        log::warn!("Error dispatching pending events during setup: {}", e);
    }

    log::debug!("Surface setup completed");

    Ok((conn, event_queue, guayusa_state))
}

pub async fn wayland_event_loop(
    mut event_queue: wayland_client::EventQueue<GuayusaWaylandState>,
    mut guayusa_state: GuayusaWaylandState,
    mut receiver: mpsc::UnboundedReceiver<InhibitorMessage>,
    status: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let qh = event_queue.handle();

    log::info!("Starting Wayland event loop");

    loop {
        // Check for shutdown signal
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Shutdown signal received, cleaning up Wayland resources");
            break;
        }

        // Process Wayland events
        if let Err(e) = event_queue.dispatch_pending(&mut guayusa_state) {
            log::error!("Error dispatching Wayland events: {}", e);
        }

        // Use tokio::select! for more efficient waiting
        tokio::select! {
            message = receiver.recv() => {
                if let Some(message) = message {
                    match message {
                        InhibitorMessage::Enable => {
                            if !guayusa_state.is_inhibited() {
                                if let Err(e) = guayusa_state.create_inhibitor(&qh) {
                                    log::error!("Failed to create inhibitor: {}", e);
                                } else {
                                    flush_and_dispatch(&mut event_queue, &mut guayusa_state, "inhibitor enable");
                                    status.store(true, Ordering::Relaxed);
                                    log::info!("Idle inhibition enabled");
                                }
                            }
                        }
                        InhibitorMessage::Disable => {
                            if guayusa_state.is_inhibited() {
                                guayusa_state.destroy_inhibitor();
                                flush_and_dispatch(&mut event_queue, &mut guayusa_state, "inhibitor disable");
                                status.store(false, Ordering::Relaxed);
                                log::info!("Idle inhibition disabled");
                            }
                        }
                    }
                } else {
                    // Receiver closed, exit
                    log::info!("Message receiver closed, exiting event loop");
                    break;
                }
            }
            _ = sleep(Duration::from_millis(800)) => {
                // Periodic check for shutdown and event processing
                continue;
            }
        }
    }

    guayusa_state.cleanup();
    Ok(())
}
