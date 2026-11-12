pub mod dlpspec;
pub mod nano;
pub mod spectrum;

use dlpspec::{CalibCoeffs, ScanResults, SlewScanConfig};
use hidapi::HidApi;
use nano::{cmd, fmt_dlpc_ver, fmt_ver, LampMode, Nano, NanoError};
use serde::{Deserialize, Serialize};
use spectrum::{ReferenceMode, ScanView};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
struct Cache {
    serial: String,
    ref_cal: Option<Vec<u8>>,
    ref_matrix: Option<Vec<u8>>,
    calib: Option<CalibCoeffs>,
    last_blob: Option<Vec<u8>>,
    last_results: Option<ScanResults>,
    last_view: Option<ScanView>,
    local_ref: Option<ScanResults>,
}

pub struct AppState {
    api: Mutex<Option<HidApi>>,
    dev: Mutex<Option<Nano>>,
    cache: Mutex<Cache>,
    data_dir: PathBuf,
}

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

impl AppState {
    fn with_dev<T>(&self, f: impl FnOnce(&mut Nano) -> Result<T, NanoError>) -> CmdResult<T> {
        let mut g = self.dev.lock().unwrap();
        let dev = g.as_mut().ok_or_else(|| NanoError::NotConnected.to_string())?;
        match f(dev) {
            Ok(v) => Ok(v),
            Err(NanoError::Hid(e)) => {
                // Device probably unplugged: drop the handle so the UI can reconnect.
                *g = None;
                Err(format!("USB error: {} (disconnected)", e))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn ref_path(&self, kind: &str, serial: &str) -> PathBuf {
        self.data_dir.join(format!("{}_{}.dat", kind, serial))
    }

    /// Make sure the factory reference (ref-cal scan + matrix) for the connected
    /// device is cached in memory and on disk.
    fn ensure_reference(&self) -> CmdResult<()> {
        {
            let c = self.cache.lock().unwrap();
            if c.ref_cal.is_some() && c.ref_matrix.is_some() {
                return Ok(());
            }
        }
        let serial = self.cache.lock().unwrap().serial.clone();
        let (cal_p, mat_p) = (self.ref_path("refcal", &serial), self.ref_path("refmatrix", &serial));
        let (cal, mat) = match self.with_dev(|d| Ok((d.read_file(nano::file_type::REF_CAL_DATA)?, d.read_file(nano::file_type::REF_CAL_MATRIX)?))) {
            Ok(v) => {
                let _ = std::fs::write(&cal_p, &v.0);
                let _ = std::fs::write(&mat_p, &v.1);
                v
            }
            Err(e) => match (std::fs::read(&cal_p), std::fs::read(&mat_p)) {
                (Ok(a), Ok(b)) => (a, b),
                _ => return Err(format!("could not read the factory reference: {}", e)),
            },
        };
        let mut c = self.cache.lock().unwrap();
        c.ref_cal = Some(cal);
        c.ref_matrix = Some(mat);
        Ok(())
    }

    /// Load cached reference blobs for a serial number (offline viewing of saved scans).
    fn load_reference_for(&self, serial: &str) {
        let mut c = self.cache.lock().unwrap();
        if c.serial == serial && c.ref_cal.is_some() {
            return;
        }
        if let (Ok(a), Ok(b)) = (std::fs::read(self.ref_path("refcal", serial)), std::fs::read(self.ref_path("refmatrix", serial))) {
            c.ref_cal = Some(a);
            c.ref_matrix = Some(b);
            c.serial = serial.to_string();
        }
    }

    fn build_view(&self, results: ScanResults, blob: Vec<u8>, mode: ReferenceMode, extra: serde_json::Value) -> CmdResult<ScanView> {
        if mode == ReferenceMode::Builtin {
            if self.dev.lock().unwrap().is_some() {
                let _ = self.ensure_reference();
            } else {
                self.load_reference_for(&results.head.serial_number);
            }
        }
        let mut c = self.cache.lock().unwrap();
        let view = spectrum::make_view(&results, mode, c.ref_cal.as_deref(), c.ref_matrix.as_deref(), c.local_ref.as_ref(), extra);
        c.last_blob = Some(blob);
        c.last_results = Some(results);
        c.last_view = Some(view.clone());
        Ok(view)
    }
}

// ------------------------------------------------------------------ connection

#[derive(Serialize)]
struct DeviceInfo {
    serial: String,
    model: String,
    tiva_version: String,
    dlpc_sw_version: String,
    dlpc_flash_version: String,
    spec_lib_version: String,
    cal_data_version: String,
    ref_cal_version: String,
    cfg_data_version: String,
    path: String,
}

#[tauri::command]
async fn connect(state: State<'_, AppState>) -> CmdResult<DeviceInfo> {
    {
        let mut api = state.api.lock().unwrap();
        if api.is_none() {
            *api = Some(HidApi::new().map_err(err)?);
        }
        let api = api.as_mut().unwrap();
        api.refresh_devices().map_err(err)?;
        let dev = Nano::open(api).map_err(err)?;
        *state.dev.lock().unwrap() = Some(dev);
    }
    let info = state.with_dev(|d| {
        let v = d.versions()?;
        Ok(DeviceInfo {
            serial: d.serial_number().unwrap_or_default(),
            model: d.model_name().unwrap_or_default(),
            tiva_version: fmt_ver(v[0]),
            dlpc_sw_version: fmt_dlpc_ver(v[1]),
            dlpc_flash_version: fmt_dlpc_ver(v[2]),
            spec_lib_version: fmt_ver(v[3]),
            cal_data_version: v[4].to_string(),
            ref_cal_version: v[5].to_string(),
            cfg_data_version: v[6].to_string(),
            path: d.path.clone(),
        })
    })?;
    let calib = state.with_dev(|d| d.calib_coeffs_blob()).ok().and_then(|b| CalibCoeffs::from_blob(&b).ok());
    {
        let mut c = state.cache.lock().unwrap();
        if c.serial != info.serial {
            c.ref_cal = None;
            c.ref_matrix = None;
        }
        c.serial = info.serial.clone();
        c.calib = calib;
    }
    // Fetch the factory reference in the background of the connect (cached to disk).
    let _ = state.ensure_reference();
    Ok(info)
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> CmdResult<()> {
    *state.dev.lock().unwrap() = None;
    Ok(())
}

#[derive(Serialize)]
struct StatusInfo {
    present: bool,
    connected: bool,
    device_status: u32,
    error_status: u32,
    error_codes: Vec<u8>,
    battery_volts: Option<f64>,
    /// true while a long operation (scan, download) holds the device
    busy: bool,
}

#[tauri::command]
async fn poll_status(state: State<'_, AppState>) -> CmdResult<StatusInfo> {
    let present = {
        let mut api = state.api.lock().unwrap();
        if api.is_none() {
            *api = HidApi::new().ok();
        }
        match api.as_mut() {
            Some(a) => {
                let _ = a.refresh_devices();
                Nano::is_present(a)
            }
            None => false,
        }
    };
    if !present {
        *state.dev.lock().unwrap() = None;
    }
    let connected = state.dev.lock().unwrap().is_some();
    let idle = StatusInfo { present, connected, device_status: 0, error_status: 0, error_codes: vec![], battery_volts: None, busy: false };
    if !connected {
        return Ok(idle);
    }
    // Do not wait behind a running scan: report busy instead.
    let Ok(mut g) = state.dev.try_lock() else {
        return Ok(StatusInfo { busy: true, ..idle });
    };
    let Some(d) = g.as_mut() else { return Ok(StatusInfo { connected: false, ..idle }) };
    let r = (|| -> Result<_, NanoError> { Ok((d.device_status()?, d.error_status()?, d.read_battery().ok())) })();
    match r {
        Ok((ds, (es, codes), batt)) => Ok(StatusInfo {
            device_status: ds,
            error_status: es,
            error_codes: codes,
            battery_volts: batt.map(|b| b as f64 / 100.0),
            ..idle
        }),
        Err(NanoError::Busy) => Ok(StatusInfo { busy: true, ..idle }),
        Err(NanoError::Hid(e)) => {
            *g = None;
            Err(format!("USB error: {} (disconnected)", e))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn reset_errors(state: State<'_, AppState>) -> CmdResult<()> {
    state.with_dev(|d| d.reset_error_status())
}

// ------------------------------------------------------------------ sensors / utilities

#[derive(Serialize)]
struct Sensors {
    // Each reading is independent: a failing sensor gives None plus a message.
    ambient_c: Option<f64>,
    detector_c: Option<f64>,
    humidity_pct: Option<f64>,
    hdc_temp_c: Option<f64>,
    battery_v: Option<f64>,
    tiva_temp_c: Option<f64>,
    photodetector: Option<(u32, u32, u32)>,
    device_time: Option<String>,
    pga: Option<u8>,
    hibernate: Option<bool>,
    sd_scans: Option<u32>,
    errors: Vec<String>,
}

#[tauri::command]
async fn read_sensors(state: State<'_, AppState>) -> CmdResult<Sensors> {
    state.with_dev(|d| {
        let mut errors = Vec::new();
        fn opt<T>(errors: &mut Vec<String>, name: &str, r: Result<T, NanoError>) -> Option<T> {
            r.map_err(|e| errors.push(format!("{}: {}", name, e))).ok()
        }
        // TMP006 (ambient + detector temperature)
        let before = d.error_status().map(|e| e.0).unwrap_or(0);
        let temp = match d.read_temp() {
            Ok(t) => {
                let after = d.error_status().map(|e| e.0).unwrap_or(0);
                if after & 0x80 != 0 && before & 0x80 == 0 || (t.0.abs() <= 1 && t.1.abs() <= 1) && after & 0x80 != 0 {
                    errors.push("TMP006 temperature sensor did not respond (device error flag set)".into());
                    None
                } else {
                    Some(t)
                }
            }
            Err(e) => {
                errors.push(format!("TMP006: {}", e));
                None
            }
        };
        let hum = opt(&mut errors, "HDC1000", d.read_humidity());
        let batt = opt(&mut errors, "Battery", d.read_battery());
        let tiva = opt(&mut errors, "Tiva", d.read_tiva_temp());
        let pd = opt(&mut errors, "Photodetector", d.read_photodetector());
        let dt = d.get_date_time().ok();
        Ok(Sensors {
            ambient_c: temp.map(|t| t.0 as f64 / 100.0),
            detector_c: temp.map(|t| t.1 as f64 / 100.0),
            humidity_pct: hum.map(|h| h.0 as f64 / 100.0),
            hdc_temp_c: hum.map(|h| h.1 as f64 / 100.0),
            battery_v: batt.map(|b| b as f64 / 100.0),
            tiva_temp_c: tiva.map(|t| t as f64 / 100.0),
            photodetector: pd,
            device_time: dt.map(|dt| format!("20{:02}-{:02}-{:02} {:02}:{:02}:{:02}", dt[0], dt[1], dt[2], dt[4], dt[5], dt[6])),
            pga: d.get_pga().ok(),
            hibernate: d.get_hibernate().ok(),
            sd_scans: d.num_sd_scans().ok(),
            errors,
        })
    })
}

#[tauri::command]
async fn sync_clock(state: State<'_, AppState>) -> CmdResult<String> {
    use chrono::{Datelike, Local, Timelike};
    let now = Local::now();
    let d = [
        (now.year() - 2000) as u8,
        now.month() as u8,
        now.day() as u8,
        now.weekday().number_from_monday() as u8, // Qt dayOfWeek(): Monday = 1
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    ];
    state.with_dev(|dev| dev.set_date_time(d))?;
    Ok(now.format("%Y-%m-%d %H:%M:%S").to_string())
}

#[tauri::command]
async fn set_hibernate(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.with_dev(|d| d.set_hibernate(enabled))
}

#[tauri::command]
async fn reset_device(state: State<'_, AppState>) -> CmdResult<()> {
    let r = state.with_dev(|d| d.reset());
    *state.dev.lock().unwrap() = None;
    r
}

#[tauri::command]
async fn set_lamp(state: State<'_, AppState>, mode: LampMode) -> CmdResult<()> {
    state.with_dev(|d| d.set_lamp(mode))
}

#[tauri::command]
async fn run_test(state: State<'_, AppState>, name: String) -> CmdResult<String> {
    let pass = |b: u8| if b == 0 { "PASS".to_string() } else { format!("FAIL (code {})", b) };
    state.with_dev(|d| match name.as_str() {
        "eeprom" => d.self_test(cmd::EEPROM_TEST).map(pass),
        "adc" => d.self_test(cmd::ADC_TEST).map(pass),
        "tmp" => d.self_test(cmd::TMP_TEST).map(pass),
        "hdc" => d.self_test(cmd::HDC_TEST).map(pass),
        "sdcard" => d.self_test(cmd::SDC_TEST).map(pass),
        "sdram" => d.self_test(cmd::SDRAM_TEST).map(pass),
        "bq" => d.bq_test().map(|v| format!("status={} batt={:.2} V usb_det={} charge_current={} pts_fault={}", v[0], v[1] as f64 / 100.0, v[2], v[3], v[4])),
        "led_on" => d.led_test(true).map(|_| "LED test on".to_string()),
        "led_off" => d.led_test(false).map(|_| "LED test off".to_string()),
        "button_start" => d.button_test_write(true).map(|_| "Button test armed: press the scan button, then Read".to_string()),
        "button_read" => d.button_test_read().map(|b| format!("button register = 0x{:02X}", b)),
        "button_stop" => d.button_test_write(false).map(|_| "Button test stopped".to_string()),
        other => Err(NanoError::Protocol(format!("unknown test {}", other))),
    })
}

// ------------------------------------------------------------------ configurations

#[derive(Serialize)]
struct ConfigList {
    configs: Vec<SlewScanConfig>,
    active: u8,
    calib: Option<CalibCoeffs>,
}

#[tauri::command]
async fn list_configs(state: State<'_, AppState>) -> CmdResult<ConfigList> {
    let (configs, active) = state.with_dev(|d| {
        let n = d.num_configs()?;
        let mut out = Vec::new();
        for i in 0..n {
            let blob = d.read_config_blob(i)?;
            out.push(SlewScanConfig::from_blob(&blob).map_err(|e| NanoError::Protocol(e.to_string()))?);
        }
        Ok((out, d.active_config()?))
    })?;
    Ok(ConfigList { configs, active, calib: state.cache.lock().unwrap().calib })
}

#[tauri::command]
async fn set_active_config(state: State<'_, AppState>, index: u8) -> CmdResult<()> {
    state.with_dev(|d| d.set_active_config(index))
}

/// Replace the full list of configurations stored on the device (like the TI GUI does).
#[tauri::command]
async fn save_configs(state: State<'_, AppState>, configs: Vec<SlewScanConfig>, active: u8) -> CmdResult<()> {
    let calib = state.cache.lock().unwrap().calib;
    for c in &configs {
        spectrum::validate_config(c, calib.as_ref())?;
    }
    let serial = state.cache.lock().unwrap().serial.clone();
    state.with_dev(|d| {
        d.erase_all_configs()?;
        for (i, c) in configs.iter().enumerate() {
            let mut c = c.clone();
            c.serial_number = serial.clone();
            d.save_config_blob(i as u8, &c.to_blob())?;
        }
        if !configs.is_empty() {
            d.set_active_config(active.min(configs.len() as u8 - 1))?;
        }
        Ok(())
    })
}

#[tauri::command]
async fn validate_config(state: State<'_, AppState>, config: SlewScanConfig) -> CmdResult<spectrum::ConfigCheck> {
    let calib = state.cache.lock().unwrap().calib;
    Ok(spectrum::check_config(&config, calib.as_ref()))
}

// ------------------------------------------------------------------ scanning

#[derive(Deserialize)]
struct ScanOptions {
    repeats: u16,
    store_in_sd: bool,
    name_tag: String,
    /// None = automatic PGA
    pga: Option<u8>,
    lamp: LampMode,
    reference: ReferenceMode,
    /// Optional: apply this config (not stored) before scanning
    config: Option<SlewScanConfig>,
}

#[derive(Serialize, Clone)]
struct Progress {
    stage: String,
    elapsed_ms: u64,
    estimated_ms: u32,
}

#[tauri::command]
async fn estimate_scan_time(state: State<'_, AppState>, repeats: u16) -> CmdResult<u32> {
    state.with_dev(|d| {
        d.set_num_repeats(repeats)?;
        d.estimated_scan_time_ms()
    })
}

#[tauri::command]
async fn scan(app: AppHandle, state: State<'_, AppState>, opts: ScanOptions) -> CmdResult<ScanView> {
    let t0 = std::time::Instant::now();
    let est = state.with_dev(|d| {
        if let Some(cfg) = &opts.config {
            let n = d.apply_config_blob(&cfg.to_blob())?;
            if n < 0 {
                return Err(NanoError::Protocol("device rejected the scan configuration".into()));
            }
        }
        match opts.pga {
            Some(g) => d.set_fixed_pga(true, g)?,
            None => d.set_fixed_pga(false, 64)?,
        }
        d.set_lamp(opts.lamp)?;
        d.set_num_repeats(opts.repeats.max(1))?;
        if !opts.name_tag.is_empty() {
            d.set_scan_name_tag(&opts.name_tag)?;
        }
        d.estimated_scan_time_ms()
    })?;
    let _ = app.emit("scan-progress", Progress { stage: "scanning".into(), elapsed_ms: 0, estimated_ms: est });
    let app2 = app.clone();
    let blob = state.with_dev(|d| {
        d.scan(opts.store_in_sd, |_st| {
            let _ = app2.emit(
                "scan-progress",
                Progress { stage: "scanning".into(), elapsed_ms: t0.elapsed().as_millis() as u64, estimated_ms: est },
            );
        })
    })?;
    let scan_ms = t0.elapsed().as_millis() as u64;
    let _ = app.emit("scan-progress", Progress { stage: "processing".into(), elapsed_ms: scan_ms, estimated_ms: est });
    let results = match dlpspec::interpret::interpret(&blob) {
        Ok(r) => r,
        Err(e) => {
            // Fall back to on-device interpretation (intensity only).
            let (wl, it) = state.with_dev(|d| d.device_interpret()).map_err(|e2| format!("interpretation failed: {} / device: {}", e, e2))?;
            spectrum::results_from_device(&wl, &it)
        }
    };
    state.build_view(results, blob, opts.reference, serde_json::json!({ "scan_time_ms": scan_ms, "estimated_ms": est }))
}

#[tauri::command]
async fn recompute(state: State<'_, AppState>, reference: ReferenceMode) -> CmdResult<ScanView> {
    let (r, b) = {
        let c = state.cache.lock().unwrap();
        (c.last_results.clone().ok_or("no scan yet")?, c.last_blob.clone().unwrap_or_default())
    };
    let extra = state.cache.lock().unwrap().last_view.as_ref().map(|v| v.extra.clone()).unwrap_or_default();
    state.build_view(r, b, reference, extra)
}

#[tauri::command]
async fn set_local_reference(state: State<'_, AppState>) -> CmdResult<String> {
    let (results, blob) = {
        let c = state.cache.lock().unwrap();
        (c.last_results.clone().ok_or("no scan yet")?, c.last_blob.clone())
    };
    let desc = format!("{} ({} points, {})", results.head.scan_name, results.length, results.cfg.config_name);
    if let Some(b) = blob {
        let _ = std::fs::write(state.data_dir.join("local_reference.dat"), b);
    }
    state.cache.lock().unwrap().local_ref = Some(results);
    Ok(desc)
}

#[tauri::command]
async fn read_sd_last_scan(state: State<'_, AppState>, reference: ReferenceMode) -> CmdResult<ScanView> {
    let blob = state.with_dev(|d| d.read_last_sd_scan())?;
    let r = dlpspec::interpret::interpret(&blob).map_err(err)?;
    state.build_view(r, blob, reference, serde_json::json!({ "source": "sd" }))
}

#[tauri::command]
async fn delete_sd_last_scan(state: State<'_, AppState>) -> CmdResult<()> {
    state.with_dev(|d| d.delete_last_sd_scan())
}

// ------------------------------------------------------------------ files

#[tauri::command]
async fn save_scan(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let (view, blob) = {
        let c = state.cache.lock().unwrap();
        (c.last_view.clone().ok_or("no scan to save")?, c.last_blob.clone().unwrap_or_default())
    };
    let default = format!("{}.csv", view.file_stem);
    let Some(path) = app.dialog().file().set_file_name(&default).add_filter("CSV", &["csv"]).blocking_save_file() else {
        return Ok(None);
    };
    let path = path.into_path().map_err(err)?;
    std::fs::write(&path, spectrum::to_csv(&view)).map_err(err)?;
    if !blob.is_empty() {
        std::fs::write(path.with_extension("dat"), &blob).map_err(err)?;
    }
    Ok(Some(path.to_string_lossy().to_string()))
}

#[tauri::command]
async fn open_scan(app: AppHandle, state: State<'_, AppState>, reference: ReferenceMode) -> CmdResult<Option<ScanView>> {
    let Some(path) = app.dialog().file().add_filter("NIRscan raw scan", &["dat"]).blocking_pick_file() else {
        return Ok(None);
    };
    let path = path.into_path().map_err(err)?;
    let blob = std::fs::read(&path).map_err(err)?;
    let r = dlpspec::interpret::interpret(&blob).map_err(err)?;
    let v = state.build_view(r, blob, reference, serde_json::json!({ "source": path.to_string_lossy() }))?;
    Ok(Some(v))
}

#[tauri::command]
async fn pick_reference_file(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let Some(path) = app.dialog().file().add_filter("NIRscan raw scan", &["dat"]).blocking_pick_file() else {
        return Ok(None);
    };
    let path = path.into_path().map_err(err)?;
    let blob = std::fs::read(&path).map_err(err)?;
    let r = dlpspec::interpret::interpret(&blob).map_err(err)?;
    let _ = std::fs::write(state.data_dir.join("local_reference.dat"), &blob);
    let desc = format!("{} ({} points)", path.file_name().unwrap_or_default().to_string_lossy(), r.length);
    state.cache.lock().unwrap().local_ref = Some(r);
    Ok(Some(desc))
}

#[tauri::command]
async fn local_reference_info(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.cache.lock().unwrap().local_ref.as_ref().map(|r| format!("{} ({} points, {})", r.head.scan_name, r.length, r.cfg.config_name)))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("nirscan-nano"));
            let _ = std::fs::create_dir_all(&dir);
            let local_ref = std::fs::read(dir.join("local_reference.dat")).ok().and_then(|b| dlpspec::interpret::interpret(&b).ok());
            app.manage(AppState {
                api: Mutex::new(None),
                dev: Mutex::new(None),
                cache: Mutex::new(Cache { local_ref, ..Default::default() }),
                data_dir: dir,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            disconnect,
            poll_status,
            reset_errors,
            read_sensors,
            sync_clock,
            set_hibernate,
            reset_device,
            set_lamp,
            run_test,
            list_configs,
            set_active_config,
            save_configs,
            validate_config,
            estimate_scan_time,
            scan,
            recompute,
            set_local_reference,
            read_sd_last_scan,
            delete_sd_last_scan,
            save_scan,
            open_scan,
            pick_reference_file,
            local_reference_info
        ])
        .run(tauri::generate_context!())
        .expect("error while running NIRscan Nano");
}
