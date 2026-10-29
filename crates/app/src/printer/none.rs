//! No printing here (macOS printing is left for the roadmap's polish): *Save as PDF* still
//! works.

use super::{Capabilities, Printer};
use crate::pages::Page;
use crate::print::Paper;

pub const HAS_PROPERTIES: bool = false;

pub fn unavailable() -> Option<String> {
    Some("Printing is not available on this system yet. Save as PDF and print that.".into())
}

pub fn list() -> Vec<Printer> {
    Vec::new()
}

pub fn capabilities(_printer: &str) -> Capabilities {
    Capabilities::default()
}

pub struct Request {
    pub printer: String,
    pub paper: Paper,
    pub landscape: bool,
    pub copies: u32,
    pub title: String,
}

pub struct Spool;

impl Spool {
    pub fn open(_: &Request) -> Result<Self, String> {
        Err("printing is not available".into())
    }
    pub fn page(&mut self, _: &Page) -> Result<(), String> {
        Ok(())
    }
    pub fn finish(self) -> Result<String, String> {
        Ok(String::new())
    }
}

pub fn properties(_printer: &str) -> Result<(), String> {
    Ok(())
}
