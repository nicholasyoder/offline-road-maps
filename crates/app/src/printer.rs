//! Sending pages to a printer (phase 4 step 4.10).
//!
//! Linux goes through CUPS's command-line tools (decision 11): the printers from `lpstat`,
//! the paper sizes from `lpoptions`, and the job as a PDF on `lp`'s stdin. Windows goes
//! through GDI: `EnumPrintersW`, `DeviceCapabilitiesW`, and a device context the pages are
//! drawn on with `StretchDIBits`. Elsewhere there is no printing, and the reason says so.
//!
//! Both platforms print a page of the paper's size, so what the page holds is at the
//! scale the print page showed (`print-scaling=none` on CUPS, one device pixel per image
//! pixel on GDI).

use crate::print::Paper;

/// A printer, as the user picks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub is_default: bool,
}

/// What a printer can do that the print page uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capabilities {
    /// The sizes of [`Paper`] it takes; empty when it could not be asked, which allows all.
    pub papers: Vec<Paper>,
    /// Its resolution in dots per inch, when known.
    pub dpi: Option<u32>,
    /// The unprintable margin in points, when known: the largest of its four sides.
    pub margin_pt: Option<f64>,
}

impl Capabilities {
    pub fn takes(&self, paper: Paper) -> bool {
        self.papers.is_empty() || self.papers.contains(&paper)
    }
}

/// The dpi pages are drawn at for a printer whose resolution is `native`: that, or 300 when
/// it does not say (CUPS does not), up to 600 for memory (a Letter page is 100 MiB there).
/// `OMP_PRINT_DPI` overrides it, for comparisons.
pub fn dpi_for(native: Option<u32>) -> f64 {
    if let Some(v) = std::env::var("OMP_PRINT_DPI")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| (72.0..=1200.0).contains(v))
    {
        return v;
    }
    f64::from(native.unwrap_or(DEFAULT_DPI as u32)).clamp(72.0, MAX_DPI)
}

const DEFAULT_DPI: f64 = 300.0;
const MAX_DPI: f64 = 600.0;

#[cfg(target_os = "linux")]
mod cups;
#[cfg(target_os = "linux")]
pub use cups::*;

#[cfg(windows)]
mod gdi;
#[cfg(windows)]
pub use gdi::*;

#[cfg(not(any(target_os = "linux", windows)))]
mod none;
#[cfg(not(any(target_os = "linux", windows)))]
pub use none::*;
