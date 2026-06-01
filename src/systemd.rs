use anyhow::{Context, Result, bail};
use std::env;
use tokio::sync::{Mutex, watch};
use zbus::{Connection, Proxy, zvariant::OwnedObjectPath};

const SYSTEMD_SERVICE: &str = "org.freedesktop.systemd1";
const SYSTEMD_MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";
const SYSTEMD_UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";

const DEFAULT_LOCK_UNIT: &str = "guayusa-swayidle-lock.service";
const DEFAULT_SUSPEND_UNIT: &str = "guayusa-swayidle-suspend.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    NoSuspend,
    NoIdle,
    Custom,
    Unavailable,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::NoSuspend => "no-suspend",
            Mode::NoIdle => "no-idle",
            Mode::Custom => "custom",
            Mode::Unavailable => "unavailable",
        }
    }

    pub fn parse_primary(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Mode::Normal),
            "no-suspend" => Some(Mode::NoSuspend),
            "no-idle" => Some(Mode::NoIdle),
            _ => None,
        }
    }

    pub fn is_inhibited(self) -> bool {
        matches!(self, Mode::NoSuspend | Mode::NoIdle | Mode::Custom)
    }

    pub fn next_cycle(self) -> Self {
        match self {
            Mode::Normal => Mode::NoSuspend,
            Mode::NoSuspend => Mode::NoIdle,
            Mode::NoIdle => Mode::Normal,
            Mode::Custom | Mode::Unavailable => Mode::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitState {
    Active,
    Inactive,
    Unavailable,
}

pub struct SystemdController {
    connection: Connection,
    lock_unit: String,
    suspend_unit: String,
    mode_sender: watch::Sender<Mode>,
    operation_lock: Mutex<()>,
}

impl SystemdController {
    pub fn new(connection: Connection, mode_sender: watch::Sender<Mode>) -> Self {
        let lock_unit =
            env::var("GUAYUSA_LOCK_UNIT").unwrap_or_else(|_| DEFAULT_LOCK_UNIT.to_string());
        let suspend_unit =
            env::var("GUAYUSA_SUSPEND_UNIT").unwrap_or_else(|_| DEFAULT_SUSPEND_UNIT.to_string());

        Self {
            connection,
            lock_unit,
            suspend_unit,
            mode_sender,
            operation_lock: Mutex::new(()),
        }
    }

    pub async fn set_mode(&self, mode: Mode) -> Result<Mode> {
        if !matches!(mode, Mode::Normal | Mode::NoSuspend | Mode::NoIdle) {
            bail!("mode {} cannot be set directly", mode.as_str());
        }

        let _guard = self.operation_lock.lock().await;
        match mode {
            Mode::Normal => {
                self.start_unit(&self.lock_unit).await?;
                self.start_unit(&self.suspend_unit).await?;
            }
            Mode::NoSuspend => {
                self.start_unit(&self.lock_unit).await?;
                self.stop_unit(&self.suspend_unit).await?;
            }
            Mode::NoIdle => {
                self.stop_unit(&self.lock_unit).await?;
                self.stop_unit(&self.suspend_unit).await?;
            }
            Mode::Custom | Mode::Unavailable => unreachable!(),
        }

        self.refresh_mode_locked().await
    }

    pub async fn cycle_mode(&self) -> Result<Mode> {
        let _guard = self.operation_lock.lock().await;
        let next_mode = self.refresh_mode_locked().await?.next_cycle();

        match next_mode {
            Mode::Normal => {
                self.start_unit(&self.lock_unit).await?;
                self.start_unit(&self.suspend_unit).await?;
            }
            Mode::NoSuspend => {
                self.start_unit(&self.lock_unit).await?;
                self.stop_unit(&self.suspend_unit).await?;
            }
            Mode::NoIdle => {
                self.stop_unit(&self.lock_unit).await?;
                self.stop_unit(&self.suspend_unit).await?;
            }
            Mode::Custom | Mode::Unavailable => unreachable!(),
        }

        self.refresh_mode_locked().await
    }

    pub async fn refresh_mode(&self) -> Result<Mode> {
        let _guard = self.operation_lock.lock().await;
        self.refresh_mode_locked().await
    }

    async fn refresh_mode_locked(&self) -> Result<Mode> {
        let mode = infer_mode(
            self.unit_state(&self.lock_unit).await,
            self.unit_state(&self.suspend_unit).await,
        );
        if *self.mode_sender.borrow() != mode {
            let _ = self.mode_sender.send(mode);
        }
        Ok(mode)
    }

    async fn manager_proxy(&self) -> Result<Proxy<'_>> {
        Proxy::new(
            &self.connection,
            SYSTEMD_SERVICE,
            SYSTEMD_MANAGER_PATH,
            SYSTEMD_MANAGER_INTERFACE,
        )
        .await
        .context("failed to create systemd manager proxy")
    }

    async fn start_unit(&self, unit: &str) -> Result<()> {
        let proxy = self.manager_proxy().await?;
        let _: OwnedObjectPath = proxy
            .call("StartUnit", &(unit, "replace"))
            .await
            .with_context(|| format!("failed to start {unit}"))?;
        Ok(())
    }

    async fn stop_unit(&self, unit: &str) -> Result<()> {
        let proxy = self.manager_proxy().await?;
        let _: OwnedObjectPath = proxy
            .call("StopUnit", &(unit, "replace"))
            .await
            .with_context(|| format!("failed to stop {unit}"))?;
        Ok(())
    }

    async fn unit_state(&self, unit: &str) -> UnitState {
        match self.load_unit_state(unit).await {
            Ok(state) => state,
            Err(e) => {
                log::warn!("Unable to read state for {}: {}", unit, e);
                UnitState::Unavailable
            }
        }
    }

    async fn load_unit_state(&self, unit: &str) -> Result<UnitState> {
        let manager = self.manager_proxy().await?;
        let path: OwnedObjectPath = manager
            .call("LoadUnit", &(unit))
            .await
            .with_context(|| format!("failed to load {unit}"))?;

        let unit_proxy = Proxy::new(
            &self.connection,
            SYSTEMD_SERVICE,
            path.as_str(),
            SYSTEMD_UNIT_INTERFACE,
        )
        .await
        .with_context(|| format!("failed to create proxy for {unit}"))?;

        let active_state: String = unit_proxy
            .get_property("ActiveState")
            .await
            .with_context(|| format!("failed to read ActiveState for {unit}"))?;

        Ok(match active_state.as_str() {
            "active" | "activating" | "reloading" => UnitState::Active,
            _ => UnitState::Inactive,
        })
    }
}

fn infer_mode(lock: UnitState, suspend: UnitState) -> Mode {
    match (lock, suspend) {
        (UnitState::Active, UnitState::Active) => Mode::Normal,
        (UnitState::Active, UnitState::Inactive) => Mode::NoSuspend,
        (UnitState::Inactive, UnitState::Inactive) => Mode::NoIdle,
        (UnitState::Inactive, UnitState::Active) => Mode::Custom,
        (UnitState::Unavailable, _) | (_, UnitState::Unavailable) => Mode::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, UnitState, infer_mode};

    #[test]
    fn parses_only_primary_modes() {
        assert_eq!(Mode::parse_primary("normal"), Some(Mode::Normal));
        assert_eq!(Mode::parse_primary("no-suspend"), Some(Mode::NoSuspend));
        assert_eq!(Mode::parse_primary("no-idle"), Some(Mode::NoIdle));
        assert_eq!(Mode::parse_primary("custom"), None);
        assert_eq!(Mode::parse_primary("unavailable"), None);
    }

    #[test]
    fn reports_legacy_status() {
        assert!(!Mode::Normal.is_inhibited());
        assert!(Mode::NoSuspend.is_inhibited());
        assert!(Mode::NoIdle.is_inhibited());
        assert!(Mode::Custom.is_inhibited());
        assert!(!Mode::Unavailable.is_inhibited());
    }

    #[test]
    fn cycles_primary_modes() {
        assert_eq!(Mode::Normal.next_cycle(), Mode::NoSuspend);
        assert_eq!(Mode::NoSuspend.next_cycle(), Mode::NoIdle);
        assert_eq!(Mode::NoIdle.next_cycle(), Mode::Normal);
        assert_eq!(Mode::Custom.next_cycle(), Mode::Normal);
        assert_eq!(Mode::Unavailable.next_cycle(), Mode::Normal);
    }

    #[test]
    fn infers_modes_from_unit_states() {
        assert_eq!(
            infer_mode(UnitState::Active, UnitState::Active),
            Mode::Normal
        );
        assert_eq!(
            infer_mode(UnitState::Active, UnitState::Inactive),
            Mode::NoSuspend
        );
        assert_eq!(
            infer_mode(UnitState::Inactive, UnitState::Inactive),
            Mode::NoIdle
        );
        assert_eq!(
            infer_mode(UnitState::Inactive, UnitState::Active),
            Mode::Custom
        );
        assert_eq!(
            infer_mode(UnitState::Unavailable, UnitState::Active),
            Mode::Unavailable
        );
    }
}
