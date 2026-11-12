# NIRscan Nano for macOS

A native macOS replacement for TI's Windows-only *DLP NIRscan Nano GUI*,
built with [Tauri 2](https://tauri.app) (Rust backend, TypeScript UI).
It talks to the spectrometer directly over USB HID (VID:PID `0451:4200`),
no drivers needed.

## Features

* Auto-detects and connects to the NIRscan Nano; shows firmware versions,
  serial number, battery, scan/SD/Bluetooth state and device errors.
* **Scan**: pick the stored configuration, scans to average, PGA (auto or
  fixed), lamp mode, optional SD-card copy and name tag; live progress.
* Plots **absorbance, reflectance, sample intensity and reference** with
  zoom and overlay of up to 8 scans.
* Reference: built-in factory reference (read from the device and cached per
  serial number for offline use), or a local reference scan you take yourself.
* Save as CSV (same columns as the TI GUI) plus the raw `.dat` blob; reopen
  `.dat` files later, without the device.
* **Scan configurations**: list, create, duplicate, edit (Column/Hadamard,
  up to 5 sections, width, points, exposure), validate against the device
  calibration, set active, write the list to the device.
* **Device**: sensors (temperatures, humidity, battery, lamp photodiode),
  clock sync, hibernate, lamp on/auto, SD card last scan read/delete, reset.
* **Tests**: EEPROM, ADC, TMP006, HDC1000, SDRAM, SD card, charger, LED, button.

Factory calibration, serial/model writes and firmware updates are deliberately
left out.

## Build

Requirements: Rust (stable), Node 18+, Xcode command line tools.

```bash
npm install
npm run tauri dev      # run in development
npm run tauri build    # produces src-tauri/target/release/bundle/macos/NIRscan Nano.app (+ .dmg)
```

Command line tool for protocol testing (no GUI):

```bash
cd src-tauri
cargo run --bin nano-cli -- info
cargo run --bin nano-cli -- sensors
cargo run --bin nano-cli -- configs
cargo run --bin nano-cli -- scan scan.dat
cargo run --bin nano-cli -- interpret scan.dat refcal.dat refmatrix.dat
```

Unit tests (`cargo test --lib`) check TPL round trips, the Hadamard
S-matrices and a synthetic two-section (column + Hadamard) scan.

## Layout

```
src/                      UI (vanilla TypeScript + uPlot)
src-tauri/src/nano.rs     USB HID transport and command set
src-tauri/src/dlpspec/    Rust port of the TI DLP Spectrum Library (TPL, interpret, reference)
src-tauri/src/spectrum.rs absorbance/reflectance, CSV, config validation
src-tauri/src/lib.rs      Tauri commands
docs/PROTOCOL.md          protocol documentation
```

The original TI sources in `../gui` and `../DLPNIRscanNanoSoftware_2.1.0` are
not modified.
# NirGui
