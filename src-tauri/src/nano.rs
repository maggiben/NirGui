//! USB HID protocol for the TI DLP NIRscan Nano (Tiva TM4C129 firmware v2.x).
//!
//! Every command is an `nnoMessage`:
//! ```text
//!  byte 0   flags: bit7 rw (1=read) | bit6 reply | bits5-4 resp | bits3-2 reserved | bits1-0 dest
//!  byte 1   sequence number
//!  byte 2-3 length (LE) = 2 (command) + parameter bytes
//!  byte 4-5 command (LE) = (CMD2 << 8) | CMD3
//!  byte 6.. parameters
//! ```
//! Messages are sent as 64-byte HID output reports (report ID 0). The first
//! report carries the 4-byte header + first 60 payload bytes, further reports
//! carry 64 raw payload bytes. Responses come back the same way: the header
//! `length` tells how many payload bytes follow; `resp` = 0 OK, 1 error, 2 busy.

use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

pub const VID: u16 = 0x0451;
pub const PID: u16 = 0x4200;
const PKT: usize = 64;
const DEFAULT_TIMEOUT_MS: i32 = 20_000;

#[derive(Debug, thiserror::Error)]
pub enum NanoError {
    #[error("NIRscan Nano not found (USB 0451:4200)")]
    NotFound,
    #[error("not connected")]
    NotConnected,
    #[error("USB HID error: {0}")]
    Hid(#[from] hidapi::HidError),
    #[error("device returned NACK for command 0x{0:04X}")]
    Nack(u16),
    #[error("device busy (another interface, e.g. Bluetooth, is active)")]
    Busy,
    #[error("timeout waiting for the device")]
    Timeout,
    #[error("{0}")]
    Protocol(String),
}

pub type NanoResult<T> = Result<T, NanoError>;

/// Command codes: (CMD2 << 8) | CMD3, from NNOCommandDefs.h
#[allow(dead_code)]
pub mod cmd {
    pub const FLASH_GET_CHKSUM: u16 = 0x0015;
    pub const FILE_WRITE_DATA: u16 = 0x0025;
    pub const FILE_SET_WRITESIZE: u16 = 0x002A;
    pub const FILE_GET_READSIZE: u16 = 0x002D;
    pub const FILE_GET_DATA: u16 = 0x002E;
    pub const GOTO_TIVA_BL: u16 = 0x002F;
    pub const EEPROM_TEST: u16 = 0x0101;
    pub const ADC_TEST: u16 = 0x0102;
    pub const BQ_TEST: u16 = 0x0103;
    pub const SDRAM_TEST: u16 = 0x0104;
    pub const DLPC_ENABLE: u16 = 0x0105;
    pub const TMP_TEST: u16 = 0x0106;
    pub const HDC_TEST: u16 = 0x0107;
    pub const BT_TEST: u16 = 0x0108;
    pub const SDC_TEST: u16 = 0x0109;
    pub const LED_TEST: u16 = 0x010B;
    pub const BUTTON_TEST_RD: u16 = 0x010C;
    pub const BUTTON_TEST_WR: u16 = 0x010D;
    pub const TIVA_VER: u16 = 0x0216;
    pub const PERFORM_SCAN: u16 = 0x0218;
    pub const SCAN_GET_STATUS: u16 = 0x0219;
    pub const TIVA_RESET: u16 = 0x021A;
    pub const SET_PGA: u16 = 0x021B;
    pub const SCAN_CFG_APPLY: u16 = 0x021E;
    pub const SCAN_CFG_SAVE: u16 = 0x021F;
    pub const SCAN_CFG_READ: u16 = 0x0220;
    pub const SCAN_CFG_ERASEALL: u16 = 0x0221;
    pub const SCAN_CFG_NUM: u16 = 0x0222;
    pub const SCAN_GET_ACT_CFG: u16 = 0x0223;
    pub const SCAN_SET_ACT_CFG: u16 = 0x0224;
    pub const SET_DLPC_ONOFF_CTRL: u16 = 0x0225;
    pub const GET_PGA: u16 = 0x0228;
    pub const CALIB_STRUCT_READ: u16 = 0x022A;
    pub const SCAN_NUM_REPEATS: u16 = 0x022E;
    pub const SERIAL_NUMBER_READ: u16 = 0x0233;
    pub const WRITE_SCAN_NAME_TAG: u16 = 0x0234;
    pub const READ_SCAN_TIME: u16 = 0x0237;
    pub const DEL_LAST_SCAN_FILE_SD: u16 = 0x0238;
    pub const START_SCAN_INTERPRET: u16 = 0x0239;
    pub const MODEL_NAME_READ: u16 = 0x023C;
    pub const READ_TEMP: u16 = 0x0300;
    pub const READ_HUM: u16 = 0x0302;
    pub const SET_DATE_TIME: u16 = 0x0309;
    pub const READ_BATT_VOLT: u16 = 0x030A;
    pub const READ_TIVA_TEMP: u16 = 0x030B;
    pub const GET_DATE_TIME: u16 = 0x030C;
    pub const HIBERNATE_MODE: u16 = 0x030D;
    pub const SET_HIBERNATE: u16 = 0x030E;
    pub const GET_HIBERNATE: u16 = 0x030F;
    pub const GET_NUM_SCAN_FILES_SD: u16 = 0x0400;
    pub const READ_PHOTODETECTOR: u16 = 0x0402;
    pub const READ_DEVICE_STATUS: u16 = 0x0403;
    pub const READ_ERROR_STATUS: u16 = 0x0404;
    pub const RESET_ERROR_STATUS: u16 = 0x0405;
    pub const GET_SPECIFIC_ERR_STATUS: u16 = 0x0406;
    pub const GET_SPECIFIC_ERR_CODE: u16 = 0x0407;
    pub const SET_FIXED_PGA: u16 = 0x040C;
}

/// NNO_FILE_TYPE
#[allow(dead_code)]
pub mod file_type {
    pub const SCAN_DATA: u32 = 0;
    pub const SCAN_CONFIG: u32 = 1;
    pub const REF_CAL_DATA: u32 = 2;
    pub const REF_CAL_MATRIX: u32 = 3;
    pub const HADSNR_DATA: u32 = 4;
    pub const SCAN_CONFIG_LIST: u32 = 5;
    pub const SCAN_LIST: u32 = 6;
    pub const SCAN_DATA_FROM_SD: u32 = 7;
    pub const INTERPRET_DATA: u32 = 8;
}

/// Device status bits (NNOStatusDefs.h)
pub mod status {
    pub const TIVA: u32 = 0x01;
    pub const SCAN_IN_PROGRESS: u32 = 0x02;
    pub const SD_CARD_PRESENT: u32 = 0x04;
    pub const SD_CARD_OPER_IN_PROG: u32 = 0x08;
    pub const BLE_STACK_OPEN: u32 = 0x10;
    pub const ACTIVE_BLE_CONNECTION: u32 = 0x20;
    pub const SCAN_INTERPRET_IN_PROGRESS: u32 = 0x40;
}

pub struct Nano {
    dev: HidDevice,
    seq: u8,
    pub timeout_ms: i32,
    pub path: String,
}

fn le32(b: &[u8], off: usize) -> u32 {
    let mut a = [0u8; 4];
    for (i, v) in a.iter_mut().enumerate() {
        *v = b.get(off + i).copied().unwrap_or(0);
    }
    u32::from_le_bytes(a)
}

impl Nano {
    pub fn is_present(api: &HidApi) -> bool {
        api.device_list().any(|d| d.vendor_id() == VID && d.product_id() == PID)
    }

    pub fn open(api: &HidApi) -> NanoResult<Self> {
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == VID && d.product_id() == PID)
            .ok_or(NanoError::NotFound)?;
        let dev = info.open_device(api)?;
        dev.set_blocking_mode(true)?;
        let path = info.path().to_string_lossy().to_string();
        let mut n = Nano { dev, seq: 0, timeout_ms: DEFAULT_TIMEOUT_MS, path };
        n.drain();
        Ok(n)
    }

    /// Discard any stale input reports (e.g. after an earlier timeout).
    fn drain(&mut self) {
        let mut buf = [0u8; PKT];
        while let Ok(n) = self.dev.read_timeout(&mut buf, 0) {
            if n == 0 {
                break;
            }
        }
    }

    fn send(&mut self, flags: u8, seq: u8, command: u16, params: &[u8]) -> NanoResult<()> {
        let len = (2 + params.len()) as u16;
        let mut msg = Vec::with_capacity(4 + len as usize);
        msg.push(flags);
        msg.push(seq);
        msg.extend_from_slice(&len.to_le_bytes());
        msg.extend_from_slice(&command.to_le_bytes());
        msg.extend_from_slice(params);
        for chunk in msg.chunks(PKT) {
            let mut report = [0u8; PKT + 1]; // report ID 0 + 64 bytes
            report[1..1 + chunk.len()].copy_from_slice(chunk);
            self.dev.write(&report)?;
        }
        Ok(())
    }

    /// Read a full response message. Returns (resp flags, payload).
    fn recv(&mut self, command: u16) -> NanoResult<(u8, Vec<u8>)> {
        let mut buf = [0u8; PKT];
        let n = self.dev.read_timeout(&mut buf, self.timeout_ms)?;
        if n == 0 {
            return Err(NanoError::Timeout);
        }
        let flags = buf[0];
        let resp = (flags >> 4) & 0x3;
        let length = u16::from_le_bytes([buf[2], buf[3]]) as usize;
        if resp == 2 {
            return Err(NanoError::Busy);
        }
        if resp == 1 {
            return Err(NanoError::Nack(command));
        }
        let mut payload = Vec::with_capacity(length);
        payload.extend_from_slice(&buf[4..n.min(PKT)]);
        while payload.len() < length {
            let n = self.dev.read_timeout(&mut buf, self.timeout_ms)?;
            if n == 0 {
                return Err(NanoError::Timeout);
            }
            payload.extend_from_slice(&buf[..n]);
        }
        payload.truncate(length);
        Ok((resp, payload))
    }

    /// Read command (flags 0xC0): returns the response payload.
    pub fn read(&mut self, command: u16, params: &[u8]) -> NanoResult<Vec<u8>> {
        self.send(0xC0, 0, command, params)?;
        let (_, payload) = self.recv(command)?;
        if payload.is_empty() {
            return Err(NanoError::Nack(command));
        }
        Ok(payload)
    }

    /// Write command (flags 0x40) with acknowledgement; returns response payload (often empty).
    pub fn write(&mut self, command: u16, params: &[u8]) -> NanoResult<Vec<u8>> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.send(0x40, seq, command, params)?;
        Ok(self.recv(command)?.1)
    }

    /// Write command where the device does not reply (reset, hibernate).
    pub fn write_no_ack(&mut self, command: u16, params: &[u8]) -> NanoResult<()> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.send(0x40, seq, command, params)
    }

    fn read_u32(&mut self, command: u16) -> NanoResult<u32> {
        Ok(le32(&self.read(command, &[])?, 0))
    }

    // ---------------------------------------------------------------- info

    pub fn versions(&mut self) -> NanoResult<[u32; 7]> {
        let p = self.read(cmd::TIVA_VER, &[])?;
        let mut v = [0u32; 7];
        for (i, x) in v.iter_mut().enumerate() {
            *x = le32(&p, i * 4);
        }
        Ok(v)
    }

    fn read_string(&mut self, command: u16, len: usize) -> NanoResult<String> {
        let p = self.read(command, &[])?;
        let s = &p[..p.len().min(len)];
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        Ok(String::from_utf8_lossy(&s[..end]).trim().to_string())
    }

    pub fn serial_number(&mut self) -> NanoResult<String> {
        self.read_string(cmd::SERIAL_NUMBER_READ, 8)
    }

    pub fn model_name(&mut self) -> NanoResult<String> {
        self.read_string(cmd::MODEL_NAME_READ, 16)
    }

    pub fn device_status(&mut self) -> NanoResult<u32> {
        self.read_u32(cmd::READ_DEVICE_STATUS)
    }

    /// Returns (error status bits, raw error code bytes).
    pub fn error_status(&mut self) -> NanoResult<(u32, Vec<u8>)> {
        let p = self.read(cmd::READ_ERROR_STATUS, &[])?;
        Ok((le32(&p, 0), p.get(4..).map(|s| s.to_vec()).unwrap_or_default()))
    }

    pub fn reset_error_status(&mut self) -> NanoResult<()> {
        self.write(cmd::RESET_ERROR_STATUS, &[]).map(|_| ())
    }

    // ---------------------------------------------------------------- sensors

    /// (ambient, detector) temperature in hundredths of degC
    pub fn read_temp(&mut self) -> NanoResult<(i32, i32)> {
        let p = self.read(cmd::READ_TEMP, &[])?;
        Ok((le32(&p, 0) as i32, le32(&p, 4) as i32))
    }

    /// (humidity in hundredths of %, HDC temp in hundredths of degC)
    pub fn read_humidity(&mut self) -> NanoResult<(u32, i32)> {
        let p = self.read(cmd::READ_HUM, &[])?;
        Ok((le32(&p, 4), le32(&p, 0) as i32))
    }

    /// Battery voltage in hundredths of a volt
    pub fn read_battery(&mut self) -> NanoResult<u32> {
        self.read_u32(cmd::READ_BATT_VOLT)
    }

    pub fn read_tiva_temp(&mut self) -> NanoResult<i32> {
        Ok(self.read_u32(cmd::READ_TIVA_TEMP)? as i32)
    }

    pub fn read_photodetector(&mut self) -> NanoResult<(u32, u32, u32)> {
        let p = self.read(cmd::READ_PHOTODETECTOR, &[])?;
        Ok((le32(&p, 0), le32(&p, 4), le32(&p, 8)))
    }

    /// (year since 2000, month, day, weekday, hour, minute, second)
    pub fn get_date_time(&mut self) -> NanoResult<[u8; 7]> {
        let p = self.read(cmd::GET_DATE_TIME, &[])?;
        let mut d = [0u8; 7];
        for (i, v) in d.iter_mut().enumerate() {
            *v = p.get(i).copied().unwrap_or(0);
        }
        Ok(d)
    }

    pub fn set_date_time(&mut self, d: [u8; 7]) -> NanoResult<()> {
        self.write(cmd::SET_DATE_TIME, &d).map(|_| ())
    }

    // ---------------------------------------------------------------- scan

    pub fn set_num_repeats(&mut self, n: u16) -> NanoResult<()> {
        self.write(cmd::SCAN_NUM_REPEATS, &n.to_le_bytes()).map(|_| ())
    }

    /// Estimated scan time in ms for the active config and repeat count
    pub fn estimated_scan_time_ms(&mut self) -> NanoResult<u32> {
        self.read_u32(cmd::READ_SCAN_TIME)
    }

    pub fn set_scan_name_tag(&mut self, tag: &str) -> NanoResult<()> {
        let mut b = [0u8; 14];
        let s = tag.as_bytes();
        let n = s.len().min(13);
        b[..n].copy_from_slice(&s[..n]);
        self.write(cmd::WRITE_SCAN_NAME_TAG, &b).map(|_| ())
    }

    pub fn perform_scan(&mut self, store_in_sd: bool) -> NanoResult<()> {
        self.write(cmd::PERFORM_SCAN, &[store_in_sd as u8]).map(|_| ())
    }

    pub fn start_scan_interpret(&mut self) -> NanoResult<()> {
        self.write(cmd::START_SCAN_INTERPRET, &[]).map(|_| ())
    }

    pub fn set_fixed_pga(&mut self, fixed: bool, gain: u8) -> NanoResult<()> {
        self.write(cmd::SET_FIXED_PGA, &[fixed as u8, gain]).map(|_| ())
    }

    pub fn set_pga(&mut self, gain: u8) -> NanoResult<()> {
        self.write(cmd::SET_PGA, &[gain]).map(|_| ())
    }

    pub fn get_pga(&mut self) -> NanoResult<u8> {
        Ok(self.read(cmd::GET_PGA, &[])?[0])
    }

    /// Lamp control: `auto` lets the scan turn the lamp/DLPC on and off.
    pub fn set_lamp(&mut self, mode: LampMode) -> NanoResult<()> {
        match mode {
            LampMode::Auto => {
                self.write(cmd::SET_DLPC_ONOFF_CTRL, &[1])?;
                self.write(cmd::DLPC_ENABLE, &[0, 0])?;
            }
            LampMode::On => {
                self.write(cmd::SET_DLPC_ONOFF_CTRL, &[0])?;
                self.write(cmd::DLPC_ENABLE, &[1, 1])?;
            }
        }
        Ok(())
    }

    /// Read a file from the device (NNO_GetFileSizeToRead + NNO_GetFile)
    pub fn read_file(&mut self, ftype: u32) -> NanoResult<Vec<u8>> {
        let size = le32(&self.read(cmd::FILE_GET_READSIZE, &ftype.to_le_bytes())?, 0) as usize;
        if size == 0 {
            return Err(NanoError::Protocol(format!("device has no data for file type {}", ftype)));
        }
        let mut out = Vec::with_capacity(size);
        while out.len() < size {
            let chunk = self.read(cmd::FILE_GET_DATA, &[])?;
            out.extend_from_slice(&chunk);
        }
        out.truncate(size);
        Ok(out)
    }

    /// Run a scan and download the raw (serialized) scan data blob.
    pub fn scan(&mut self, store_in_sd: bool, mut progress: impl FnMut(u32)) -> NanoResult<Vec<u8>> {
        let est = self.estimated_scan_time_ms().unwrap_or(10_000);
        let timeout = Duration::from_millis((est as u64 * 3).max(15_000));
        let start = Instant::now();
        self.perform_scan(store_in_sd)?;
        loop {
            let st = self.device_status()?;
            progress(st);
            if st & status::SCAN_IN_PROGRESS == 0 {
                break;
            }
            if start.elapsed() > timeout {
                return Err(NanoError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.read_file(file_type::SCAN_DATA)
    }

    /// Ask the device to interpret the last scan (fallback path); returns (wavelengths, intensities).
    pub fn device_interpret(&mut self) -> NanoResult<(Vec<f64>, Vec<i32>)> {
        let start = Instant::now();
        self.start_scan_interpret()?;
        loop {
            let st = self.device_status()?;
            if st & status::SCAN_INTERPRET_IN_PROGRESS == 0 {
                break;
            }
            if start.elapsed() > Duration::from_secs(60) {
                return Err(NanoError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let blob = self.read_file(file_type::INTERPRET_DATA)?;
        let n = le32(&blob, 0) as usize;
        let mut wl = Vec::with_capacity(n);
        let mut it = Vec::with_capacity(n);
        for i in 0..n {
            let off = 4 + i * 12;
            if off + 12 > blob.len() {
                break;
            }
            wl.push(f64::from_le_bytes(blob[off..off + 8].try_into().unwrap()));
            it.push(le32(&blob, off + 8) as i32);
        }
        Ok((wl, it))
    }

    // ---------------------------------------------------------------- configs

    pub fn num_configs(&mut self) -> NanoResult<u8> {
        Ok(self.read(cmd::SCAN_CFG_NUM, &[])?[0])
    }

    pub fn active_config(&mut self) -> NanoResult<u8> {
        Ok(self.read(cmd::SCAN_GET_ACT_CFG, &[])?[0])
    }

    pub fn set_active_config(&mut self, index: u8) -> NanoResult<()> {
        self.write(cmd::SCAN_SET_ACT_CFG, &[index]).map(|_| ())
    }

    /// Serialized config blob stored at `index` in the device EEPROM
    pub fn read_config_blob(&mut self, index: u8) -> NanoResult<Vec<u8>> {
        let p = self.read(cmd::SCAN_CFG_READ, &(index as u32).to_le_bytes())?;
        let size = le32(&p, 0) as usize;
        if size == 0 || 4 + size > p.len() {
            return Err(NanoError::Protocol(format!("bad config record {}", index)));
        }
        Ok(p[4..4 + size].to_vec())
    }

    pub fn erase_all_configs(&mut self) -> NanoResult<()> {
        self.write(cmd::SCAN_CFG_ERASEALL, &[]).map(|_| ())
    }

    pub fn save_config_blob(&mut self, index: u8, blob: &[u8]) -> NanoResult<()> {
        if blob.len() > 255 {
            return Err(NanoError::Protocol("config blob too large".into()));
        }
        let mut p = vec![index, blob.len() as u8];
        p.extend_from_slice(blob);
        self.write(cmd::SCAN_CFG_SAVE, &p).map(|_| ())
    }

    /// Apply a config for the next scans without storing it; returns the pattern count.
    pub fn apply_config_blob(&mut self, blob: &[u8]) -> NanoResult<i32> {
        let r = self.write(cmd::SCAN_CFG_APPLY, blob)?;
        Ok(if r.len() >= 4 { le32(&r, 0) as i32 } else { 0 })
    }

    pub fn calib_coeffs_blob(&mut self) -> NanoResult<Vec<u8>> {
        self.read(cmd::CALIB_STRUCT_READ, &[])
    }

    // ---------------------------------------------------------------- utilities

    pub fn num_sd_scans(&mut self) -> NanoResult<u32> {
        self.read_u32(cmd::GET_NUM_SCAN_FILES_SD)
    }

    pub fn read_last_sd_scan(&mut self) -> NanoResult<Vec<u8>> {
        self.read_file(file_type::SCAN_DATA_FROM_SD)
    }

    pub fn delete_last_sd_scan(&mut self) -> NanoResult<()> {
        self.write(cmd::DEL_LAST_SCAN_FILE_SD, &[]).map(|_| ())
    }

    pub fn get_hibernate(&mut self) -> NanoResult<bool> {
        Ok(self.read(cmd::GET_HIBERNATE, &[])?[0] != 0)
    }

    pub fn set_hibernate(&mut self, on: bool) -> NanoResult<()> {
        self.write(cmd::SET_HIBERNATE, &[on as u8]).map(|_| ())
    }

    pub fn reset(&mut self) -> NanoResult<()> {
        self.write_no_ack(cmd::TIVA_RESET, &[])
    }

    pub fn led_test(&mut self, on: bool) -> NanoResult<()> {
        self.write(cmd::LED_TEST, &[on as u8]).map(|_| ())
    }

    /// Simple board self tests; returns first payload byte (0 = pass)
    pub fn self_test(&mut self, command: u16) -> NanoResult<u8> {
        let saved = self.timeout_ms;
        self.timeout_ms = 40_000;
        let r = self.read(command, &[]);
        self.timeout_ms = saved;
        Ok(r?[0])
    }

    /// BQ24250 charger test: (status, batt_volt, usb_det, charge_curr, pts_fault)
    pub fn bq_test(&mut self) -> NanoResult<[i32; 5]> {
        let p = self.read(cmd::BQ_TEST, &[])?;
        let mut v = [0i32; 5];
        for (i, x) in v.iter_mut().enumerate() {
            *x = le32(&p, i * 4) as i32;
        }
        Ok(v)
    }

    pub fn button_test_write(&mut self, enable: bool) -> NanoResult<()> {
        self.write(cmd::BUTTON_TEST_WR, &[enable as u8]).map(|_| ())
    }

    pub fn button_test_read(&mut self) -> NanoResult<u8> {
        Ok(self.read(cmd::BUTTON_TEST_RD, &[])?[0])
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LampMode {
    Auto,
    On,
}

/// Formats a version word stored as MAJOR.MINOR.PATCH in bytes 2,1,0.
pub fn fmt_ver(v: u32) -> String {
    format!("{}.{}.{}", (v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF)
}

/// DLPC flash build version: MAJOR(8).MINOR(8).BUILD(16)
pub fn fmt_dlpc_ver(v: u32) -> String {
    format!("{}.{}.{}", v >> 24, (v >> 16) & 0xFF, v & 0xFFFF)
}
