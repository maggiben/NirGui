//! Rust port of the parts of the TI DLP Spectrum Library (dlpspec v2.x) that a
//! host application needs: (de)serialization of scan configurations, scan data
//! and the reference calibration matrix, plus scan interpretation
//! (column / Hadamard / slew) and reference interpolation.
//!
//! The arithmetic intentionally mirrors the C implementation, including its
//! integer truncation behaviour, so results match the Windows GUI.

pub mod had_tables;
pub mod interpret;
pub mod tpl;

use serde::{Deserialize, Serialize};
use tpl::{TplImage, TplWriter};

pub const ADC_DATA_LEN: usize = 864;
pub const SLEW_SCAN_MAX_SECTIONS: usize = 5;
pub const SCAN_CFG_FILENAME_LEN: usize = 40;
pub const NANO_SER_NUM_LEN: usize = 8;
pub const SCAN_NAME_LEN: usize = 20;
pub const NUM_COEFFS: usize = 3;
pub const REF_CAL_INTERP_WIDTH: usize = 19;
pub const REF_CAL_INTERP_WAVELENGTH: usize = 50;
pub const MIN_DMD_COLUMN: f64 = 0.0;
pub const MAX_DMD_COLUMN: f64 = 853.0;
pub const MAX_PATTERNS_PER_SCAN: usize = 624;
pub const CUR_SCANDATA_VERSION: u32 = 1;

pub const COLUMN_TYPE: u8 = 0;
pub const HADAMARD_TYPE: u8 = 1;
pub const SLEW_TYPE: u8 = 2;

// TPL format strings (see dlpspec_scan.h / dlpspec_types.h)
const SCAN_CONFIG_HEAD_FORMAT: &str = "cvc#c#";
const SCAN_CFG_STUB_FORMAT: &str = "vvcvv";
const SCAN_DATA_HEAD_FORMAT: &str = "c#ccccccc" /*date*/;
const SCAN_DATA_HEAD_BODY_FORMAT: &str = "jjvvu$(f#f#)c#vccc";

fn fmt_cfg() -> String {
    format!("S({}{})", SCAN_CONFIG_HEAD_FORMAT, SCAN_CFG_STUB_FORMAT)
}
fn fmt_slew_cfg_head() -> String {
    format!("S({}vc)", SCAN_CONFIG_HEAD_FORMAT)
}
const FMT_SLEW_CFG_SECT: &str = "S(ccvvvv)#";
fn fmt_scan_data() -> String {
    format!(
        "S(u{}{}{}{}i#)",
        SCAN_DATA_HEAD_FORMAT, SCAN_DATA_HEAD_BODY_FORMAT, SCAN_CONFIG_HEAD_FORMAT, SCAN_CFG_STUB_FORMAT
    )
}
fn fmt_slew_data_head() -> String {
    format!("S(u{}{})", SCAN_DATA_HEAD_FORMAT, SCAN_DATA_HEAD_BODY_FORMAT)
}
const FMT_ADC: &str = "i#";
const FMT_REF_CAL_MATRIX: &str = "S(c#f#v##)";
const FMT_CALIB: &str = "S(f#f#)";

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("TPL: {0}")]
    Tpl(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("illegal scan type {0}")]
    IllegalScanType(u8),
    #[error("spectrum library failure: {0}")]
    Fail(String),
}

pub type SpecResult<T> = Result<T, SpecError>;

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn to_fixed<const N: usize>(s: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let bytes = s.as_bytes();
    let n = bytes.len().min(N - 1);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct CalibCoeffs {
    pub shift_vector_coeffs: [f64; NUM_COEFFS],
    pub pixel_to_wavelength_coeffs: [f64; NUM_COEFFS],
}

impl CalibCoeffs {
    pub fn from_blob(buf: &[u8]) -> SpecResult<Self> {
        let mut t = TplImage::parse(buf)?;
        t.expect_format(FMT_CALIB)?;
        let mut c = CalibCoeffs::default();
        for v in c.shift_vector_coeffs.iter_mut() {
            *v = t.f64()?;
        }
        for v in c.pixel_to_wavelength_coeffs.iter_mut() {
            *v = t.f64()?;
        }
        Ok(c)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct SlewSection {
    pub section_scan_type: u8,
    pub width_px: u8,
    pub wavelength_start_nm: u16,
    pub wavelength_end_nm: u16,
    pub num_patterns: u16,
    /// Index into EXP_TIME (0 = 635 us ... 6 = 60960 us)
    pub exposure_time: u16,
}

/// A scan configuration. Legacy single-section configs are converted to a
/// one-section slew config on read (exactly like dlpspec_copy_scanData_hdr_to_scanResults).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SlewScanConfig {
    pub scan_type: u8,
    pub scan_config_index: u16,
    pub serial_number: String,
    pub config_name: String,
    pub num_repeats: u16,
    pub num_sections: u8,
    pub sections: Vec<SlewSection>,
}

impl SlewScanConfig {
    pub fn section(&self, i: usize) -> SlewSection {
        self.sections.get(i).copied().unwrap_or_default()
    }

    pub fn total_patterns(&self) -> u32 {
        (0..self.num_sections as usize).map(|i| self.section(i).num_patterns as u32).sum()
    }

    /// Decode a configuration blob as returned by NNO_CMD_SCAN_CFG_READ.
    pub fn from_blob(buf: &[u8]) -> SpecResult<Self> {
        let first = TplImage::parse(buf)?;
        if first.format == fmt_cfg() {
            // Legacy scanConfig
            let mut t = first;
            let scan_type = t.u8()?;
            let idx = t.u16()?;
            let sn = t.bytes::<NANO_SER_NUM_LEN>()?;
            let name = t.bytes::<SCAN_CFG_FILENAME_LEN>()?;
            let start = t.u16()?;
            let end = t.u16()?;
            let width = t.u8()?;
            let np = t.u16()?;
            let rep = t.u16()?;
            return Ok(SlewScanConfig {
                scan_type: SLEW_TYPE,
                scan_config_index: idx,
                serial_number: cstr(&sn),
                config_name: cstr(&name),
                num_repeats: rep,
                num_sections: 1,
                sections: vec![SlewSection {
                    section_scan_type: scan_type,
                    width_px: width,
                    wavelength_start_nm: start,
                    wavelength_end_nm: end,
                    num_patterns: np,
                    exposure_time: 0,
                }],
            });
        }
        let head_len = first.total_len;
        let mut t = first;
        t.expect_format(&fmt_slew_cfg_head())?;
        let scan_type = t.u8()?;
        let idx = t.u16()?;
        let sn = t.bytes::<NANO_SER_NUM_LEN>()?;
        let name = t.bytes::<SCAN_CFG_FILENAME_LEN>()?;
        let rep = t.u16()?;
        let nsec = t.u8()?;
        let mut s = TplImage::parse(&buf[head_len..])?;
        s.expect_format(FMT_SLEW_CFG_SECT)?;
        let count = s.fxlens.first().copied().unwrap_or(0) as usize;
        let mut sections = Vec::with_capacity(count);
        for _ in 0..count {
            sections.push(SlewSection {
                section_scan_type: s.u8()?,
                width_px: s.u8()?,
                wavelength_start_nm: s.u16()?,
                wavelength_end_nm: s.u16()?,
                num_patterns: s.u16()?,
                exposure_time: s.u16()?,
            });
        }
        // The firmware stores unused sections uninitialized: blank them.
        for s in sections.iter_mut().skip(nsec as usize) {
            *s = SlewSection::default();
        }
        Ok(SlewScanConfig {
            scan_type,
            scan_config_index: idx,
            serial_number: cstr(&sn),
            config_name: cstr(&name),
            num_repeats: rep,
            num_sections: nsec,
            sections,
        })
    }

    /// Serialize as a slew configuration (head image + 5-section image),
    /// matching dlpspec_scan_write_configuration().
    pub fn to_blob(&self) -> Vec<u8> {
        let mut head = TplWriter::new(&fmt_slew_cfg_head(), &[NANO_SER_NUM_LEN as u32, SCAN_CFG_FILENAME_LEN as u32]);
        head.u8(SLEW_TYPE)
            .u16(self.scan_config_index)
            .bytes(&to_fixed::<NANO_SER_NUM_LEN>(&self.serial_number))
            .bytes(&to_fixed::<SCAN_CFG_FILENAME_LEN>(&self.config_name))
            .u16(self.num_repeats)
            .u8(self.num_sections);
        let mut sect = TplWriter::new(FMT_SLEW_CFG_SECT, &[SLEW_SCAN_MAX_SECTIONS as u32]);
        for i in 0..SLEW_SCAN_MAX_SECTIONS {
            let s = self.section(i);
            sect.u8(s.section_scan_type)
                .u8(s.width_px)
                .u16(s.wavelength_start_nm)
                .u16(s.wavelength_end_nm)
                .u16(s.num_patterns)
                .u16(s.exposure_time);
        }
        let mut out = head.finish();
        out.extend(sect.finish());
        out
    }
}

/// Common header of scanData / slewScanData.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanDataHeader {
    pub header_version: u32,
    pub scan_name: String,
    pub year: u8,
    pub month: u8,
    pub day: u8,
    pub day_of_week: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub system_temp_hundredths: i16,
    pub detector_temp_hundredths: i16,
    pub humidity_hundredths: u16,
    pub lamp_pd: u16,
    pub scan_data_index: u32,
    pub calibration_coeffs: CalibCoeffs,
    pub serial_number: String,
    pub adc_data_length: u16,
    pub black_pattern_first: u8,
    pub black_pattern_period: u8,
    pub pga: u8,
}

fn read_scan_data_header(t: &mut TplImage) -> SpecResult<ScanDataHeader> {
    let header_version = t.u32()?;
    let name = t.bytes::<SCAN_NAME_LEN>()?;
    let mut h = ScanDataHeader {
        header_version,
        scan_name: cstr(&name),
        year: t.u8()?,
        month: t.u8()?,
        day: t.u8()?,
        day_of_week: t.u8()?,
        hour: t.u8()?,
        minute: t.u8()?,
        second: t.u8()?,
        system_temp_hundredths: t.i16()?,
        detector_temp_hundredths: t.i16()?,
        humidity_hundredths: t.u16()?,
        lamp_pd: t.u16()?,
        scan_data_index: t.u32()?,
        ..Default::default()
    };
    for v in h.calibration_coeffs.shift_vector_coeffs.iter_mut() {
        *v = t.f64()?;
    }
    for v in h.calibration_coeffs.pixel_to_wavelength_coeffs.iter_mut() {
        *v = t.f64()?;
    }
    h.serial_number = cstr(&t.bytes::<NANO_SER_NUM_LEN>()?);
    h.adc_data_length = t.u16()?;
    h.black_pattern_first = t.u8()?;
    h.black_pattern_period = t.u8()?;
    h.pga = t.u8()?;
    Ok(h)
}

/// Legacy (non-slew) scan configuration embedded in scanData.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanConfig {
    pub scan_type: u8,
    pub scan_config_index: u16,
    pub serial_number: String,
    pub config_name: String,
    pub wavelength_start_nm: u16,
    pub wavelength_end_nm: u16,
    pub width_px: u8,
    pub num_patterns: u16,
    pub num_repeats: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScanDataConfig {
    Single(ScanConfig),
    Slew(SlewScanConfig),
}

/// Deserialized scan data (uScanData).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanData {
    pub head: ScanDataHeader,
    pub cfg: ScanDataConfig,
    pub adc_data: Vec<i32>, // always ADC_DATA_LEN long
}

impl ScanData {
    /// Equivalent of dlpspec_scan_read_data(): accepts both the single-image
    /// scanData format and the 4-image slewScanData format.
    pub fn from_blob(buf: &[u8]) -> SpecResult<Self> {
        let first = TplImage::parse(buf)?;
        if first.format == fmt_scan_data() {
            let mut t = first;
            let head = read_scan_data_header(&mut t)?;
            let cfg = ScanConfig {
                scan_type: t.u8()?,
                scan_config_index: t.u16()?,
                serial_number: cstr(&t.bytes::<NANO_SER_NUM_LEN>()?),
                config_name: cstr(&t.bytes::<SCAN_CFG_FILENAME_LEN>()?),
                wavelength_start_nm: t.u16()?,
                wavelength_end_nm: t.u16()?,
                width_px: t.u8()?,
                num_patterns: t.u16()?,
                num_repeats: t.u16()?,
            };
            let n = t.fxlens.last().copied().unwrap_or(ADC_DATA_LEN as u32) as usize;
            let mut adc = vec![0i32; ADC_DATA_LEN];
            for i in 0..n.min(ADC_DATA_LEN) {
                adc[i] = t.i32()?;
            }
            return Ok(ScanData { head, cfg: ScanDataConfig::Single(cfg), adc_data: adc });
        }
        // slewScanData: data head | cfg head | sections | adc data
        let mut off = first.total_len;
        let mut t = first;
        t.expect_format(&fmt_slew_data_head())?;
        let head = read_scan_data_header(&mut t)?;
        let cfg = SlewScanConfig::from_blob(&buf[off..])?;
        // advance past cfg head + sections images
        let h = TplImage::parse(&buf[off..])?;
        off += h.total_len;
        let s = TplImage::parse(&buf[off..])?;
        off += s.total_len;
        let mut a = TplImage::parse(&buf[off..])?;
        a.expect_format(FMT_ADC)?;
        let n = a.fxlens.first().copied().unwrap_or(0) as usize;
        let mut adc = vec![0i32; ADC_DATA_LEN];
        for i in 0..n.min(ADC_DATA_LEN) {
            adc[i] = a.i32()?;
        }
        Ok(ScanData { head, cfg: ScanDataConfig::Slew(cfg), adc_data: adc })
    }

    pub fn scan_type(&self) -> u8 {
        match &self.cfg {
            ScanDataConfig::Single(c) => c.scan_type,
            ScanDataConfig::Slew(c) => c.scan_type,
        }
    }

    pub fn slew_config(&self) -> SlewScanConfig {
        match &self.cfg {
            ScanDataConfig::Slew(c) => c.clone(),
            ScanDataConfig::Single(c) => SlewScanConfig {
                scan_type: SLEW_TYPE,
                scan_config_index: c.scan_config_index,
                serial_number: c.serial_number.clone(),
                config_name: c.config_name.clone(),
                num_repeats: c.num_repeats,
                num_sections: 1,
                sections: vec![SlewSection {
                    section_scan_type: c.scan_type,
                    width_px: c.width_px,
                    wavelength_start_nm: c.wavelength_start_nm,
                    wavelength_end_nm: c.wavelength_end_nm,
                    num_patterns: c.num_patterns,
                    exposure_time: 0,
                }],
            },
        }
    }
}

/// Reference calibration matrix (refCalMatrix).
#[derive(Debug, Clone)]
pub struct RefCalMatrix {
    pub width: [u8; REF_CAL_INTERP_WIDTH],
    pub wavelength: [f64; REF_CAL_INTERP_WAVELENGTH],
    pub ref_lookup: [[u16; REF_CAL_INTERP_WAVELENGTH]; REF_CAL_INTERP_WIDTH],
}

impl RefCalMatrix {
    pub fn from_blob(buf: &[u8]) -> SpecResult<Self> {
        let mut t = TplImage::parse(buf)?;
        t.expect_format(FMT_REF_CAL_MATRIX)?;
        let mut m = RefCalMatrix {
            width: [0; REF_CAL_INTERP_WIDTH],
            wavelength: [0.0; REF_CAL_INTERP_WAVELENGTH],
            ref_lookup: [[0; REF_CAL_INTERP_WAVELENGTH]; REF_CAL_INTERP_WIDTH],
        };
        for w in m.width.iter_mut() {
            *w = t.u8()?;
        }
        for w in m.wavelength.iter_mut() {
            *w = t.f64()?;
        }
        for row in m.ref_lookup.iter_mut() {
            for v in row.iter_mut() {
                *v = t.u16()?;
            }
        }
        Ok(m)
    }
}

/// Interpreted scan (scanResults).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResults {
    pub head: ScanDataHeader,
    pub cfg: SlewScanConfig,
    pub wavelength: Vec<f64>, // ADC_DATA_LEN entries
    pub intensity: Vec<i32>,  // ADC_DATA_LEN entries
    pub length: usize,
}

impl ScanResults {
    pub fn empty() -> Self {
        ScanResults {
            head: ScanDataHeader::default(),
            cfg: SlewScanConfig::default(),
            wavelength: vec![0.0; ADC_DATA_LEN],
            intensity: vec![0; ADC_DATA_LEN],
            length: 0,
        }
    }

    pub fn wavelengths(&self) -> &[f64] {
        &self.wavelength[..self.length]
    }
    pub fn intensities(&self) -> &[i32] {
        &self.intensity[..self.length]
    }
}

/// Exposure time enum (EXP_TIME) to microseconds.
pub fn exp_time_us(e: u16) -> u32 {
    match e {
        0 => 635,
        1 => 1270,
        2 => 2450,
        3 => 5080,
        4 => 15240,
        5 => 30480,
        6 => 60960,
        _ => 0,
    }
}

/// dlpspec_util_columnToNm
pub fn column_to_nm(column: f64, c: &[f64; 3]) -> f64 {
    c[2] * column * column + c[1] * column + c[0]
}

/// dlpspec_util_nmToColumn
pub fn nm_to_column(nm: f64, c: &[f64; 3]) -> SpecResult<f64> {
    if c[2] == 0.0 {
        if c[1] != 0.0 {
            return Ok((nm - c[0]) / c[1]);
        }
        return Err(SpecError::InvalidInput("degenerate calibration".into()));
    }
    let disc = (c[1] * c[1] - 4.0 * c[2] * (c[0] - nm)).sqrt();
    let f = (-c[1] + disc) / (2.0 * c[2]);
    if (MIN_DMD_COLUMN..=MAX_DMD_COLUMN).contains(&f) {
        return Ok(f);
    }
    let f = (-c[1] - disc) / (2.0 * c[2]);
    if (MIN_DMD_COLUMN..=MAX_DMD_COLUMN).contains(&f) {
        return Ok(f);
    }
    Err(SpecError::Fail(format!("{} nm is outside the DMD", nm)))
}
