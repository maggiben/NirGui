//! Minimal reader/writer for the TPL binary serialization format
//! (http://troydhanson.github.io/tpl/) as used by the TI DLP Spectrum Library.
//!
//! Image layout (little endian, as produced by the Tiva firmware):
//! ```text
//!   "tpl"            3 bytes magic
//!   flags            1 byte  (bit0 = big endian, bit1 = contains strings)
//!   total_len        u32     (whole image, inclusive)
//!   format           NUL-terminated ASCII, e.g. "S(cvc#c#vc)"
//!   fxlens           one u32 per '#' in the format (fixed array lengths)
//!   data             the packed values, in format order
//! ```
//! Only the fixed-size subset used by dlpspec is supported (no A/B/s types).

use super::SpecError;

pub const MAGIC: &[u8; 3] = b"tpl";
const FL_BIGENDIAN: u8 = 1;

/// A parsed TPL image header plus a cursor over its data section.
pub struct TplImage<'a> {
    pub format: String,
    pub fxlens: Vec<u32>,
    pub total_len: usize,
    data: &'a [u8],
    pos: usize,
}

impl<'a> TplImage<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, SpecError> {
        if buf.len() < 8 || &buf[0..3] != MAGIC {
            return Err(SpecError::Tpl("missing tpl magic".into()));
        }
        let flags = buf[3];
        if flags & FL_BIGENDIAN != 0 {
            return Err(SpecError::Tpl("big-endian tpl images are not supported".into()));
        }
        let total_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
        if total_len > buf.len() || total_len < 8 {
            return Err(SpecError::Tpl(format!(
                "tpl image length {} exceeds buffer {}",
                total_len,
                buf.len()
            )));
        }
        let img = &buf[..total_len];
        let nul = img[8..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| SpecError::Tpl("unterminated format".into()))?;
        let format = String::from_utf8_lossy(&img[8..8 + nul]).to_string();
        let mut pos = 8 + nul + 1;
        let n_fx = format.matches('#').count();
        let mut fxlens = Vec::with_capacity(n_fx);
        for _ in 0..n_fx {
            if pos + 4 > img.len() {
                return Err(SpecError::Tpl("truncated fxlens".into()));
            }
            fxlens.push(u32::from_le_bytes(img[pos..pos + 4].try_into().unwrap()));
            pos += 4;
        }
        Ok(TplImage { format, fxlens, total_len, data: img, pos })
    }

    /// Read only the format string of the image at the start of `buf`.
    pub fn peek_format(buf: &[u8]) -> Option<String> {
        TplImage::parse(buf).ok().map(|i| i.format)
    }

    pub fn expect_format(&self, fmt: &str) -> Result<(), SpecError> {
        if self.format != fmt {
            return Err(SpecError::Tpl(format!(
                "unexpected tpl format '{}', expected '{}'",
                self.format, fmt
            )));
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], SpecError> {
        if self.pos + n > self.data.len() {
            return Err(SpecError::Tpl("truncated tpl data".into()));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, SpecError> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, SpecError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn i16(&mut self) -> Result<i16, SpecError> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, SpecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32, SpecError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn f64(&mut self) -> Result<f64, SpecError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn bytes<const N: usize>(&mut self) -> Result<[u8; N], SpecError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
}

/// Builds a TPL image. Values must be pushed in format order.
pub struct TplWriter {
    format: String,
    fxlens: Vec<u32>,
    data: Vec<u8>,
}

impl TplWriter {
    pub fn new(format: &str, fxlens: &[u32]) -> Self {
        debug_assert_eq!(format.matches('#').count(), fxlens.len());
        TplWriter { format: format.to_string(), fxlens: fxlens.to_vec(), data: Vec::new() }
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.data.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.data.extend_from_slice(v);
        self
    }
    pub fn finish(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(0); // little endian, no strings
        out.extend_from_slice(&[0u8; 4]); // length placeholder
        out.extend_from_slice(self.format.as_bytes());
        out.push(0);
        for f in &self.fxlens {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&self.data);
        let len = out.len() as u32;
        out[4..8].copy_from_slice(&len.to_le_bytes());
        out
    }
}
