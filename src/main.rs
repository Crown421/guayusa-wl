use anyhow::{bail, Context, Result};
use clap::Parser;
use shared_memory::{Shmem, ShmemConf};
use signal_hook::{
    consts::{SIGINT, SIGTERM, SIGUSR1},
    iterator::Signals,
};
use std::{
    io::Write, // For writing to stdout
    mem,
    process,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use wayland_client::{
    protocol::{wl_compositor, wl_registry, wl_surface},
    Connection, Dispatch, QueueHandle,
};

// Use the idle inhibit protocol from wayland-protocols crate
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};

const HELP_MSG: &str = "Usage: matcha_rust [MODE] [OPTION]...\n\
MODE:\n\
  -d, --daemon     Main instance (Daemon Mode)\n\
  -t, --toggle     Toggle instance (Toggle Mode)\n\
  -s, --status     Get status of inhibitor (Status Mode)\n\n\
Options:\n\
  -o, --off        Start daemon with inhibitor off\n\
  -h, --help       Display this help and exit";

const SHARED_MEM_NAME: &str = "/matcha-idle-inhibit-rust";
const SIGNAL_FILE: &str = "/tmp/matcha-idle-signal-rust"; // Signal file for IPC

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
enum AppSignalState {
    Inhibit = 0,
    Uninhibit = 1,
    Kill = 2,
}

impl AppSignalState {
    fn from_u8(val: u8) -> Self {
        match val {
            0 => AppSignalState::Inhibit,
            1 => AppSignalState::Uninhibit,
            _ => AppSignalState::Kill, // Default to Kill or handle error
        }
    }
}

// Data stored in shared memory
// Needs to be simple, Copy, and have a defined layout.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct SharedMemData {
    inhibit: bool,
    // The semaphore is now named and managed externally, not in shmem
}

#[derive(Parser, Debug)]
#[clap(override_help = HELP_MSG, version = "0.1.0", author = "Rust Developer")]
struct Args {
    #[clap(short, long, help = "Main instance (Daemon Mode)")]
    daemon: bool,

    #[clap(short, long, help = "Toggle instance (Toggle Mode)")]
    toggle: bool,

    #[clap(short, long, help = "Get status of inhibitor (Status Mode)")]
    status: bool,

    #[clap(
        short,
        long,
        help = "Start daemon with inhibitor off (only with --daemon)"
    )]
    off: bool,
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
        // Note: idle_inhibit_manager and compositor don't have destroy methods in this version
    }
}

// Implement Dispatch for Wayland objects we care about
impl Dispatch<wl_registry::WlRegistry, ()> for MatchaWaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
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

// Dummy dispatch for objects we bind but don't need to handle events for (yet)
impl Dispatch<wl_compositor::WlCompositor, ()> for MatchaWaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
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
        _conn: &Connection,
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
        _conn: &Connection,
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
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

fn get_shared_mem_data(shmem: &mut Shmem) -> Result<SharedMemData> {
    // Easiest is to copy from the raw pointer.
    let shmem_ptr = shmem.as_ptr();
    let data = unsafe { ptr::read_volatile(shmem_ptr as *const SharedMemData) };
    Ok(data)
}

fn set_shared_mem_data(shmem: &mut Shmem, data: &SharedMemData) -> Result<()> {
    let shmem_ptr = shmem.as_ptr() as *mut SharedMemData;
    unsafe { ptr::write_volatile(shmem_ptr, *data) };
    Ok(())
}

fn daemon_mode(start_inhibited: bool) -> Result<()> {
    log::info!(
        "Starting daemon mode. Initial inhibition: {}",
        start_inhibited
    );

    // --- Shared Memory Setup ---
    let mut shmem = ShmemConf::new()
        .size(mem::size_of::<SharedMemData>())
        .flink(SHARED_MEM_NAME)
        .create()
        .context("Failed to create shared memory. Another instance running?")?;

    let initial_shmem_data = SharedMemData {
        inhibit: start_inhibited,
    };
    set_shared_mem_data(&mut shmem, &initial_shmem_data)?;
    log::debug!("Initialized shared memory with: {:?}", initial_shmem_data);

    // --- Signal File Setup ---
    // Remove existing signal file
    let _ = std::fs::remove_file(SIGNAL_FILE);

    // Create signal file
    std::fs::File::create(SIGNAL_FILE).context("Failed to create signal file")?;

    // --- Signal Handling Setup ---
    let signal_state = Arc::new(AtomicU8::new(AppSignalState::Inhibit as u8)); // Start with assuming inhibit based on logic flow
    let term_signal_received = Arc::new(AtomicBool::new(false));

    let mut signals =
        Signals::new(&[SIGUSR1, SIGINT, SIGTERM]).context("Failed to register signal handlers")?;

    let signal_state_clone = Arc::clone(&signal_state);
    let term_signal_clone = Arc::clone(&term_signal_received);

    std::thread::spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGUSR1 => {
                    log::info!("SIGUSR1 received, toggling internal signal state.");
                    let current =
                        AppSignalState::from_u8(signal_state_clone.load(Ordering::SeqCst));
                    if current == AppSignalState::Inhibit {
                        signal_state_clone.store(AppSignalState::Uninhibit as u8, Ordering::SeqCst);
                    } else if current == AppSignalState::Uninhibit {
                        // Only toggle between Inhibit and Uninhibit
                        signal_state_clone.store(AppSignalState::Inhibit as u8, Ordering::SeqCst);
                    }
                }
                SIGINT | SIGTERM => {
                    log::info!("SIGINT/SIGTERM received, initiating shutdown.");
                    signal_state_clone.store(AppSignalState::Kill as u8, Ordering::SeqCst);
                    term_signal_clone.store(true, Ordering::SeqCst);
                    // Signal the main loop by touching the signal file
                    let _ = std::fs::write(SIGNAL_FILE, b"kill");
                    break; // Exit signal handling thread
                }
                _ => log::warn!("Received unhandled signal: {}", signal),
            }
        }
    });

    // --- Wayland Setup ---
    let conn = Connection::connect_to_env().context("Failed to connect to Wayland display")?;
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
        bail!("Failed to get zwp_idle_inhibit_manager_v1. Is your compositor sway or similar supporting this protocol?");
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

    // --- Main Loop ---
    // Initialize internal state based on shared memory.
    // This 'signal_state' is influenced by actual signals (SIGUSR1)
    // and the 'shmem_data.inhibit' is influenced by the toggle command.
    // The effective inhibition depends on both.
    if start_inhibited {
        matcha_state
            .create_inhibitor(&qh)
            .context("Failed to create initial inhibitor")?;
        signal_state.store(AppSignalState::Inhibit as u8, Ordering::SeqCst);
        log::info!("Daemon started with inhibitor ON");
        println!("inhibit|bool|true");
        std::io::stdout().flush()?;
    } else {
        // No inhibitor created initially
        signal_state.store(AppSignalState::Uninhibit as u8, Ordering::SeqCst);
        log::info!("Daemon started with inhibitor OFF");
        println!("inhibit|bool|false");
        std::io::stdout().flush()?;
    }
    event_queue.blocking_dispatch(&mut matcha_state)?;

    log::info!("Daemon running. Waiting for signals or file changes.");
    let mut last_modified = std::fs::metadata(SIGNAL_FILE)
        .ok()
        .and_then(|m| m.modified().ok());

    while AppSignalState::from_u8(signal_state.load(Ordering::Relaxed)) != AppSignalState::Kill {
        // Check for termination signal
        if term_signal_received.load(Ordering::Relaxed) {
            log::info!("Termination signal received, exiting loop.");
            break;
        }

        // Check if signal file was modified (indicates toggle command)
        if let Ok(metadata) = std::fs::metadata(SIGNAL_FILE) {
            if let Ok(modified) = metadata.modified() {
                if last_modified.map_or(true, |last| modified > last) {
                    last_modified = Some(modified);
                    // File was modified, continue to process state change
                }
            }
        }

        // Sleep briefly to avoid busy waiting
        std::thread::sleep(Duration::from_millis(100));

        // Process Wayland events that might have accumulated
        event_queue.dispatch_pending(&mut matcha_state)?;

        let current_sig_state = AppSignalState::from_u8(signal_state.load(Ordering::SeqCst));
        if current_sig_state == AppSignalState::Kill {
            log::debug!("Kill signal detected in main loop.");
            break;
        }

        let shmem_data =
            get_shared_mem_data(&mut shmem).context("Failed to read shared memory in loop")?;
        log::debug!(
            "Loop: signal_state={:?}, shmem_data.inhibit={}",
            current_sig_state,
            shmem_data.inhibit
        );

        // Determine desired state:
        // - If SIGUSR1 toggled to UNINHIBIT, or toggle command set shmem to false: turn off inhibitor
        // - Else (SIGUSR1 toggled to INHIBIT and toggle command set shmem to true): turn on inhibitor
        let should_inhibit = shmem_data.inhibit && (current_sig_state == AppSignalState::Inhibit);

        if should_inhibit {
            if matcha_state.inhibitor.is_none() {
                log::info!("Activating inhibitor due to state change.");
                matcha_state
                    .create_inhibitor(&qh)
                    .context("Failed to create inhibitor in loop")?;
                event_queue.blocking_dispatch(&mut matcha_state)?;
                println!("inhibit|bool|true");
                std::io::stdout().flush()?;
            }
        } else {
            if matcha_state.inhibitor.is_some() {
                log::info!("Deactivating inhibitor due to state change.");
                matcha_state.destroy_inhibitor();
                event_queue.blocking_dispatch(&mut matcha_state)?;
                println!("inhibit|bool|false");
                std::io::stdout().flush()?;
            }
        }
    }

    log::info!("Daemon shutting down...");
    matcha_state.cleanup();
    event_queue.blocking_dispatch(&mut matcha_state).ok(); // Final flush

    let _ = ShmemConf::new()
        .flink(SHARED_MEM_NAME)
        .open()
        .map(|shm| drop(shm)); // Just drop it, no set_removed_on_drop method
    let _ = std::fs::remove_file(SIGNAL_FILE);
    log::info!("Cleanup complete. Exiting.");
    Ok(())
}

fn toggle_mode() -> Result<()> {
    log::debug!("Toggle mode: Connecting to shared memory and signal file.");
    let mut shmem = ShmemConf::new()
        .flink(SHARED_MEM_NAME)
        .open()
        .context("Failed to open shared memory. Is the daemon running?")?;

    let mut data =
        get_shared_mem_data(&mut shmem).context("Failed to read shared memory for toggle")?;
    data.inhibit = !data.inhibit;
    set_shared_mem_data(&mut shmem, &data).context("Failed to write shared memory for toggle")?;

    // Signal the daemon by writing to the signal file
    std::fs::write(SIGNAL_FILE, b"toggle").context("Failed to write to signal file")?;

    log::info!(
        "Toggled inhibit state to: {}. Signal file updated.",
        data.inhibit
    );
    println!(
        "Set inhibit to: {}",
        if data.inhibit { "on" } else { "off" }
    );
    Ok(())
}

fn status_mode() -> Result<()> {
    log::debug!("Status mode: Connecting to shared memory.");
    let mut shmem = ShmemConf::new()
        .flink(SHARED_MEM_NAME)
        .open()
        .context("Failed to open shared memory. Is the daemon running?")?;

    let data =
        get_shared_mem_data(&mut shmem).context("Failed to read shared memory for status")?;
    println!("{}", if data.inhibit { "on" } else { "off" });
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    let mode_count = [args.daemon, args.toggle, args.status]
        .iter()
        .filter(|&&x| x)
        .count();

    if mode_count != 1 {
        eprintln!("ERROR: You must specify exactly one of --daemon, --toggle, or --status.\n");
        eprintln!("{}", HELP_MSG);
        process::exit(1);
    }

    if args.daemon {
        let start_inhibited = !args.off;
        daemon_mode(start_inhibited)
    } else if args.toggle {
        toggle_mode()
    } else if args.status {
        status_mode()
    } else {
        // Should be caught by mode_count, but as a fallback:
        eprintln!("{}", HELP_MSG);
        process::exit(1);
    }
}
