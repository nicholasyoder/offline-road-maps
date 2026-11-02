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
    /// Checks that the header's fields fit its records and the records fit the file, so
    /// [`Dbf::get`] stays in bounds.
    fn parse(data: Vec<u8>) -> Result<Self> {
        ensure!(data.len() >= 32, "dbf too short");
        let records = u32::from_le_bytes(data[4..8].try_into()?) as usize;
        let header_len = u16::from_le_bytes(data[8..10].try_into()?) as usize;
        let record_len = u16::from_le_bytes(data[10..12].try_into()?) as usize;
        ensure!(header_len <= data.len(), "dbf header truncated");
        let mut fields = Vec::new();
        let mut at = 32;
        let mut offset = 1; // The deletion flag.
        while at + 32 <= header_len && data[at] != 0x0d {
            let d = &data[at..at + 32];
            let name = String::from_utf8_lossy(&d[..11])
                .trim_end_matches('\0')
                .to_owned();
            let len = d[16] as usize;
            ensure!(
                offset + len <= record_len,
                "dbf field {name} runs past the {record_len}-byte record"
            );
            fields.push(Field { name, offset, len });
            offset += len;
            at += 32;
        }
        let body = records.checked_mul(record_len);
        ensure!(
            body.is_some_and(|b| data.len() - header_len >= b),
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
    while at + 8 <= data.len() {
        let n = out.len() + 1;
        let words = i32::from_be_bytes(data[at + 4..at + 8].try_into()?);
        let len =
            usize::try_from(words).with_context(|| format!("shp record {n}: negative length"))? * 2;
        let rec = data
            .get(at + 8..at + 8 + len)
            .with_context(|| format!("shp record {n} truncated"))?;
        at += 8 + len;
        out.push(polyline(rec).with_context(|| format!("shp record {n}"))?);
    }
    Ok(out)
}

/// One polyline record's points (all its parts in one line), or none for a null shape.
fn polyline(rec: &[u8]) -> Result<Vec<P>> {
    let bytes = |o: usize, n: usize| rec.get(o..o + n).context("record too short");
    let le_i32 = |o| -> Result<i32> { Ok(i32::from_le_bytes(bytes(o, 4)?.try_into()?)) };
    let le_f64 = |o| -> Result<f64> { Ok(f64::from_le_bytes(bytes(o, 8)?.try_into()?)) };
    let shape = le_i32(0)?;
    if shape == 0 {
        return Ok(Vec::new());
    }
    if !matches!(shape, 3 | 13 | 23) {
        bail!("shape type {shape} is not a polyline");
    }
    let count = |o| usize::try_from(le_i32(o)?).context("negative count");
    let (parts, points) = (count(36)?, count(40)?);
    let base = parts.checked_mul(4).and_then(|n| n.checked_add(44));
    let end = base.and_then(|b| points.checked_mul(16)?.checked_add(b));
    let (Some(base), Some(end)) = (base, end) else {
        bail!("{parts} parts and {points} points do not fit the record");
    };
    ensure!(
        end <= rec.len(),
        "{parts} parts and {points} points do not fit the record"
    );
    let mut line = Vec::with_capacity(points);
    for i in 0..points {
        let o = base + i * 16;
        line.push(geom::project(le_f64(o + 8)?, le_f64(o)?));
    }
    Ok(line)
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
    let damaged = || format!("{}: damaged; run `fetch` again", path.display());
    let dbf = Dbf::parse(read(".dbf")?).with_context(damaged)?;
    let lines = parse_shp(&read(".shp")?).with_context(damaged)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.shp` of polyline records, each `(parts, points, coordinates)`.
    fn shp(records: &[(i32, i32, &[f64])]) -> Vec<u8> {
        let mut b = vec![0u8; 100];
        for (n, (parts, points, xy)) in records.iter().enumerate() {
            let mut rec = 3i32.to_le_bytes().to_vec();
            rec.extend([0u8; 32]);
            rec.extend(parts.to_le_bytes());
            rec.extend(points.to_le_bytes());
            rec.extend(0i32.to_le_bytes());
            for v in *xy {
                rec.extend(v.to_le_bytes());
            }
            b.extend((n as i32 + 1).to_be_bytes());
            b.extend((rec.len() as i32 / 2).to_be_bytes());
            b.extend(rec);
        }
        b
    }

    /// A `.dbf` with one record and the fields `(name, length)`; `record_len` as given.
    fn dbf(fields: &[(&str, u8)], record_len: u16) -> Vec<u8> {
        let header_len = 32 + 32 * fields.len() as u16 + 1;
        let mut b = vec![0u8; 32];
        b[4..8].copy_from_slice(&1u32.to_le_bytes());
        b[8..10].copy_from_slice(&header_len.to_le_bytes());
        b[10..12].copy_from_slice(&record_len.to_le_bytes());
        for (name, len) in fields {
            let mut d = [0u8; 32];
            d[..name.len()].copy_from_slice(name.as_bytes());
            d[16] = *len;
            b.extend(d);
        }
        b.push(0x0d);
        b.extend(std::iter::repeat_n(b' ', record_len as usize));
        b
    }

    #[test]
    fn damaged_shapefiles_are_errors() {
        let line = [-75.5, 39.5, -75.4, 39.6];
        assert_eq!(parse_shp(&shp(&[(1, 2, &line)])).unwrap()[0].len(), 2);
        // More points than the record holds, or a negative count: errors, not panics.
        assert!(parse_shp(&shp(&[(1, 3, &line)])).is_err());
        assert!(parse_shp(&shp(&[(-1, 2, &line)])).is_err());
        assert!(parse_shp(&shp(&[(1, i32::MAX, &line)])).is_err());
        // A record whose length runs past the file.
        let full = shp(&[(1, 2, &line)]);
        assert!(parse_shp(&full[..full.len() - 4]).is_err());

        let fields = [("FULLNAME", 10), ("LFROMHN", 5)];
        let d = Dbf::parse(dbf(&fields, 16)).unwrap();
        assert_eq!(d.get(0, d.column("LFROMHN").unwrap()), "");
        // A field past the record, and a file cut short.
        assert!(Dbf::parse(dbf(&fields, 12)).is_err());
        let full = dbf(&fields, 16);
        assert!(Dbf::parse(full[..full.len() - 1].to_vec()).is_err());
        let mut header = dbf(&fields, 16);
        header[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(Dbf::parse(header).is_err());
    }
}
