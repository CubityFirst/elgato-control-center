# Elgato Control Center (mini)

A small Windows tray app for Elgato Key Lights. Left-click the tray icon to open a
popover where you can switch each light on or off and adjust brightness and colour
temperature. Built with Rust + Tauri 2, so it ships as a single small `.exe` that uses
the system WebView2 runtime.

Not affiliated with or endorsed by Elgato or Corsair; "Elgato" and "Key Light" are their
trademarks and are used here only to say which lights this works with.

## Features

- Finds lights automatically via mDNS (`_elg._tcp`) on every network interface.
- Add a light by IP if discovery is blocked (`192.168.1.50` or `192.168.1.50:9123`).
- Per-light power switch, brightness (3–100 %) and temperature (2900–7000 K).
- Tray icon reflects state: filled disc when any light is on, ring when all are off,
  dimmed when no light is reachable. It follows the taskbar's light/dark theme.
- Tray menu: Open, All lights on/off, Rescan network, Start with Windows, Quit.
  Middle-click toggles all.
- Remembers known lights between launches (`%APPDATA%\local.elgato-control-center\lights.json`),
  keyed by serial number, so a light that DHCP moves to a new IP is followed rather than duplicated.
- Keyboard friendly: Tab through every control, arrow keys on sliders, Esc to close.

## Build

Requirements: Rust (stable) and the MSVC build tools. WebView2 is already part of Windows 11.

```
cd src-tauri
cargo build --release
```

The portable executable is `src-tauri\target\release\elgato-control-center.exe`.
Copy it anywhere and run it; it lives only in the tray. It is a windowed program,
so no console appears in either debug or release builds.

## Installer

The Tauri CLI (prebuilt, via npm) bundles a per-user NSIS setup that needs no admin rights:

```
npm install
npm run build
```

The installer lands in `src-tauri\target\release\bundle\nsis\` as
`Elgato Control Center_1.0.0_x64-setup.exe`. It adds a Start Menu entry and an
uninstaller under Settings > Apps. NSIS itself is downloaded once by the CLI on first use.

## Tests

```
cd src-tauri
cargo test
```

Two opt-in tests touch real things and are skipped by default: one talks to a light,
the other toggles the "Start with Windows" registry entry and restores it.

```
set ELGATO_TEST_LIGHT=192.168.1.50
cargo test -- --ignored
```

## Layout

```
src-tauri/src/main.rs       tray icon, popover positioning, commands, persistence
src-tauri/src/store.rs      known-light map: keying by serial, merging network results
src-tauri/src/lights.rs     mDNS discovery and the Key Light HTTP API
src-tauri/src/autostart.rs  "Start with Windows" (HKCU Run key)
src-tauri/windows/hooks.nsh uninstaller removes the startup entry
ui/                         popover (plain HTML/CSS/JS, no build step)
```

## Notes

- Windows Firewall may ask to allow the app on first run; mDNS needs UDP 5353.
- The Key Light API is local HTTP on port 9123 (`/elgato/lights`, `/elgato/accessory-info`).
  Temperature is sent in mireds (143–344); the UI converts to Kelvin.
- "Start with Windows" writes `HKCU\...\CurrentVersion\Run`, so it needs no admin rights and
  respects being switched off in Settings > Apps > Startup. If the portable `.exe` is moved,
  the entry is updated the next time it runs.

## License

[MIT](LICENSE)
