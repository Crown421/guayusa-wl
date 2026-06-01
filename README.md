# guayusa-wl

A D-Bus service that controls split `swayidle` user services. It is mainly for personal use, and replace a previous version that created a bare wl surface.
Unfortunately, a recent update to sway stopped this from working.

Guayusa exposes a small D-Bus interface for switching between normal idle behavior, disabling only idle suspend, or disabling both lock/display-off and idle suspend.

## Modes

- `normal`: lock/display-off and suspend idle policies are enabled
- `no-suspend`: lock/display-off stays enabled, idle suspend is disabled
- `no-idle`: lock/display-off and idle suspend are disabled

The before-sleep lock policy is intentionally separate. Guayusa does not stop it, so manual suspend, lid-close suspend, and other non-idle sleep paths can still lock before sleep.

## Building

```bash
cargo build --release
```

## Setup

Guayusa manages these user units by default:

- `guayusa-swayidle-lock.service`
- `guayusa-swayidle-suspend.service`

It also ships this independent unit:

- `guayusa-swayidle-before-sleep.service`

Enable the units you want:

```bash
systemctl --user enable --now guayusa.service
systemctl --user enable --now guayusa-swayidle-lock.service
systemctl --user enable --now guayusa-swayidle-suspend.service
systemctl --user enable --now guayusa-swayidle-before-sleep.service
```

Remove any direct `swayidle` launch from your Sway config. If Sway still starts its own `swayidle` process, Guayusa can stop the managed units while the unmanaged process continues enforcing idle actions.

The default managed unit names can be overridden for custom policy units:

```ini
# systemctl --user edit guayusa.service
[Service]
Environment=GUAYUSA_LOCK_UNIT=my-swayidle-lock.service
Environment=GUAYUSA_SUSPEND_UNIT=my-swayidle-suspend.service
```

## D-Bus Interface

The service is exposed at:

- Service name: `org.guayusa.IdleInhibitor`
- Object path: `/`
- Interface: `org.guayusa.Idle`

### Methods

- `SetMode(string)` - Sets `normal`, `no-suspend`, or `no-idle`
- `CycleMode()` - Cycles `normal -> no-suspend -> no-idle -> normal`
- `Enable()` - Compatibility method; sets `no-idle`
- `Disable()` - Compatibility method; sets `normal`
- `Toggle()` - Compatibility method; toggles between `normal` and `no-idle`
- `SetInhibit(bool)` - Compatibility method; `true` sets `no-idle`, `false` sets `normal`

### Properties

- `Mode` - Current mode: `normal`, `no-suspend`, `no-idle`, `custom`, or `unavailable`
- `Status` - Compatibility boolean; true for `no-suspend`, `no-idle`, and `custom`

### Signals

- `ModeChanged(string)`
- `StatusChanged(bool)`

## Control via busctl

```bash
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle SetMode s no-suspend
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle SetMode s no-idle
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle SetMode s normal
busctl --user call org.guayusa.IdleInhibitor / org.guayusa.Idle CycleMode
busctl --user get-property org.guayusa.IdleInhibitor / org.guayusa.Idle Mode
busctl --user get-property org.guayusa.IdleInhibitor / org.guayusa.Idle Status
```

Monitor changes:

```bash
dbus-monitor --session "type='signal',interface='org.guayusa.Idle'"
```

## Requirements

- `swayidle`
- systemd user manager
- D-Bus session bus

## License

This project is licensed under the MIT License.
