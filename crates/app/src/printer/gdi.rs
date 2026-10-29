//! Printing on Windows through GDI: printers from `EnumPrintersW`, paper sizes from
//! `DeviceCapabilitiesW`, the printable area from a device context's `GetDeviceCaps`, and
//! pages drawn with `StretchDIBits` in bands of rows. The driver's own dialog
//! (`DocumentPropertiesW`) sets quality, colour and duplex, and its `DEVMODE` is kept for the
//! rest of the session.

use std::ptr::{null, null_mut};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DEVMODEW, DIB_RGB_COLORS, DeleteDC,
    GetDeviceCaps, HALFTONE, HDC, HORZRES, LOGPIXELSX, PHYSICALHEIGHT, PHYSICALOFFSETX,
    PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY, SetStretchBltMode, StretchDIBits, VERTRES,
};
use windows_sys::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW,
    PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
};
use windows_sys::Win32::Storage::Xps::{
    DC_PAPERS, DOCINFOW, DeviceCapabilitiesW, EndDoc, EndPage, StartDocW, StartPage,
};

use super::{Capabilities, Printer};
use crate::pages::Page;
use crate::print::Paper;

pub const HAS_PROPERTIES: bool = true;

/// Printing is always there on Windows.
pub fn unavailable() -> Option<String> {
    None
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the system gives NUL-terminated strings.
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

fn last_error(what: &str) -> String {
    // SAFETY: reads the thread's last error.
    format!("{what} failed (Windows error {})", unsafe {
        GetLastError()
    })
}

pub fn list() -> Vec<Printer> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0u32, 0u32);
    // SAFETY: the first call only asks for the size; the second fills a buffer of that size.
    let names = unsafe {
        EnumPrintersW(flags, null(), 4, null_mut(), 0, &mut needed, &mut count);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        if EnumPrintersW(
            flags,
            null(),
            4,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
            &mut count,
        ) == 0
        {
            return Vec::new();
        }
        let infos = buf.as_ptr().cast::<PRINTER_INFO_4W>();
        (0..count as usize)
            .map(|i| from_wide((*infos.add(i)).pPrinterName))
            .collect::<Vec<_>>()
    };
    let default = default_printer();
    names
        .into_iter()
        .map(|name| Printer {
            is_default: name == default,
            name,
        })
        .collect()
}

fn default_printer() -> String {
    let mut len = 0u32;
    // SAFETY: the first call reports the length, the second fills a buffer of it.
    unsafe {
        GetDefaultPrinterW(null_mut(), &mut len);
        if len == 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) == 0 {
            return String::new();
        }
        from_wide(buf.as_ptr())
    }
}

/// `DMPAPER_*`.
fn paper_id(p: Paper) -> i16 {
    match p {
        Paper::Letter => 1,
        Paper::Legal => 5,
        Paper::Tabloid => 3,
        Paper::A3 => 8,
        Paper::A4 => 9,
        Paper::A5 => 11,
    }
}

pub fn capabilities(printer: &str) -> Capabilities {
    let name = wide(printer);
    let mut caps = Capabilities::default();
    // SAFETY: DC_PAPERS fills an array of WORDs, as many as the call returned a count of.
    unsafe {
        let n = DeviceCapabilitiesW(name.as_ptr(), null(), DC_PAPERS, null_mut(), null());
        if n > 0 {
            let mut ids = vec![0u16; n as usize];
            let got = DeviceCapabilitiesW(
                name.as_ptr(),
                null(),
                DC_PAPERS,
                ids.as_mut_ptr().cast(),
                null(),
            );
            ids.truncate(got.max(0) as usize);
            caps.papers = Paper::ALL
                .into_iter()
                .filter(|p| ids.contains(&(paper_id(*p) as u16)))
                .collect();
        }
        let dc = CreateDCW(null(), name.as_ptr(), null(), null());
        if !dc.is_null() {
            let dpi = GetDeviceCaps(dc, LOGPIXELSX as i32);
            let (pw, ph) = (
                GetDeviceCaps(dc, PHYSICALWIDTH as i32),
                GetDeviceCaps(dc, PHYSICALHEIGHT as i32),
            );
            let (ox, oy) = (
                GetDeviceCaps(dc, PHYSICALOFFSETX as i32),
                GetDeviceCaps(dc, PHYSICALOFFSETY as i32),
            );
            let (hr, vr) = (
                GetDeviceCaps(dc, HORZRES as i32),
                GetDeviceCaps(dc, VERTRES as i32),
            );
            if dpi > 0 {
                caps.dpi = Some(dpi as u32);
                let px = [ox, oy, pw - hr - ox, ph - vr - oy]
                    .into_iter()
                    .max()
                    .unwrap_or(0);
                caps.margin_pt = Some(f64::from(px.max(0)) * 72.0 / f64::from(dpi));
            }
            DeleteDC(dc);
        }
    }
    caps
}

/// A `DEVMODE` and its size, in 8-byte words so that it is aligned.
#[derive(Clone)]
struct Devmode(Vec<u64>);

impl Devmode {
    fn ptr(&mut self) -> *mut DEVMODEW {
        self.0.as_mut_ptr().cast()
    }
}

/// The `DEVMODE`s the driver dialog gave, by printer.
static KEPT: Mutex<Vec<(String, Devmode)>> = Mutex::new(Vec::new());

fn kept(printer: &str) -> Option<Devmode> {
    let all = KEPT.lock().ok()?;
    all.iter()
        .find(|(n, _)| n == printer)
        .map(|(_, d)| d.clone())
}

struct Printer_(PRINTER_HANDLE);

impl Printer_ {
    fn open(name: &[u16]) -> Result<Self, String> {
        let mut h = PRINTER_HANDLE { Value: null_mut() };
        // SAFETY: `name` is NUL-terminated; `h` receives the handle.
        if unsafe { OpenPrinterW(name.as_ptr().cast_mut(), &mut h, null()) } == 0 {
            return Err(last_error("OpenPrinter"));
        }
        Ok(Self(h))
    }
}

impl Drop for Printer_ {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenPrinterW.
        unsafe { ClosePrinter(self.0) };
    }
}

const DM_IN_BUFFER: u32 = 8;
const DM_IN_PROMPT: u32 = 4;
const DM_OUT_BUFFER: u32 = 2;

/// The driver's `DEVMODE` for `printer`: the one kept, or its defaults. With `prompt`, the
/// driver's dialog opens first; `None` when it is cancelled.
fn devmode(printer: &str, prompt: bool) -> Result<Option<Devmode>, String> {
    let name = wide(printer);
    let h = Printer_::open(&name)?;
    let mut have = kept(printer);
    // SAFETY: a null output asks for the size, then the buffer is that large.
    unsafe {
        let size = DocumentPropertiesW(null_mut(), h.0, name.as_ptr(), null_mut(), null(), 0);
        if size <= 0 {
            return Err(last_error("DocumentProperties"));
        }
        let mut out = Devmode(vec![0u64; (size as usize).div_ceil(8)]);
        let mut mode = DM_OUT_BUFFER;
        if prompt {
            mode |= DM_IN_PROMPT;
        }
        let input = match have.as_mut() {
            Some(d) => {
                mode |= DM_IN_BUFFER;
                d.ptr().cast_const()
            }
            None => null(),
        };
        let r = DocumentPropertiesW(null_mut(), h.0, name.as_ptr(), out.ptr(), input, mode);
        match r {
            1 => Ok(Some(out)),
            2 => Ok(None),
            _ => Err(last_error("DocumentProperties")),
        }
    }
}

/// Opens the driver's own dialog for `printer` and keeps what it sets.
pub fn properties(printer: &str) -> Result<(), String> {
    if let Some(d) = devmode(printer, true)?
        && let Ok(mut all) = KEPT.lock()
    {
        all.retain(|(n, _)| n != printer);
        all.push((printer.to_owned(), d));
    }
    Ok(())
}

const DM_ORIENTATION: u32 = 1;
const DM_PAPERSIZE: u32 = 2;
const DM_COPIES: u32 = 0x100;

/// What to print and how.
pub struct Request {
    pub printer: String,
    pub paper: Paper,
    pub landscape: bool,
    pub copies: u32,
    pub title: String,
}

/// A job being sent: a document on a printer's device context.
pub struct Spool {
    dc: HDC,
    printer: String,
    open_page: bool,
    pages: usize,
}

// SAFETY: the context is made, used and closed on one worker thread; this only moves it there.
unsafe impl Send for Spool {}

impl Spool {
    pub fn open(r: &Request) -> Result<Self, String> {
        let mut dm = devmode(&r.printer, false)?.ok_or("no printer settings")?;
        let name = wide(&r.printer);
        let title = wide(&r.title);
        // SAFETY: the DEVMODE is the driver's own, large enough for the fields set.
        unsafe {
            let m = &mut *dm.ptr();
            m.dmFields |= DM_ORIENTATION | DM_PAPERSIZE | DM_COPIES;
            m.Anonymous1.Anonymous1.dmPaperSize = paper_id(r.paper);
            m.Anonymous1.Anonymous1.dmOrientation = if r.landscape { 2 } else { 1 };
            m.Anonymous1.Anonymous1.dmCopies = r.copies.clamp(1, 99) as i16;
            let dc = CreateDCW(null(), name.as_ptr(), null(), dm.ptr());
            if dc.is_null() {
                return Err(last_error("CreateDC"));
            }
            let info = DOCINFOW {
                cbSize: size_of::<DOCINFOW>() as i32,
                lpszDocName: title.as_ptr(),
                lpszOutput: null(),
                lpszDatatype: null(),
                fwType: 0,
            };
            if StartDocW(dc, &info) <= 0 {
                let e = last_error("StartDoc");
                DeleteDC(dc);
                return Err(e);
            }
            Ok(Self {
                dc,
                printer: r.printer.clone(),
                open_page: false,
                pages: 0,
            })
        }
    }

    /// Draws a page on the paper: the image covers the whole sheet, and the printer's
    /// unprintable margin trims its edge.
    pub fn page(&mut self, page: &Page) -> Result<(), String> {
        let [w, h] = page.size;
        let (w, h) = (w as i32, h as i32);
        // SAFETY: `dc` is open; each band's bitmap outlives its StretchDIBits call.
        unsafe {
            if StartPage(self.dc) <= 0 {
                return Err(last_error("StartPage"));
            }
            self.open_page = true;
            let phys_w = GetDeviceCaps(self.dc, PHYSICALWIDTH as i32);
            let phys_h = GetDeviceCaps(self.dc, PHYSICALHEIGHT as i32);
            let off_x = GetDeviceCaps(self.dc, PHYSICALOFFSETX as i32);
            let off_y = GetDeviceCaps(self.dc, PHYSICALOFFSETY as i32);
            SetStretchBltMode(self.dc, HALFTONE as i32);
            let stride = (w as usize * 3).next_multiple_of(4);
            let band = 128usize;
            let mut bits = vec![0u8; stride * band];
            let mut y = 0usize;
            while y < h as usize {
                let rows = band.min(h as usize - y);
                for r in 0..rows {
                    let src = &page.rgb[(y + r) * w as usize * 3..][..w as usize * 3];
                    let dst = &mut bits[r * stride..][..w as usize * 3];
                    for (d, s) in dst.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
                        d.copy_from_slice(&[s[2], s[1], s[0]]);
                    }
                }
                let at = |row: usize| (row as i64 * i64::from(phys_h) / i64::from(h)) as i32;
                let (dy0, dy1) = (at(y), at(y + rows));
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        // Negative: the rows run top to bottom.
                        biHeight: -(rows as i32),
                        biPlanes: 1,
                        biBitCount: 24,
                        biCompression: BI_RGB,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    bmiColors: [Default::default()],
                };
                let got = StretchDIBits(
                    self.dc,
                    -off_x,
                    dy0 - off_y,
                    phys_w,
                    dy1 - dy0,
                    0,
                    0,
                    w,
                    rows as i32,
                    bits.as_ptr().cast(),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                if got == 0 {
                    return Err(last_error("StretchDIBits"));
                }
                y += rows;
            }
            self.open_page = false;
            if EndPage(self.dc) <= 0 {
                return Err(last_error("EndPage"));
            }
        }
        self.pages += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<String, String> {
        // SAFETY: `dc` is open with a document started; Drop deletes it.
        if unsafe { EndDoc(self.dc) } <= 0 {
            return Err(last_error("EndDoc"));
        }
        Ok(format!("Sent to {}.", self.printer))
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        // SAFETY: closes what `open` made; a job that was not finished is abandoned.
        unsafe {
            if self.open_page {
                EndPage(self.dc);
            }
            DeleteDC(self.dc);
        }
    }
}
