//! Scan interpretation: port of dlpspec_scan.c, dlpspec_scan_col.c,
//! dlpspec_scan_had.c and the relevant helpers in dlpspec_helper.c.

use super::*;

const MIN_PX_BETWEEN_COL_GROUPS: i32 = 2;
const MAX_HAD_SETS: usize = 14 + 2; // MAX_COL_GROUP_WIDTH + MIN_PX_BETWEEN_COL_GROUPS

/// Per-section view of scan data (the C code builds a temporary scanData).
#[derive(Clone)]
struct SectionData {
    head: ScanDataHeader,
    cfg: ScanConfig,
    adc: Vec<i32>,
}

fn is_black(i: i32, first: u8, period: u8) -> bool {
    period != 0 && (i - first as i32) % period as i32 == 0
}

/// dlpspec_copy_scanData_hdr_to_scanResults
fn results_with_header(head: &ScanDataHeader, cfg: &ScanDataConfig) -> ScanResults {
    let mut r = ScanResults::empty();
    r.head = head.clone();
    r.cfg = match cfg {
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
    };
    r
}

/// dlpspec_subtract_remove_dc_level: removes black-pattern samples and the DC level.
fn subtract_remove_dc_level(head: &ScanDataHeader, adc: &[i32], r: &mut ScanResults) {
    let n = (head.adc_data_length as usize).min(ADC_DATA_LEN);
    let (first, period) = (head.black_pattern_first, head.black_pattern_period);
    let mut dc: i32 = 0;
    let mut nb: i32 = 0;
    for i in 0..n {
        if is_black(i as i32, first, period) {
            dc = dc.wrapping_add(adc[i]);
            nb += 1;
        }
    }
    if nb > 0 {
        dc /= nb;
    }
    let mut res = 0usize;
    for (i, &v) in adc.iter().enumerate().take(n) {
        if !is_black(i as i32, first, period) {
            r.intensity[res] = v.wrapping_sub(dc);
            res += 1;
        }
    }
    r.length = res;
}

/// dlpspec_subtract_dc_level (slew variant, in place; black samples set to 0).
fn subtract_dc_level(head: &ScanDataHeader, adc: &mut [i32]) {
    let n = (head.adc_data_length as usize).min(ADC_DATA_LEN);
    let (first, period) = (head.black_pattern_first, head.black_pattern_period);
    let mut dc: i32 = 0;
    let mut nb: i32 = 0;
    for i in 0..n {
        if is_black(i as i32, first, period) {
            dc = dc.wrapping_add(adc[i]);
            nb += 1;
        }
    }
    if nb > 0 {
        dc /= nb;
    }
    for i in 0..n {
        if is_black(i as i32, first, period) {
            adc[i] = 0;
        } else {
            adc[i] = adc[i].wrapping_sub(dc);
        }
    }
}

pub struct PatDefCol {
    pub num_patterns: u16,
    pub col_width: u16,
    pub col_mid_pix: Vec<u16>,
}

/// dlpspec_scan_col_genPatDef
pub fn col_gen_pat_def(cfg: &ScanConfig, coeffs: &CalibCoeffs) -> SpecResult<PatDefCol> {
    let c = &coeffs.pixel_to_wavelength_coeffs;
    let start_px = nm_to_column(cfg.wavelength_start_nm as f64, c)
        .map_err(|_| SpecError::InvalidInput("start wavelength".into()))?;
    let end_px = nm_to_column(cfg.wavelength_end_nm as f64, c)
        .map_err(|_| SpecError::InvalidInput("end wavelength".into()))?;
    let np = cfg.num_patterns as i32;
    let step = if np - 1 > 0 { (end_px - start_px) / (np - 1) as f64 } else { 0.0 };
    let mut mid = Vec::with_capacity(np.max(0) as usize);
    let mut x = start_px;
    for _ in 0..np.min(MAX_PATTERNS_PER_SCAN as i32) {
        mid.push(x as u16); // C stores into uint16_t (truncation)
        x += step;
    }
    Ok(PatDefCol { num_patterns: cfg.num_patterns, col_width: cfg.width_px as u16, col_mid_pix: mid })
}

pub struct HadSet {
    pub col_group_num: Vec<u16>,
    pub col_mid_pix: Vec<u16>,
    pub had_order: u16,
}

pub struct PatDefHad {
    pub num_patterns: u32,
    pub col_width: u16,
    pub sets: Vec<HadSet>,
}

fn check_prime(n: i32) -> bool {
    let mut i = 2;
    while i <= n / 2 {
        if n % i == 0 {
            return false;
        }
        i += 1;
    }
    true
}

/// getPaleyOrder: smallest prime p = 3 (mod 4) with p >= n
pub fn paley_order(n: i32) -> i32 {
    let mut n = n + 1;
    if n % 4 != 0 {
        n += 4 - (n % 4);
    }
    while !check_prime(n - 1) {
        n += 4;
    }
    n - 1
}

/// dlpspec_scan_had_genPatDef
pub fn had_gen_pat_def(cfg: &ScanConfig, coeffs: &CalibCoeffs) -> SpecResult<PatDefHad> {
    let pc = col_gen_pat_def(cfg, coeffs)?;
    let mid = |k: usize| -> i32 { pc.col_mid_pix.get(k).copied().unwrap_or(0) as i32 };
    let w = pc.col_width as i32;
    let np = cfg.num_patterns as i32;
    let mut num_sets = 0usize;
    for i in 1..MAX_HAD_SETS {
        let mut j = 0i32;
        while j < np - i as i32 - 1 {
            let a = mid(j as usize);
            let b = mid(j as usize + i);
            let px_between = if a < b {
                let this_edge = a - w / 2 + w - 1;
                let next_edge = b - w / 2;
                next_edge - this_edge - 1
            } else {
                let this_edge = a - w / 2;
                let next_edge = b - w / 2 + w - 1;
                this_edge - next_edge - 1
            };
            if px_between < MIN_PX_BETWEEN_COL_GROUPS {
                num_sets = 0;
                break;
            } else {
                num_sets = i;
            }
            j += 1;
        }
        if num_sets != 0 {
            break;
        }
    }
    if num_sets == 0 {
        return Err(SpecError::Fail("no valid Hadamard set arrangement".into()));
    }
    let mut sets = Vec::with_capacity(num_sets);
    let mut total = 0u32;
    for i in 0..num_sets {
        let mut groups = Vec::new();
        let mut mids = Vec::new();
        let mut k = i;
        while (k as i32) < np {
            groups.push(k as u16);
            mids.push(mid(k) as u16);
            k += num_sets;
        }
        let order = paley_order(groups.len() as i32) as u16;
        total += order as u32;
        sets.push(HadSet { col_group_num: groups, col_mid_pix: mids, had_order: order });
    }
    Ok(PatDefHad { num_patterns: total, col_width: pc.col_width, sets })
}

fn mid_px(mid: u16, width_px: u8) -> f64 {
    if width_px % 2 != 0 {
        mid as f64
    } else {
        mid as f64 - 0.5
    }
}

/// dlpspec_scan_col_interpret
fn col_interpret(sd: &SectionData) -> SpecResult<ScanResults> {
    let mut r = results_with_header(&sd.head, &ScanDataConfig::Single(sd.cfg.clone()));
    subtract_remove_dc_level(&sd.head, &sd.adc, &mut r);
    let pd = col_gen_pat_def(&sd.cfg, &sd.head.calibration_coeffs)?;
    let c = &sd.head.calibration_coeffs.pixel_to_wavelength_coeffs;
    for i in 0..r.length {
        let m = pd.col_mid_pix.get(i).copied().unwrap_or(0);
        r.wavelength[i] = column_to_nm(mid_px(m, sd.cfg.width_px), c);
    }
    r.head.pga = sd.head.pga;
    Ok(r)
}

/// Unpacks the S-matrix (LSB first, row major).
fn s_matrix(order: u16) -> SpecResult<Vec<f64>> {
    if order > had_tables::MAX_ORDER_AVAIL {
        return Err(SpecError::InvalidInput(format!("Hadamard order {} too large", order)));
    }
    let packed = had_tables::packed_s_matrix(order)
        .ok_or_else(|| SpecError::InvalidInput(format!("no S-matrix of order {}", order)))?;
    let n = order as usize * order as usize;
    let mut m = vec![0.0; n];
    for (idx, v) in m.iter_mut().enumerate() {
        *v = ((packed[idx / 8] >> (idx % 8)) & 1) as f64;
    }
    Ok(m)
}

/// dlpspec_scan_had_interpret
fn had_interpret(sd: &SectionData) -> SpecResult<ScanResults> {
    let mut r = results_with_header(&sd.head, &ScanDataConfig::Single(sd.cfg.clone()));
    subtract_remove_dc_level(&sd.head, &sd.adc, &mut r);
    let mut adc_adjusted = vec![0.0f64; ADC_DATA_LEN];
    for i in 0..r.length {
        adc_adjusted[i] = r.intensity[i] as f64;
    }
    let pd = had_gen_pat_def(&sd.cfg, &sd.head.calibration_coeffs)?;
    let c = sd.head.calibration_coeffs.pixel_to_wavelength_coeffs;
    let mut pos = 0usize;
    let mut total = 0usize;
    for set in &pd.sets {
        let n = set.had_order as usize;
        let s = s_matrix(set.had_order)?;
        let div = ((set.had_order as i32 + 1) / 4) as f64;
        let inv: Vec<f64> = s.iter().map(|v| (v - 0.5) / div).collect();
        let adc_buff: Vec<f64> = (0..n).map(|j| adc_adjusted.get(j + pos).copied().unwrap_or(0.0)).collect();
        for (j, &group) in set.col_group_num.iter().enumerate() {
            let mut sum = 0.0;
            for k in 0..n {
                sum += adc_buff[k] * inv[k * n + j];
            }
            let g = group as usize;
            if g < ADC_DATA_LEN {
                r.intensity[g] = sum as i32;
                r.wavelength[g] = column_to_nm(mid_px(set.col_mid_pix[j], sd.cfg.width_px), &c);
            }
        }
        total += set.col_group_num.len();
        pos += n;
    }
    r.length = total.min(ADC_DATA_LEN);
    Ok(r)
}

/// dlpspec_scan_section_get_adc_data_range -> (start_index, num_patterns, num_black_patterns)
fn section_adc_range(head: &ScanDataHeader, cfg: &SlewScanConfig, section_index: usize) -> SpecResult<(usize, u16, u16)> {
    let period = head.black_pattern_period as usize;
    let mut start = 0usize;
    for i in 0..=section_index {
        let s = cfg.section(i);
        let np: u16 = match s.section_scan_type {
            COLUMN_TYPE => s.num_patterns,
            HADAMARD_TYPE => {
                let sc = ScanConfig {
                    scan_type: HADAMARD_TYPE,
                    wavelength_start_nm: s.wavelength_start_nm,
                    wavelength_end_nm: s.wavelength_end_nm,
                    width_px: s.width_px,
                    num_patterns: s.num_patterns,
                    num_repeats: cfg.num_repeats,
                    ..Default::default()
                };
                had_gen_pat_def(&sc, &head.calibration_coeffs)?.num_patterns as u16
            }
            t => return Err(SpecError::IllegalScanType(t)),
        };
        let mut nb: u16 = 0;
        let mut j = start;
        while period != 0 && j < start + np as usize + nb as usize {
            if (j + 1) % period == 0 {
                nb += 1;
            }
            j += 1;
        }
        if i == section_index {
            return Ok((start, np, nb));
        }
        start += np as usize + nb as usize;
    }
    unreachable!()
}

/// dlpspec_get_scanData_from_slewScanData
fn section_from_slew(head: &ScanDataHeader, cfg: &SlewScanConfig, adc: &[i32], idx: usize) -> SpecResult<SectionData> {
    if idx >= cfg.num_sections as usize {
        return Err(SpecError::InvalidInput("section index".into()));
    }
    let (start, np, nb) = section_adc_range(head, cfg, idx)?;
    let len = np as usize + nb as usize;
    let s = cfg.section(idx);
    let mut h = head.clone();
    h.adc_data_length = len as u16;
    if h.black_pattern_period != 0 {
        h.black_pattern_first = h.black_pattern_first.wrapping_sub((start % h.black_pattern_period as usize) as u8);
    }
    let mut sadc = vec![0i32; ADC_DATA_LEN];
    for k in 0..len {
        if start + k < ADC_DATA_LEN {
            sadc[k] = adc[start + k];
        }
    }
    Ok(SectionData {
        head: h,
        cfg: ScanConfig {
            scan_type: s.section_scan_type,
            scan_config_index: cfg.scan_config_index,
            serial_number: cfg.serial_number.clone(),
            config_name: cfg.config_name.clone(),
            wavelength_start_nm: s.wavelength_start_nm,
            wavelength_end_nm: s.wavelength_end_nm,
            width_px: s.width_px,
            num_patterns: s.num_patterns,
            num_repeats: cfg.num_repeats,
        },
        adc: sadc,
    })
}

fn slew_interpret(data: &ScanData, cfg: &SlewScanConfig) -> SpecResult<ScanResults> {
    let mut adc = data.adc_data.clone();
    subtract_dc_level(&data.head, &mut adc);
    let mut out: Option<ScanResults> = None;
    let mut num_data = 0usize;
    for i in 0..cfg.num_sections as usize {
        let sd = section_from_slew(&data.head, cfg, &adc, i)?;
        let tmp = match cfg.section(i).section_scan_type {
            COLUMN_TYPE => col_interpret(&sd)?,
            HADAMARD_TYPE => had_interpret(&sd)?,
            t => return Err(SpecError::IllegalScanType(t)),
        };
        let np = tmp.cfg.section(0).num_patterns as usize;
        match out.as_mut() {
            None => {
                let mut r = tmp.clone();
                let hdr = results_with_header(&data.head, &data.cfg);
                r.head = hdr.head;
                r.cfg = hdr.cfg;
                num_data = np;
                out = Some(r);
            }
            Some(r) => {
                for k in 0..np {
                    if num_data + k < ADC_DATA_LEN {
                        r.intensity[num_data + k] = tmp.intensity[k];
                        r.wavelength[num_data + k] = tmp.wavelength[k];
                    }
                }
                num_data += np;
                r.length = (r.length + tmp.length).min(ADC_DATA_LEN);
            }
        }
    }
    out.ok_or_else(|| SpecError::InvalidInput("scan has no sections".into()))
}

/// dlpspec_scan_interpret on an already-deserialized scan.
pub fn interpret_data(data: &ScanData) -> SpecResult<ScanResults> {
    if data.head.header_version != CUR_SCANDATA_VERSION {
        return Err(SpecError::Fail(format!("unsupported scan data version {}", data.head.header_version)));
    }
    match &data.cfg {
        ScanDataConfig::Slew(c) if c.scan_type == SLEW_TYPE => slew_interpret(data, c),
        ScanDataConfig::Slew(c) => Err(SpecError::IllegalScanType(c.scan_type)),
        ScanDataConfig::Single(c) => {
            let sd = SectionData { head: data.head.clone(), cfg: c.clone(), adc: data.adc_data.clone() };
            match c.scan_type {
                COLUMN_TYPE => col_interpret(&sd),
                HADAMARD_TYPE => had_interpret(&sd),
                t => Err(SpecError::IllegalScanType(t)),
            }
        }
    }
}

/// dlpspec_scan_interpret
pub fn interpret(blob: &[u8]) -> SpecResult<ScanResults> {
    interpret_data(&ScanData::from_blob(blob)?)
}

fn scale_for_pga_gain(scan: &ScanResults, refr: &mut ScanResults) -> SpecResult<()> {
    if scan.head.pga != refr.head.pga {
        if refr.head.pga == 0 {
            return Err(SpecError::InvalidInput("reference PGA is 0".into()));
        }
        for j in 0..ADC_DATA_LEN {
            if refr.wavelength[j] != 0.0 {
                refr.intensity[j] = refr.intensity[j].wrapping_mul(scan.head.pga as i32) / refr.head.pga as i32;
            } else {
                break;
            }
        }
    }
    Ok(())
}

fn cfg_compare(a: &SlewScanConfig, b: &SlewScanConfig) -> bool {
    if a.num_sections != b.num_sections {
        return false;
    }
    (0..a.num_sections as usize).all(|i| {
        let (x, y) = (a.section(i), b.section(i));
        x.section_scan_type == y.section_scan_type
            && x.width_px == y.width_px
            && x.wavelength_start_nm == y.wavelength_start_nm
            && x.wavelength_end_nm == y.wavelength_end_nm
            && x.num_patterns == y.num_patterns
    })
}

fn valid_configs_to_interp(scan: &SlewScanConfig, refc: &SlewScanConfig) -> SpecResult<()> {
    if refc.num_sections != 1 {
        return Err(SpecError::Fail("reference must have exactly one section".into()));
    }
    let r = refc.section(0);
    for i in 0..scan.num_sections as usize {
        let s = scan.section(i);
        if s.wavelength_start_nm < r.wavelength_start_nm || s.wavelength_end_nm > r.wavelength_end_nm {
            return Err(SpecError::Fail("scan range is outside the reference range".into()));
        }
    }
    Ok(())
}

/// dlpspec_compute_from_references (integer linear inter/extrapolation)
fn compute_from_references(in1: i32, in2: i32, out1: i32, out2: i32, x: i32) -> SpecResult<i32> {
    if in2 - in1 == 0 {
        return Err(SpecError::InvalidInput("degenerate reference widths".into()));
    }
    Ok(if x < in1 && x < in2 {
        out1 - (in1 - x) * (out2 - out1) / (in2 - in1)
    } else if x > in1 && x > in2 {
        out2 + (x - in2) * (out2 - out1) / (in2 - in1)
    } else {
        out2 - (in2 - x) * (out2 - out1) / (in2 - in1)
    })
}

/// dlpspec_interpolate_int_wavelengths
fn interpolate_int_wavelengths(
    desired: &[f64],
    num_desired: usize,
    ref_nm: &mut [f64],
    ref_int: &mut [i32],
    num_ref: usize,
) -> SpecResult<()> {
    if num_desired == 0 || num_desired > ADC_DATA_LEN || num_ref == 0 || num_ref > ADC_DATA_LEN {
        return Err(SpecError::InvalidInput("interpolation lengths".into()));
    }
    let mut out = vec![0i32; num_desired];
    let mut prev_j = 0usize;
    for i in 0..num_desired {
        let d = desired[i];
        if d == 0.0 {
            return Err(SpecError::InvalidInput("zero wavelength".into()));
        }
        let mut entry_found = false;
        let mut value_found = false;
        let mut j = prev_j;
        while j < num_ref {
            if ref_nm[j] == 0.0 {
                break;
            }
            if ref_nm[j] < d {
                j += 1;
                continue;
            } else if ref_nm[j] == d {
                out[i] = ref_int[j];
                value_found = true;
            } else {
                entry_found = true;
            }
            break;
        }
        if !value_found {
            if entry_found {
                if j == 0 {
                    out[i] = if ref_nm[1] - ref_nm[0] != 0.0 {
                        ref_int[0]
                            - ((ref_nm[0] - d) * (ref_int[1] - ref_int[0]) as f64 / (ref_nm[1] - ref_nm[0])) as i32
                    } else {
                        ref_int[0]
                    };
                } else {
                    out[i] = if ref_nm[j] - ref_nm[j - 1] != 0.0 {
                        ref_int[j]
                            - ((ref_nm[j] - d) / (ref_nm[j] - ref_nm[j - 1]) * (ref_int[j] - ref_int[j - 1]) as f64)
                                as i32
                    } else {
                        ref_int[j]
                    };
                }
            } else if j >= 2 {
                out[i] = if ref_nm[j - 1] - ref_nm[j - 2] != 0.0 {
                    ref_int[j - 1]
                        + ((d - ref_nm[j - 1]) * (ref_int[j - 1] - ref_int[j - 2]) as f64
                            / (ref_nm[j - 1] - ref_nm[j - 2])) as i32
                } else {
                    ref_int[j - 1]
                };
            } else {
                out[i] = ref_int[j.saturating_sub(1)];
            }
        }
        prev_j = j;
    }
    for i in 0..num_desired {
        ref_int[i] = out[i].max(0);
        ref_nm[i] = desired[i];
    }
    Ok(())
}

/// dlpspec_interpolate_double_wavelengths (reference arrays hold REF_CAL_INTERP_WAVELENGTH entries)
fn interpolate_double_wavelengths(
    desired: &[f64],
    ref_nm: &mut [f64],
    ref_int: &mut [f64],
    num_entries: usize,
) -> SpecResult<()> {
    if num_entries == 0 {
        return Err(SpecError::InvalidInput("interpolation length".into()));
    }
    let mut out = vec![0f64; num_entries];
    let mut prev_j = 0usize;
    for i in 0..num_entries {
        let d = desired.get(i).copied().unwrap_or(0.0);
        if d == 0.0 {
            return Err(SpecError::InvalidInput("zero wavelength".into()));
        }
        let mut entry_found = false;
        let mut j = prev_j;
        while j < REF_CAL_INTERP_WAVELENGTH {
            if ref_nm[j] == 0.0 {
                break;
            }
            if ref_nm[j] < d {
                j += 1;
                continue;
            } else if ref_nm[j] == d {
                out[i] = ref_int[j];
            } else {
                entry_found = true;
            }
            break;
        }
        if entry_found {
            if j == 0 {
                out[i] = if ref_nm[1] - ref_nm[0] != 0.0 {
                    ref_int[0] - ((ref_nm[0] - d) * (ref_int[1] - ref_int[0]) / (ref_nm[1] - ref_nm[0]))
                } else {
                    ref_int[0]
                };
            } else {
                // NB: the C code tests the intensity difference here, kept as-is.
                out[i] = if ref_int[j] - ref_int[j - 1] != 0.0 {
                    ref_int[j] - ((ref_nm[j] - d) / (ref_nm[j] - ref_nm[j - 1]) * (ref_int[j] - ref_int[j - 1]))
                } else {
                    ref_int[j]
                };
            }
        } else if j >= 2 {
            out[i] = if ref_nm[j - 1] - ref_nm[j - 2] != 0.0 {
                ref_int[j - 1] + ((d - ref_nm[j - 1]) * (ref_int[j - 1] - ref_int[j - 2]) / (ref_nm[j - 1] - ref_nm[j - 2]))
            } else {
                ref_int[j - 1]
            };
        } else {
            out[i] = ref_int[j.saturating_sub(1)];
        }
        prev_j = j;
    }
    for i in 0..num_entries {
        ref_int[i] = if out[i] > 0.0 { out[i] } else { 0.0 };
        ref_nm[i] = desired.get(i).copied().unwrap_or(0.0);
    }
    Ok(())
}

/// dlpspec_scan_recomputeRefIntensities
fn recompute_ref_intensities(scan: &ScanResults, refr: &mut ScanResults, m: &RefCalMatrix) -> SpecResult<()> {
    let mut factor = vec![0f64; ADC_DATA_LEN];
    let mut wavelengths = vec![0f64; ADC_DATA_LEN];
    let mut adc_idx = 0usize;
    let (mut lb, mut ub) = (0usize, 0usize);
    let ref_w = refr.cfg.section(0).width_px as i32;
    let w = |i: usize| m.width[i.min(REF_CAL_INTERP_WIDTH - 1)] as i32;
    let lk = |i: usize, j: usize| m.ref_lookup[i.min(REF_CAL_INTERP_WIDTH - 1)][j] as i32;
    for s_idx in 0..scan.cfg.num_sections as usize {
        for j in 0..REF_CAL_INTERP_WAVELENGTH {
            factor[j] = 1.0;
            wavelengths[j] = m.wavelength[j];
        }
        let sec = scan.cfg.section(s_idx);
        let section_len = sec.num_patterns as usize;
        let same = refr.cfg.num_sections == 1 && scan.cfg.num_sections == 1 && sec.width_px == refr.cfg.section(0).width_px;
        if same {
            continue;
        }
        let mut i = 0usize;
        while i < REF_CAL_INTERP_WIDTH {
            let mw = m.width[i] as i32;
            if mw < ref_w {
                i += 1;
                continue;
            } else if mw == ref_w {
                lb = i;
                ub = i;
            } else {
                lb = i.saturating_sub(1);
                ub = i;
            }
            break;
        }
        let i = i.min(REF_CAL_INTERP_WIDTH - 1);
        let scan_w = sec.width_px as i32;
        for j in 0..REF_CAL_INTERP_WAVELENGTH {
            let temp = if i == 0 {
                compute_from_references(w(0), w(1), lk(0, j), lk(1, j), scan_w)?
            } else if i == REF_CAL_INTERP_WIDTH - 1 {
                compute_from_references(w(i - 1), w(i), lk(i - 1, j), lk(i, j), scan_w)?
            } else {
                compute_from_references(w(i), w(i - 1), lk(i, j), lk(i - 1, j), scan_w)?
            };
            let ref_val = if lb == ub {
                lk(lb, j)
            } else {
                compute_from_references(w(lb), w(ub), lk(lb, j), lk(ub, j), ref_w)?
            };
            factor[j] = factor[j] * temp as f64 / ref_val as f64;
        }
        let desired: Vec<f64> = refr.wavelength[adc_idx.min(ADC_DATA_LEN)..].to_vec();
        interpolate_double_wavelengths(&desired, &mut wavelengths, &mut factor, section_len)?;
        for k in 0..section_len {
            if adc_idx >= ADC_DATA_LEN {
                break;
            }
            if refr.wavelength[adc_idx] != 0.0 {
                refr.intensity[adc_idx] = (refr.intensity[adc_idx] as f64 * factor[k]) as i32;
            } else {
                break;
            }
            adc_idx += 1;
        }
    }
    scale_for_pga_gain(scan, refr)
}

/// dlpspec_scan_interpReference: builds reference intensities matching `scan`
/// from the factory reference-calibration scan blob and the ref-cal matrix blob.
pub fn interp_reference(ref_cal_blob: &[u8], matrix_blob: &[u8], scan: &ScanResults) -> SpecResult<ScanResults> {
    let matrix = RefCalMatrix::from_blob(matrix_blob)?;
    let mut refr = interpret(ref_cal_blob)?;
    if cfg_compare(&scan.cfg, &refr.cfg) {
        scale_for_pga_gain(scan, &mut refr)?;
        return Ok(refr);
    }
    valid_configs_to_interp(&scan.cfg, &refr.cfg)?;
    if scan.wavelength[..scan.length].iter().any(|&w| w == 0.0) {
        return Err(SpecError::InvalidInput("scan has zero wavelengths".into()));
    }
    if (0..refr.length).any(|i| refr.wavelength[i] == 0.0 || refr.intensity[i] == 0) {
        return Err(SpecError::InvalidInput("reference has zero entries".into()));
    }
    let ref_len = refr.length;
    {
        let ScanResults { wavelength, intensity, .. } = &mut refr;
        interpolate_int_wavelengths(&scan.wavelength, scan.length, wavelength, intensity, ref_len)?;
    }
    refr.length = scan.length;
    recompute_ref_intensities(scan, &mut refr, &matrix)?;
    Ok(refr)
}

/// Absorbance = -log10(sample / reference), reflectance = sample / reference.
pub fn absorbance(s: i32, r: i32) -> f64 {
    -((s as f64) / (r as f64)).log10()
}
pub fn reflectance(s: i32, r: i32) -> f64 {
    if r == 0 {
        0.0
    } else {
        s as f64 / r as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paley_orders() {
        assert_eq!(paley_order(1), 3);
        assert_eq!(paley_order(5), 7);
        assert_eq!(paley_order(8), 11);
        assert_eq!(paley_order(12), 19);
        assert_eq!(paley_order(100), 103);
    }

    #[test]
    fn s_matrices_are_cyclic_s_matrices() {
        // Every S-matrix must satisfy S * S^T = (n+1)/4 * (I + J)
        for &n in &[3u16, 7, 11, 19, 23, 31, 43, 103, 251] {
            let s = s_matrix(n).unwrap();
            let n = n as usize;
            let q = ((n + 1) / 4) as f64;
            for a in 0..n {
                for b in 0..n {
                    let dot: f64 = (0..n).map(|k| s[a * n + k] * s[b * n + k]).sum();
                    let expected = if a == b { 2.0 * q } else { q };
                    assert_eq!(dot, expected, "order {} rows {} {}", n, a, b);
                }
            }
        }
    }

    #[test]
    fn config_roundtrip() {
        let cfg = SlewScanConfig {
            scan_type: SLEW_TYPE,
            scan_config_index: 7,
            serial_number: "1234567".into(),
            config_name: "Column 1".into(),
            num_repeats: 6,
            num_sections: 2,
            sections: vec![
                SlewSection { section_scan_type: 0, width_px: 6, wavelength_start_nm: 900, wavelength_end_nm: 1300, num_patterns: 100, exposure_time: 0 },
                SlewSection { section_scan_type: 1, width_px: 8, wavelength_start_nm: 1300, wavelength_end_nm: 1700, num_patterns: 80, exposure_time: 1 },
            ],
        };
        let blob = cfg.to_blob();
        assert_eq!(blob.len(), 82 + 73);
        let back = SlewScanConfig::from_blob(&blob).unwrap();
        assert_eq!(back.config_name, "Column 1");
        assert_eq!(back.num_sections, 2);
        assert_eq!(back.sections[1], cfg.sections[1]);
        assert_eq!(back.sections.len(), 5);
    }

    /// Builds a synthetic slewScanData blob (column + Hadamard sections with
    /// black patterns) and checks that interpretation recovers the encoded spectrum.
    #[test]
    fn synthetic_slew_scan_roundtrip() {
        use crate::dlpspec::tpl::TplWriter;
        let coeffs = CalibCoeffs {
            shift_vector_coeffs: [0.0; 3],
            pixel_to_wavelength_coeffs: [1720.0, -0.95, -0.00002],
        };
        let cfg = SlewScanConfig {
            scan_type: SLEW_TYPE,
            scan_config_index: 1,
            serial_number: "T0001".into(),
            config_name: "synthetic".into(),
            num_repeats: 1,
            num_sections: 2,
            sections: vec![
                SlewSection { section_scan_type: COLUMN_TYPE, width_px: 6, wavelength_start_nm: 900, wavelength_end_nm: 1300, num_patterns: 50, exposure_time: 0 },
                SlewSection { section_scan_type: HADAMARD_TYPE, width_px: 7, wavelength_start_nm: 1300, wavelength_end_nm: 1700, num_patterns: 40, exposure_time: 0 },
            ],
        };
        let period: usize = 24;
        let dc = 1000;
        // column section signals
        let col_sig: Vec<i32> = (0..50).map(|p| 5000 + p * 10).collect();
        // hadamard section: encode x_j per column group with the S-matrix
        let sc = ScanConfig { scan_type: HADAMARD_TYPE, wavelength_start_nm: 1300, wavelength_end_nm: 1700, width_px: 7, num_patterns: 40, num_repeats: 1, ..Default::default() };
        let pd = had_gen_pat_def(&sc, &coeffs).unwrap();
        let x = |g: u16| 3000 + 7 * g as i32;
        let mut had_sig = Vec::new();
        for set in &pd.sets {
            let n = set.had_order as usize;
            let s = s_matrix(set.had_order).unwrap();
            for k in 0..n {
                let mut v = 0;
                for (j, &g) in set.col_group_num.iter().enumerate() {
                    if s[k * n + j] == 1.0 {
                        v += x(g);
                    }
                }
                had_sig.push(v);
            }
        }
        // interleave black patterns: position j is black when (j+1) % period == 0
        let mut adc = Vec::new();
        for sig in col_sig.iter().chain(had_sig.iter()) {
            while (adc.len() + 1) % period == 0 {
                adc.push(dc);
            }
            adc.push(sig + dc);
        }
        assert!(adc.len() <= ADC_DATA_LEN);
        // serialize: data head | cfg (head+sections) | adc
        let mut h = TplWriter::new(&fmt_slew_data_head(), &[SCAN_NAME_LEN as u32, 3, 3, NANO_SER_NUM_LEN as u32]);
        h.u32(1).bytes(&to_fixed::<SCAN_NAME_LEN>("synth"));
        for v in [24u8, 5, 6, 3, 10, 11, 12] {
            h.u8(v);
        }
        h.i16(2500).i16(2400).u16(4000).u16(1234).u32(77);
        for v in coeffs.shift_vector_coeffs.iter().chain(coeffs.pixel_to_wavelength_coeffs.iter()) {
            h.f64(*v);
        }
        h.bytes(&to_fixed::<NANO_SER_NUM_LEN>("T0001")).u16(adc.len() as u16).u8((period - 1) as u8).u8(period as u8).u8(16);
        let mut blob = h.finish();
        blob.extend(cfg.to_blob());
        let mut a = TplWriter::new("i#", &[ADC_DATA_LEN as u32]);
        for i in 0..ADC_DATA_LEN {
            a.i32(adc.get(i).copied().unwrap_or(0));
        }
        blob.extend(a.finish());
        blob.resize(blob.len() + 150, 0); // device pads to SCAN_DATA_BLOB_SIZE

        let r = interpret(&blob).unwrap();
        assert_eq!(r.length, 90);
        assert_eq!(r.head.pga, 16);
        assert_eq!(r.cfg.config_name, "synthetic");
        for p in 0..50 {
            assert_eq!(r.intensity[p], col_sig[p], "column pattern {}", p);
        }
        for g in 0..40u16 {
            let got = r.intensity[50 + g as usize];
            assert!((got - x(g)).abs() <= 1, "hadamard group {}: got {} want {}", g, got, x(g));
        }
        // wavelengths increase across the scan and stay in range
        for i in 1..90 {
            assert!(r.wavelength[i] > 890.0 && r.wavelength[i] < 1710.0);
        }
        assert!(r.wavelength[0] < r.wavelength[49]);
        assert!(r.wavelength[50] < r.wavelength[89]);

        // reference with an identical config only rescales by PGA
        let refr = interp_reference_results(&r, &r);
        assert_eq!(refr.length, r.length);
    }

    fn interp_reference_results(scan: &ScanResults, refr: &ScanResults) -> ScanResults {
        let mut x = refr.clone();
        assert!(cfg_compare(&scan.cfg, &x.cfg));
        scale_for_pga_gain(scan, &mut x).unwrap();
        x
    }
}
