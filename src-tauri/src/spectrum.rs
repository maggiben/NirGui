//! Spectrum post-processing for the UI: reference handling, absorbance /
//! reflectance, CSV export and scan-configuration validation.

use crate::dlpspec::interpret::{absorbance, col_gen_pat_def, had_gen_pat_def, interp_reference, reflectance};
use crate::dlpspec::{
    exp_time_us, nm_to_column, CalibCoeffs, ScanConfig, ScanResults, SlewScanConfig, ADC_DATA_LEN, COLUMN_TYPE,
    HADAMARD_TYPE, MAX_PATTERNS_PER_SCAN, SLEW_SCAN_MAX_SECTIONS,
};
use serde::{Deserialize, Serialize};

pub const MIN_WAVELENGTH: u16 = 900;
pub const MAX_WAVELENGTH: u16 = 1700;
pub const MIN_WIDTH_PX: u8 = 2;
pub const MAX_WIDTH_PX: u8 = 52;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ReferenceMode {
    /// Factory reference stored in the device EEPROM (built-in reference)
    Builtin,
    /// A previous scan of a reference sample chosen by the user
    Local,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanMeta {
    pub scan_name: String,
    pub timestamp: String,
    pub serial_number: String,
    pub config_name: String,
    pub num_repeats: u16,
    pub pga: u8,
    /// Scan header `system_temp_hundredths`: firmware stores the HDC1000 (board) temperature here
    pub system_temp_c: f64,
    /// Scan header `detector_temp_hundredths`: TMP006 detector (object) temperature
    pub detector_temp_c: f64,
    pub humidity_pct: f64,
    pub lamp_pd: u16,
    pub scan_data_index: u32,
    pub length: usize,
    pub sections: Vec<SectionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SectionInfo {
    pub scan_type: String,
    pub start_nm: u16,
    pub end_nm: u16,
    pub width_px: u8,
    pub width_nm: f64,
    pub num_patterns: u16,
    pub exposure_us: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanView {
    pub file_stem: String,
    pub meta: ScanMeta,
    pub wavelength: Vec<f64>,
    pub intensity: Vec<i32>,
    pub reference: Option<Vec<i32>>,
    pub absorbance: Option<Vec<f64>>,
    pub reflectance: Option<Vec<f64>>,
    pub reference_mode: ReferenceMode,
    pub reference_error: Option<String>,
    pub extra: serde_json::Value,
}

/// GUI shows widths in nm using a fixed 1.17 nm/px approximation.
pub fn width_nm(px: u8) -> f64 {
    (1700.0 - 900.0) / (854.0 * 0.8) * px as f64
}

fn type_name(t: u8) -> String {
    match t {
        COLUMN_TYPE => "Column".into(),
        HADAMARD_TYPE => "Hadamard".into(),
        _ => format!("type {}", t),
    }
}

pub fn meta_of(r: &ScanResults) -> ScanMeta {
    let h = &r.head;
    ScanMeta {
        scan_name: h.scan_name.clone(),
        timestamp: format!("20{:02}-{:02}-{:02} {:02}:{:02}:{:02}", h.year, h.month, h.day, h.hour, h.minute, h.second),
        serial_number: h.serial_number.clone(),
        config_name: r.cfg.config_name.clone(),
        num_repeats: r.cfg.num_repeats,
        pga: h.pga,
        system_temp_c: h.system_temp_hundredths as f64 / 100.0,
        detector_temp_c: h.detector_temp_hundredths as f64 / 100.0,
        humidity_pct: h.humidity_hundredths as f64 / 100.0,
        lamp_pd: h.lamp_pd,
        scan_data_index: h.scan_data_index,
        length: r.length,
        sections: (0..r.cfg.num_sections as usize)
            .map(|i| {
                let s = r.cfg.section(i);
                SectionInfo {
                    scan_type: type_name(s.section_scan_type),
                    start_nm: s.wavelength_start_nm,
                    end_nm: s.wavelength_end_nm,
                    width_px: s.width_px,
                    width_nm: width_nm(s.width_px),
                    num_patterns: s.num_patterns,
                    exposure_us: exp_time_us(s.exposure_time),
                }
            })
            .collect(),
    }
}

/// Builds ScanResults from the device-side interpretation (fallback path).
pub fn results_from_device(wl: &[f64], it: &[i32]) -> ScanResults {
    let mut r = ScanResults::empty();
    let n = wl.len().min(it.len()).min(ADC_DATA_LEN);
    r.wavelength[..n].copy_from_slice(&wl[..n]);
    r.intensity[..n].copy_from_slice(&it[..n]);
    r.length = n;
    r.head.scan_name = "device-interpreted".into();
    r
}

pub fn make_view(
    r: &ScanResults,
    mode: ReferenceMode,
    ref_cal: Option<&[u8]>,
    ref_matrix: Option<&[u8]>,
    local_ref: Option<&ScanResults>,
    extra: serde_json::Value,
) -> ScanView {
    let n = r.length;
    let meta = meta_of(r);
    let reference: Result<Option<Vec<i32>>, String> = match mode {
        ReferenceMode::None => Ok(None),
        ReferenceMode::Builtin => match (ref_cal, ref_matrix) {
            (Some(c), Some(m)) => interp_reference(c, m, r).map(|x| Some(x.intensity[..n].to_vec())).map_err(|e| e.to_string()),
            _ => Err("factory reference not available (connect the device once)".into()),
        },
        ReferenceMode::Local => match local_ref {
            Some(lr) if lr.length == n && lr.cfg_eq_sections(&r.cfg) => Ok(Some(lr.intensity[..n].to_vec())),
            Some(_) => Err("local reference was taken with a different scan configuration".into()),
            None => Err("no local reference set".into()),
        },
    };
    let (reference, reference_error) = match reference {
        Ok(v) => (v, None),
        Err(e) => (None, Some(e)),
    };
    let sample = &r.intensity[..n];
    let finite = |v: f64| if v.is_finite() { v } else { f64::NAN };
    let absorbance_v = reference.as_ref().map(|rf| sample.iter().zip(rf).map(|(&s, &q)| finite(absorbance(s, q))).collect());
    let reflectance_v = reference.as_ref().map(|rf| sample.iter().zip(rf).map(|(&s, &q)| finite(reflectance(s, q))).collect());
    let stem_name = if meta.scan_name.is_empty() { "scan".to_string() } else { meta.scan_name.replace(['/', ' ', ':'], "_") };
    let h = &r.head;
    ScanView {
        file_stem: format!(
            "{}_{}_20{:02}{:02}{:02}_{:02}{:02}{:02}",
            stem_name,
            r.cfg.config_name.replace(['/', ' ', ':'], "_"),
            h.year,
            h.month,
            h.day,
            h.hour,
            h.minute,
            h.second
        ),
        meta,
        wavelength: r.wavelength[..n].to_vec(),
        intensity: sample.to_vec(),
        reference,
        absorbance: absorbance_v,
        reflectance: reflectance_v,
        reference_mode: mode,
        reference_error,
        extra,
    }
}

impl ScanResults {
    fn cfg_eq_sections(&self, other: &SlewScanConfig) -> bool {
        self.cfg.num_sections == other.num_sections
            && (0..other.num_sections as usize).all(|i| {
                let (a, b) = (self.cfg.section(i), other.section(i));
                a.section_scan_type == b.section_scan_type
                    && a.width_px == b.width_px
                    && a.wavelength_start_nm == b.wavelength_start_nm
                    && a.wavelength_end_nm == b.wavelength_end_nm
                    && a.num_patterns == b.num_patterns
            })
    }
}

pub fn to_csv(v: &ScanView) -> String {
    let m = &v.meta;
    let mut s = String::new();
    s.push_str(&format!("Scan Name:,{}\n", m.scan_name));
    s.push_str(&format!("Date/Time:,{}\n", m.timestamp));
    s.push_str(&format!("Serial Number:,{}\n", m.serial_number));
    s.push_str(&format!("Scan Config Name:,{}\n", m.config_name));
    s.push_str(&format!("Num Repeats:,{}\n", m.num_repeats));
    s.push_str(&format!("PGA Gain:,{}\n", m.pga));
    s.push_str(&format!("System Temp (C) [HDC1000]:,{:.2}\n", m.system_temp_c));
    s.push_str(&format!("Detector Temp (C) [TMP006]:,{:.2}\n", m.detector_temp_c));
    s.push_str(&format!("Humidity (%):,{:.2}\n", m.humidity_pct));
    s.push_str(&format!("Lamp PD:,{}\n", m.lamp_pd));
    s.push_str(&format!("Data Index:,{}\n", m.scan_data_index));
    for (i, sec) in m.sections.iter().enumerate() {
        s.push_str(&format!(
            "Section {}:,{},{} nm,{} nm,{:.2} nm ({} px),{} patterns,{} us\n",
            i + 1,
            sec.scan_type,
            sec.start_nm,
            sec.end_nm,
            sec.width_nm,
            sec.width_px,
            sec.num_patterns,
            sec.exposure_us
        ));
    }
    s.push_str(&format!(
        "Reference:,{}\n",
        match v.reference_mode {
            ReferenceMode::Builtin => "Built-in (factory)",
            ReferenceMode::Local => "Local reference scan",
            ReferenceMode::None => "None",
        }
    ));
    s.push_str("***Scan Data***\n");
    s.push_str("Wavelength (nm),Absorbance (AU),Reflectance,Reference Signal (unitless),Sample Signal (unitless)\n");
    for i in 0..v.wavelength.len() {
        let a = v.absorbance.as_ref().map(|x| fmt_f(x[i])).unwrap_or_default();
        let r = v.reflectance.as_ref().map(|x| fmt_f(x[i])).unwrap_or_default();
        let rf = v.reference.as_ref().map(|x| x[i].to_string()).unwrap_or_default();
        s.push_str(&format!("{:.6},{},{},{},{}\n", v.wavelength[i], a, r, rf, v.intensity[i]));
    }
    s
}

fn fmt_f(x: f64) -> String {
    if x.is_finite() {
        format!("{:.6}", x)
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ConfigCheck {
    pub ok: bool,
    pub errors: Vec<String>,
    /// Maximum allowed patterns per section (when calibration is known)
    pub max_patterns: Vec<Option<u16>>,
    pub total_patterns: u32,
}

/// Mirrors ScanConfigDialog::getMaxPatterns (without the DMD shift-vector packing check).
pub fn max_patterns(sec_type: u8, start: u16, end: u16, width_px: u8, calib: &CalibCoeffs) -> u16 {
    if start < MIN_WAVELENGTH || end > MAX_WAVELENGTH || start > end {
        return 0;
    }
    let mut cfg = ScanConfig {
        scan_type: sec_type,
        wavelength_start_nm: start,
        wavelength_end_nm: end,
        width_px,
        num_repeats: 1,
        ..Default::default()
    };
    let mut best = 0u16;
    if sec_type == COLUMN_TYPE {
        for i in 8..=MAX_PATTERNS_PER_SCAN as u16 {
            cfg.num_patterns = i;
            let Ok(p) = col_gen_pat_def(&cfg, calib) else { break };
            let span = (*p.col_mid_pix.last().unwrap_or(&0) as i32 - *p.col_mid_pix.first().unwrap_or(&0) as i32).abs();
            if (i as i32) <= span {
                best = i;
            } else {
                break;
            }
        }
    } else {
        let c = &calib.pixel_to_wavelength_coeffs;
        let (Ok(a), Ok(b)) = (nm_to_column(start as f64, c), nm_to_column(end as f64, c)) else { return 0 };
        let span = (a - b).abs();
        for i in 1..MAX_PATTERNS_PER_SCAN as u16 {
            cfg.num_patterns = i;
            if let Ok(p) = had_gen_pat_def(&cfg, calib) {
                if (i as f64) <= span
                    && (p.num_patterns as usize) < MAX_PATTERNS_PER_SCAN
                    && (p.num_patterns as usize) < ADC_DATA_LEN - MAX_PATTERNS_PER_SCAN / 24
                {
                    best = i;
                }
            }
        }
    }
    best
}

pub fn check_config(c: &SlewScanConfig, calib: Option<&CalibCoeffs>) -> ConfigCheck {
    let mut e = Vec::new();
    let mut maxes = Vec::new();
    if c.config_name.trim().is_empty() {
        e.push("Configuration name is empty".into());
    }
    if c.config_name.len() > 39 {
        e.push("Configuration name is longer than 39 characters".into());
    }
    if c.num_repeats == 0 {
        e.push("Number of scans to average must be at least 1".into());
    }
    if c.num_sections == 0 || c.num_sections as usize > SLEW_SCAN_MAX_SECTIONS {
        e.push(format!("Number of sections must be 1..{}", SLEW_SCAN_MAX_SECTIONS));
    }
    let mut total = 0u32;
    for i in 0..(c.num_sections as usize).min(SLEW_SCAN_MAX_SECTIONS) {
        let s = c.section(i);
        let n = i + 1;
        if s.section_scan_type > HADAMARD_TYPE {
            e.push(format!("Section {}: invalid scan type", n));
        }
        if s.wavelength_start_nm < MIN_WAVELENGTH || s.wavelength_end_nm > MAX_WAVELENGTH || s.wavelength_start_nm >= s.wavelength_end_nm {
            e.push(format!("Section {}: wavelength range must be within {}-{} nm and start < end", n, MIN_WAVELENGTH, MAX_WAVELENGTH));
        }
        if s.width_px < MIN_WIDTH_PX || s.width_px > MAX_WIDTH_PX {
            e.push(format!("Section {}: width must be {}-{} px", n, MIN_WIDTH_PX, MAX_WIDTH_PX));
        }
        if s.exposure_time > 6 {
            e.push(format!("Section {}: invalid exposure time", n));
        }
        if s.num_patterns < 2 {
            e.push(format!("Section {}: at least 2 patterns (points) are required", n));
        }
        total += s.num_patterns as u32;
        let m = calib.map(|cal| max_patterns(s.section_scan_type, s.wavelength_start_nm, s.wavelength_end_nm, s.width_px, cal));
        if let Some(m) = m {
            if m < 2 {
                e.push(format!("Section {}: start/end wavelength and width do not allow 2 patterns", n));
            } else if s.num_patterns > m {
                e.push(format!("Section {}: maximum number of patterns is {}", n, m));
            }
        }
        maxes.push(m);
    }
    if total > MAX_PATTERNS_PER_SCAN as u32 {
        e.push(format!("Total number of patterns {} exceeds {}", total, MAX_PATTERNS_PER_SCAN));
    }
    ConfigCheck { ok: e.is_empty(), errors: e, max_patterns: maxes, total_patterns: total }
}

pub fn validate_config(c: &SlewScanConfig, calib: Option<&CalibCoeffs>) -> Result<(), String> {
    let r = check_config(c, calib);
    if r.ok {
        Ok(())
    } else {
        Err(format!("'{}': {}", c.config_name, r.errors.join("; ")))
    }
}
