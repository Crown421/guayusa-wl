# guayusa-ws - Wayland Idle Inhibitor D-Bus Service

A D-Bus service that prevents the compositor/desktop environment from going idle or engaging screen savers on Wayland compositors.

## Features

- D-Bus interface for programmatic control
- Wayland idle inhibit protocol support
- Starts with idle inhibition disabled by default
- Graceful shutdown on SIGINT/SIGTERM

## Building

```bash
cargo build --release
```

## Usage

### Starting the Service

```bash
cargo run --release
```

The service will start and expose a D-Bus interface at:
- **Service Name**: `org.guayusa.IdleInhibitor`
- **Object Path**: `/org/guayusa/IdleInhibitor`
- **Interface**: `org.guayusa.IdleInhibitor`

### D-Bus Interface

The service exposes the following methods and properties:

#### Methods

- `Enable()` - Enables idle inhibition
- `Disable()` - Disables idle inhibition

#### Properties

- `Status` (boolean, read-only) - Current idle inhibition status

### Control via busctl

#### Enable idle inhibition:
```bash
busctl --user call org.guayusa.IdleInhibitor /org/guayusa/IdleInhibitor org.guayusa.IdleInhibitor Enable
```

#### Disable idle inhibition:
```bash
busctl --user call org.guayusa.IdleInhibitor /org/guayusa/IdleInhibitor org.guayusa.IdleInhibitor Disable
```

#### Check current status:
```bash
busctl --user get-property org.guayusa.IdleInhibitor /org/guayusa/IdleInhibitor org.guayusa.IdleInhibitor Status
```

#### Monitor D-Bus signals (for debugging):
```bash
busctl --user monitor org.guayusa.IdleInhibitor
```

### Control via gdbus (alternative)

#### Enable idle inhibition:
```bash
gdbus call --session --dest org.guayusa.IdleInhibitor --object-path /org/guayusa/IdleInhibitor --method org.guayusa.IdleInhibitor.Enable
```

#### Disable idle inhibition:
```bash
gdbus call --session --dest org.guayusa.IdleInhibitor --object-path /org/guayusa/IdleInhibitor --method org.guayusa.IdleInhibitor.Disable
```

#### Check current status:
```bash
gdbus introspect --session --dest org.guayusa.IdleInhibitor --object-path /org/guayusa/IdleInhibitor
gdbus call --session --dest org.guayusa.IdleInhibitor --object-path /org/guayusa/IdleInhibitor --method org.freedesktop.DBus.Properties.Get org.guayusa.IdleInhibitor Status
```

## Systemd Service (Optional)

Create a user systemd service file at `~/.config/systemd/user/guayusad.service`:

```ini
[Unit]
Description=Wayland Idle Inhibitor D-Bus Service
After=graphical-session.target

[Service]
Type=simple
ExecStart=/path/to/guayusad
Restart=on-failure
RestartSec=5
Environment=WAYLAND_DISPLAY=wayland-0

[Install]
WantedBy=default.target
```

Then enable and start the service:

```bash
systemctl --user daemon-reload
systemctl --user enable guayusad.service
systemctl --user start guayusad.service
```

## Requirements

- Wayland compositor that supports the `zwp_idle_inhibit_manager_v1` protocol
- D-Bus session bus

## Dependencies

- `wayland-client` - Wayland protocol bindings
- `wayland-protocols` - Wayland protocol definitions
- `zbus` - D-Bus implementation
- `tokio` - Async runtime
- `signal-hook-tokio` - Signal handling
- `futures` - Async utilities

## License

This project is licensed under the MIT License.
