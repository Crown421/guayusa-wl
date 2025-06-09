# guayusa-wl

A D-Bus service that prevents the compositor/desktop environment from going idle or engaging screen savers on Wayland compositors.

Inspired by [matcha](https://codeberg.org/QuincePie/matcha) for the ilde inhibition, and [wl-gammarelay-rs](https://github.com/MaxVerevkin/wl-gammarelay-rs) for the D-Bus approach.
Partially an attempt to play around with GitHub Copilot before the montly limits kicked in.

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
- **Object Path**: `/`
- **Interface**: `org.guayusa.Idle`

### D-Bus Interface

The service exposes the following methods and properties:

#### Methods

- `Enable()` - Enables idle inhibition
- `Disable()` - Disables idle inhibition  
- `Toggle()` - Toggles idle inhibition state (returns new state)
- `SetInhibit(bool)` - Sets idle inhibition state (true = enable, false = disable)

#### Properties

- `Status` (boolean, read-only) - Current idle inhibition status

### Control via busctl

#### Enable idle inhibition:
```bash
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle Enable
```

#### Disable idle inhibition:
```bash
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle Disable
```

#### Toggle idle inhibition:
```bash
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle Toggle
```

#### Set inhibition state (enable):
```bash
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle SetInhibit b true
```

#### Set inhibition state (disable):
```bash
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle SetInhibit b false
```

#### Check current status:
```bash
busctl --user get-property org.guayusa.IdleInhibitor / org.guayusa.Idle Status
```

#### Monitor status
```bash
dbus-monitor --session "type='signal',interface='org.guayusa.Idle',member='StatusChanged'"
```

#### Monitor D-Bus signals (for debugging):
```bash
busctl --user monitor org.guayusa.IdleInhibitor
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
