//! House-number ranges: TIGER/Line `ADDRFEAT` (per side of each street segment, read from
//! the `.shp` and `.dbf` in each county zip) and OSM `addr:interpolation` ways.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use omp_core::normalize::{NumberKind, house_number};
use rayon::prelude::*;

use crate::geom::{self, P};

/// One side of one TIGER segment, or one stretch of an OSM interpolation way between two
/// numbered nodes.
pub struct Range {
    /// TIGER side relative to the segment's direction; `None` for OSM, where the line itself
    /// lies on one side of the street.
    pub right: Option<bool>,
    pub from: u32,
    pub to: u32,
    /// `O` odd, `E` even, `B` both, as in `PARITYL`/`PARITYR`.
    pub parity: u8,
    pub street: String,
    pub line: Vec<P>,
}

impl Range {
    pub fn contains(&self, n: u32) -> bool {
        let (lo, hi) = (self.from.min(self.to), self.from.max(self.to));
        let parity_ok = match self.parity {
            b'O' => n % 2 == 1,
            b'E' => n.is_multiple_of(2),
            _ => true,
        };
        (lo..=hi).contains(&n) && parity_ok
    }
}

struct Field {
    name: String,
    offset: usize,
    len: usize,
}

struct Dbf {
    data: Vec<u8>,
    header_len: usize,
    record_len: usize,
    records: usize,
    fields: Vec<Field>,
}

impl Dbf {
    fn parse(data: Vec<u8>) -> Result<Self> {
        ensure!(data.len() >= 32, "dbf too short");
        let records = u32::from_le_bytes(data[4..8].try_into()?) as usize;
        let header_len = u16::from_le_bytes(data[8..10].try_into()?) as usize;
        let record_len = u16::from_le_bytes(data[10..12].try_into()?) as usize;
        let mut fields = Vec::new();
        let mut at = 32;
        let mut offset = 1; // The deletion flag.
        while at + 32 <= header_len && data[at] != 0x0d {
            let d = &data[at..at + 32];
            let name = String::from_utf8_lossy(&d[..11])
                .trim_end_matches('\0')
                .to_owned();
            let len = d[16] as usize;
            fields.push(Field { name, offset, len });
            offset += len;
            at += 32;
        }
        ensure!(
            data.len() >= header_len + records * record_len,
            "dbf truncated"
        );
        Ok(Self {
            data,
            header_len,
            record_len,
            records,
            fields,
        })
    }

    fn column(&self, name: &str) -> Result<usize> {
        self.fields
            .iter()
            .position(|f| f.name == name)
            .with_context(|| format!("dbf has no {name} field"))
    }

    fn get(&self, rec: usize, col: usize) -> &str {
        let f = &self.fields[col];
        let start = self.header_len + rec * self.record_len + f.offset;
        std::str::from_utf8(&self.data[start..start + f.len])
            .unwrap_or_default()
            .trim()
    }
}

/// Polylines in record order; null shapes give an empty line.
fn parse_shp(data: &[u8]) -> Result<Vec<Vec<P>>> {
    ensure!(data.len() >= 100, "shp too short");
    let mut out = Vec::new();
    let mut at = 100;
    let le_i32 = |b: &[u8]| i32::from_le_bytes(b[..4].try_into().unwrap());
    let le_f64 = |b: &[u8]| f64::from_le_bytes(b[..8].try_into().unwrap());
    while at + 8 <= data.len() {
        let words = i32::from_be_bytes(data[at + 4..at + 8].try_into()?) as usize;
        let rec = data
            .get(at + 8..at + 8 + words * 2)
            .context("shp record truncated")?;
        at += 8 + words * 2;
        let shape = le_i32(rec);
        if shape == 0 {
            out.push(Vec::new());
            continue;
        }
        if !matches!(shape, 3 | 13 | 23) {
            bail!("shape type {shape} is not a polyline");
        }
        let parts = le_i32(&rec[36..]) as usize;
        let points = le_i32(&rec[40..]) as usize;
        let base = 44 + parts * 4;
        let line = (0..points)
            .map(|i| {
                let o = base + i * 16;
                geom::project(le_f64(&rec[o + 8..]), le_f64(&rec[o..]))
            })
            .collect();
        out.push(line);
    }
    Ok(out)
}

fn read_zip(path: &PathBuf) -> Result<Vec<Range>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut z = zip::ZipArchive::new(f).with_context(|| format!("read {}", path.display()))?;
    let mut read = |ext: &str| -> Result<Vec<u8>> {
        let name = z
            .file_names()
            .find_map(|n| n.ok().filter(|n| n.ends_with(ext)).map(|n| n.into_owned()))
            .with_context(|| format!("no {ext} in {}", path.display()))?;
        let mut buf = Vec::new();
        z.by_name(&name)?.read_to_end(&mut buf)?;
        Ok(buf)
    };
    let dbf = Dbf::parse(read(".dbf")?)?;
    let lines = parse_shp(&read(".shp")?)?;
    ensure!(
        lines.len() == dbf.records,
        "{}: {} shapes vs {} records",
        path.display(),
        lines.len(),
        dbf.records
    );
    let col = |n| dbf.column(n);
    let name = col("FULLNAME")?;
    let sides = [
        (false, col("LFROMHN")?, col("LTOHN")?, col("PARITYL")?),
        (true, col("RFROMHN")?, col("RTOHN")?, col("PARITYR")?),
    ];
    let mut ranges = Vec::new();
    for (rec, line) in lines.into_iter().enumerate() {
        if line.len() < 2 {
            continue;
        }
        for &(right, from, to, parity) in &sides {
            let (f, t) = (dbf.get(rec, from), dbf.get(rec, to));
            if f.is_empty() && t.is_empty() {
                continue;
            }
            let plain = |s: &str| house_number(s).filter(|h| h.kind == NumberKind::Plain);
            // Range ends that are not plain numbers (e.g. "100-01") drop the range.
            let (Some(f), Some(t)) = (plain(f), plain(t)) else {
                continue;
            };
            ranges.push(Range {
                right: Some(right),
                from: f.number,
                to: t.number,
                parity: dbf.get(rec, parity).bytes().next().unwrap_or(b'B'),
                street: dbf.get(rec, name).to_owned(),
                line: line.clone(),
            });
        }
    }
    Ok(ranges)
}

pub fn read(files: &[PathBuf]) -> Result<Vec<Range>> {
    let parts: Vec<_> = files.par_iter().map(read_zip).collect::<Result<_>>()?;
    Ok(parts.into_iter().flatten().collect())
}
