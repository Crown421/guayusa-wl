use anyhow::{Result, bail};
use tokio::sync::watch;
use zbus::Connection;

use crate::systemd::{Mode, SystemdController, UnitReport};

pub enum Command {
    Daemon,
    Up,
    Status,
    Help,
}

impl Command {
    pub fn parse() -> Result<Self> {
        Self::parse_from(std::env::args().skip(1))
    }

    fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let Some(command) = args.next() else {
            return Ok(Command::Daemon);
        };

        if args.next().is_some() {
            bail!("too many arguments\n\n{}", usage());
        }

        match command.as_str() {
            "up" => Ok(Command::Up),
            "status" => Ok(Command::Status),
            "help" | "-h" | "--help" => Ok(Command::Help),
            other => bail!("unknown command: {other}\n\n{}", usage()),
        }
    }
}

pub async fn run_up() -> Result<()> {
    let controller = cli_controller().await?;
    let reports = controller.up().await?;
    let mode = controller.current_mode().await?;

    println!("Enabled and started Guayusa units.");
    print_mode(mode);
    print_reports(&reports);

    Ok(())
}

pub async fn run_status() -> Result<()> {
    let controller = cli_controller().await?;
    let mode = controller.current_mode().await?;
    let reports = controller.setup_unit_reports().await?;

    print_mode(mode);
    print_reports(&reports);

    Ok(())
}

pub fn print_usage() {
    println!("{}", usage());
}

fn usage() -> &'static str {
    "Usage:
  guayusa             Run the D-Bus service
  guayusa up          Enable and start the packaged user services
  guayusa status      Show Guayusa mode and managed unit state"
}

async fn cli_controller() -> Result<SystemdController> {
    let connection = Connection::session().await?;
    let (mode_sender, _mode_receiver) = watch::channel(Mode::Unavailable);
    Ok(SystemdController::new(connection, mode_sender))
}

fn print_mode(mode: Mode) {
    println!("Mode: {}", mode.as_str());
}

fn print_reports(reports: &[UnitReport]) {
    for report in reports {
        println!(
            "{:<42} enabled={:<12} active={}",
            report.name, report.unit_file_state, report.active_state
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn parses_commands() {
        assert!(matches!(Command::parse_from([]), Ok(Command::Daemon)));
        assert!(matches!(
            Command::parse_from(["up".to_string()]),
            Ok(Command::Up)
        ));
        assert!(matches!(
            Command::parse_from(["status".to_string()]),
            Ok(Command::Status)
        ));
        assert!(matches!(
            Command::parse_from(["--help".to_string()]),
            Ok(Command::Help)
        ));
        assert!(Command::parse_from(["unknown".to_string()]).is_err());
        assert!(Command::parse_from(["up".to_string(), "extra".to_string()]).is_err());
    }
}
