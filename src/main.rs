use anyhow::{bail, Context, Result};
use futures::StreamExt;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook_tokio::Signals;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::sleep, time::timeout};
use zbus::{interface, ConnectionBuilder};

use wayland_client::{
    protocol::{wl_compositor, wl_registry, wl_surface},
    Connection as WaylandConnection, Dispatch, QueueHandle,
};

// Use the idle inhibit protocol from wayland-protocols crate
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};

const DBUS_OBJECT_PATH: &str = "/org/matcha/IdleInhibitor";
const DBUS_SERVICE_NAME: &str = "org.matcha.IdleInhibitor";

// Message types for internal communication
#[derive(Debug, Clone)]
enum InhibitorMessage {
    Enable,
    Disable,
    Shutdown,
}

// Wayland state structure
struct MatchaWaylandState {
    compositor: Option<wl_compositor::WlCompositor>,
    idle_inhibit_manager: Option<ZwpIdleInhibitManagerV1>,
    surface: Option<wl_surface::WlSurface>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
}

impl MatchaWaylandState {
    fn new() -> Self {
        MatchaWaylandState {
            compositor: None,
            idle_inhibit_manager: None,
            surface: None,
            inhibitor: None,
        }
    }

    fn create_inhibitor(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if self.inhibitor.is_some() {
            log::debug!("Inhibitor already exists.");
            return Ok(());
        }
        let manager = self
            .idle_inhibit_manager
            .as_ref()
            .context("Idle inhibit manager not available")?;
        let surface = self.surface.as_ref().context("Surface not available")?;

        log::info!("Creating idle inhibitor.");
        self.inhibitor = Some(manager.create_inhibitor(surface, qh, ()));
        surface.commit();
        Ok(())
    }

    fn destroy_inhibitor(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            log::info!("Destroying idle inhibitor.");
            inhibitor.destroy();
            if let Some(surface) = &self.surface {
                surface.commit();
            }
        }
    }

    fn cleanup(&mut self) {
        log::debug!("Cleaning up Wayland resources.");
        self.destroy_inhibitor();
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
    }

    fn is_inhibited(&self) -> bool {
        self.inhibitor.is_some()
    }
}

// Implement Dispatch for Wayland objects we care about
impl Dispatch<wl_registry::WlRegistry, ()> for MatchaWaylandState {
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
            log::debug!(target: "matcha_rust::wayland", "Global: name={}, interface={}, version={}", name, interface, version);
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
impl Dispatch<wl_compositor::WlCompositor, ()> for MatchaWaylandState {
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

impl Dispatch<wl_surface::WlSurface, ()> for MatchaWaylandState {
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

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for MatchaWaylandState {
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

impl Dispatch<ZwpIdleInhibitorV1, ()> for MatchaWaylandState {
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

// D-Bus interface for the idle inhibitor
struct IdleInhibitorInterface {
    sender: mpsc::UnboundedSender<InhibitorMessage>,
    status: Arc<AtomicBool>,
}

#[interface(name = "org.matcha.IdleInhibitor")]
impl IdleInhibitorInterface {
    /// Enable idle inhibition
    fn enable(&self) -> zbus::fdo::Result<()> {
        log::info!("D-Bus: Enable method called");
        if let Err(e) = self.sender.send(InhibitorMessage::Enable) {
            log::error!("Failed to send enable message: {}", e);
            return Err(zbus::fdo::Error::Failed(format!("Failed to send enable message: {}", e)));
        }
        Ok(())
    }

    /// Disable idle inhibition
    fn disable(&self) -> zbus::fdo::Result<()> {
        log::info!("D-Bus: Disable method called");
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

async fn setup_wayland() -> Result<(
    WaylandConnection,
    wayland_client::EventQueue<MatchaWaylandState>,
    MatchaWaylandState,
)> {
    let conn = WaylandConnection::connect_to_env().context("Failed to connect to Wayland display")?;
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let _registry = display.get_registry(&qh, ());
    let mut matcha_state = MatchaWaylandState::new();

    // Initial roundtrip to get globals
    event_queue.blocking_dispatch(&mut matcha_state)?;

    if matcha_state.compositor.is_none() {
        bail!("Failed to get wl_compositor");
    }
    if matcha_state.idle_inhibit_manager.is_none() {
        bail!("Failed to get zwp_idle_inhibit_manager_v1. Is your compositor supporting this protocol?");
    }

    let surface = matcha_state
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    matcha_state.surface = Some(surface);
    
    // Initial commit might be needed by some compositors for the surface to be valid
    if let Some(s) = &matcha_state.surface {
        s.commit();
    }
    event_queue.blocking_dispatch(&mut matcha_state)?;

    Ok((conn, event_queue, matcha_state))
}

async fn wayland_event_loop(
    mut event_queue: wayland_client::EventQueue<MatchaWaylandState>,
    mut matcha_state: MatchaWaylandState,
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
        if let Err(e) = event_queue.dispatch_pending(&mut matcha_state) {
            log::error!("Error dispatching Wayland events: {}", e);
        }

        // Handle D-Bus messages
        while let Ok(message) = receiver.try_recv() {
            match message {
                InhibitorMessage::Enable => {
                    if !matcha_state.is_inhibited() {
                        if let Err(e) = matcha_state.create_inhibitor(&qh) {
                            log::error!("Failed to create inhibitor: {}", e);
                        } else {
                            status.store(true, Ordering::Relaxed);
                            log::info!("Idle inhibition enabled");
                        }
                    }
                }
                InhibitorMessage::Disable => {
                    if matcha_state.is_inhibited() {
                        matcha_state.destroy_inhibitor();
                        status.store(false, Ordering::Relaxed);
                        log::info!("Idle inhibition disabled");
                    }
                }
                InhibitorMessage::Shutdown => {
                    log::info!("Shutdown message received");
                    shutdown.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        // Small sleep to prevent busy waiting
        sleep(Duration::from_millis(10)).await;
    }

    matcha_state.cleanup();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Starting Matcha Idle Inhibitor D-Bus service");

    // Set up Wayland
    let (_wayland_conn, event_queue, matcha_state) = setup_wayland().await?;

    // Should be debug
    log::info!("Wayland setup done");

    // Set up communication channels
    let (sender, receiver) = mpsc::unbounded_channel();
    let status = Arc::new(AtomicBool::new(false)); // Start with inhibition OFF
    let shutdown = Arc::new(AtomicBool::new(false));

    // Should be debug
    log::info!("Comm channel setup done");

    // Set up D-Bus service
    let idle_inhibitor = IdleInhibitorInterface {
        sender: sender.clone(),
        status: Arc::clone(&status),
    };

    // Should be debug
    log::info!("Inhibitor interface setup done");

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
    let wayland_task = tokio::spawn(wayland_event_loop(
        event_queue,
        matcha_state,
        receiver,
        status,
        Arc::clone(&shutdown),
    ));

    // Keep the D-Bus connection alive by spawning a task
    let shutdown_clone_dbus = Arc::clone(&shutdown);
    let dbus_task = tokio::spawn(async move {
        // Keep the connection alive by waiting for it to close
        let _connection = dbus_connection;
        log::info!("D-Bus connection established successfully");

        // Wait for shutdown signal
        while !shutdown_clone_dbus.load(Ordering::Relaxed) {
            // Don't call any methods that would close the connection
            // Just sleep and keep the connection in scope
            sleep(Duration::from_millis(100)).await;
        }
        log::info!("D-Bus connection task shutting down");
    });

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
