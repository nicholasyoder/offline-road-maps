//! The PDF writer (phase 4 step 4.10, needed by 4.9's `--print`): one RGB image per page, at
//! the page's size in points, Flate-compressed with PNG predictors. Written by hand: catalog,
//! pages, and per page a page, its content stream and its image. The text on a page is part
//! of the image, so it cannot be selected (decision 18).
//!
//! Pages are written as they are given, so a document never holds more than one page's pixels.

use std::io::{self, Write};

const LEVEL: u8 = 6;

/// A document being written to `W`.
pub struct Pdf<W: Write> {
    out: W,
    written: u64,
    /// Where object `n + 1` starts.
    offsets: Vec<u64>,
    pages: usize,
    title: String,
}

/// Object numbers: 1 the catalog, 2 the page tree, 3 the info; page `i` is 4 + 3i, its content
/// 5 + 3i and its image 6 + 3i.
const FIRST_PAGE: usize = 4;

impl<W: Write> Pdf<W> {
    pub fn new(out: W, title: &str) -> io::Result<Self> {
        let mut pdf = Self {
            out,
            written: 0,
            offsets: Vec::new(),
            pages: 0,
            title: title.to_owned(),
        };
        // The second line has bytes above 127, which tells programs the file is binary.
        pdf.put(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(pdf)
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.written += bytes.len() as u64;
        self.out.write_all(bytes)
    }

    /// Starts object `n`.
    fn begin(&mut self, n: usize) -> io::Result<()> {
        if self.offsets.len() < n {
            self.offsets.resize(n, 0);
        }
        self.offsets[n - 1] = self.written;
        self.put(format!("{n} 0 obj\n").as_bytes())
    }

    /// Adds a page of `rgb` pixels (`w * h * 3` bytes, row by row), `size_pt` points on paper.
    pub fn page(&mut self, rgb: &[u8], [w, h]: [u32; 2], size_pt: [f64; 2]) -> io::Result<()> {
        assert_eq!(rgb.len(), w as usize * h as usize * 3);
        let i = self.pages;
        let page = FIRST_PAGE + 3 * i;
        // Objects 1 to 3 are written last, by `finish`.
        self.begin(page)?;
        self.put(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>\nendobj\n",
                num(size_pt[0]),
                num(size_pt[1]),
                page + 2,
                page + 1
            )
            .as_bytes(),
        )?;
        let content = format!(
            "q {} 0 0 {} 0 0 cm /Im0 Do Q\n",
            num(size_pt[0]),
            num(size_pt[1])
        );
        self.begin(page + 1)?;
        self.put(format!("<< /Length {} >>\nstream\n", content.len()).as_bytes())?;
        self.put(content.as_bytes())?;
        self.put(b"endstream\nendobj\n")?;
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&predict(rgb, w as usize), LEVEL);
        self.begin(page + 2)?;
        self.put(
            format!(
                "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns {w} >> /Length {} >>\nstream\n",
                packed.len()
            )
            .as_bytes(),
        )?;
        self.put(&packed)?;
        self.put(b"\nendstream\nendobj\n")?;
        self.pages += 1;
        Ok(())
    }

    /// Writes the catalog, the page tree and the info, then the cross-reference table and the
    /// trailer; returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        let kids: Vec<String> = (0..self.pages)
            .map(|i| format!("{} 0 R", FIRST_PAGE + 3 * i))
            .collect();
        self.begin(1)?;
        self.put(b"<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;
        self.begin(2)?;
        self.put(
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
                kids.join(" "),
                self.pages
            )
            .as_bytes(),
        )?;
        let title = escape(&self.title);
        self.begin(3)?;
        self.put(
            format!("<< /Title ({title}) /Producer (Offline Road Maps) >>\nendobj\n").as_bytes(),
        )?;
        let xref_at = self.written;
        let total = self.offsets.len() + 1;
        let mut x = format!("xref\n0 {total}\n0000000000 65535 f \n");
        for off in &self.offsets {
            x += &format!("{off:010} 00000 n \n");
        }
        x += &format!(
            "trailer\n<< /Size {total} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
        );
        self.put(x.as_bytes())?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// A number for a content stream: no more digits than the points need.
fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A PDF string's text with its parentheses and backslashes escaped; characters beyond
/// ASCII are left out, as the info dictionary is plain.
fn escape(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii)
        .flat_map(|c| match c {
            '(' | ')' | '\\' => vec!['\\', c],
            c if c.is_ascii_control() => Vec::new(),
            c => vec![c],
        })
        .collect()
}

/// The rows with a PNG predictor in front of each (Predictor 15): Sub, Up or none, whichever
/// leaves the smallest bytes in all, as signed values.
fn predict(rgb: &[u8], width: usize) -> Vec<u8> {
    let row = width * 3;
    let rows = rgb.len().checked_div(row).unwrap_or(0);
    let mut out = Vec::with_capacity(rgb.len() + rows);
    let mut sub = vec![0u8; row];
    let mut up = vec![0u8; row];
    let cost = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|&b| u32::from((b as i8).unsigned_abs()))
            .sum::<u32>()
    };
    for y in 0..rows {
        let cur = &rgb[y * row..(y + 1) * row];
        let prev = (y > 0).then(|| &rgb[(y - 1) * row..y * row]);
        for x in 0..row {
            let left = if x >= 3 { cur[x - 3] } else { 0 };
            sub[x] = cur[x].wrapping_sub(left);
            up[x] = cur[x].wrapping_sub(prev.map_or(0, |p| p[x]));
        }
        let none = cost(cur);
        let (s, u) = (cost(&sub), cost(&up));
        if u <= s && u <= none {
            out.push(2);
            out.extend_from_slice(&up);
        } else if s <= none {
            out.push(1);
            out.extend_from_slice(&sub);
        } else {
            out.push(0);
            out.extend_from_slice(cur);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Undoes `predict`.
    fn unpredict(data: &[u8], width: usize) -> Vec<u8> {
        let row = width * 3;
        let mut out: Vec<u8> = Vec::new();
        for line in data.chunks(row + 1) {
            let (kind, bytes) = (line[0], &line[1..]);
            let y = out.len() / row;
            for (x, &b) in bytes.iter().enumerate() {
                let left = if x >= 3 { out[y * row + x - 3] } else { 0 };
                let up = if y > 0 { out[(y - 1) * row + x] } else { 0 };
                out.push(match kind {
                    0 => b,
                    1 => b.wrapping_add(left),
                    2 => b.wrapping_add(up),
                    k => panic!("filter {k}"),
                });
            }
        }
        out
    }

    fn picture(w: usize, h: usize) -> Vec<u8> {
        (0..w * h * 3)
            .map(|i| ((i / 3 % w) * 3 + (i / 3 / w) * 7 + i % 3 * 50) as u8)
            .collect()
    }

    #[test]
    fn predictors_round_trip() {
        let rgb = picture(37, 23);
        assert_eq!(unpredict(&predict(&rgb, 37), 37), rgb);
        let flat = vec![200u8; 30 * 10 * 3];
        assert_eq!(unpredict(&predict(&flat, 30), 30), flat);
    }

    /// Finds the offset `startxref` gives and reads the table it points at.
    fn xref(pdf: &[u8]) -> Vec<u64> {
        let tail = String::from_utf8_lossy(&pdf[pdf.len() - 40..]).into_owned();
        let at: usize = tail
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let table = String::from_utf8_lossy(&pdf[at..]).into_owned();
        assert!(table.starts_with("xref\n0 "));
        let mut lines = table.lines().skip(1);
        let n: usize = lines
            .next()
            .unwrap()
            .trim_start_matches("0 ")
            .parse()
            .unwrap();
        // The head of the table: the free entry.
        assert!(lines.next().unwrap().ends_with("65535 f "));
        lines
            .take(n - 1)
            .map(|l| l[..10].parse().unwrap())
            .collect()
    }

    #[test]
    fn a_document_is_well_formed() {
        let (w, h) = (40usize, 30usize);
        let rgb = picture(w, h);
        let mut pdf = Pdf::new(Vec::new(), "Route (to) X").unwrap();
        pdf.page(&rgb, [w as u32, h as u32], [612.0, 792.0])
            .unwrap();
        pdf.page(&rgb, [w as u32, h as u32], [595.276, 841.89])
            .unwrap();
        let bytes = pdf.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4\n"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        // Every entry points at its own "n 0 obj".
        let table = xref(&bytes);
        assert_eq!(table.len(), 3 + 2 * 3);
        assert!(table.iter().all(|&o| o > 0));
        for (i, &off) in table.iter().enumerate() {
            let num = i + 1;
            let at = &bytes[off as usize..];
            assert!(
                at.starts_with(format!("{num} 0 obj\n").as_bytes()),
                "entry {i} at {off}: {:?}",
                String::from_utf8_lossy(&at[..20])
            );
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/MediaBox [0 0 612 792]"));
        assert!(text.contains("/MediaBox [0 0 595.276 841.89]"));
        assert!(text.contains("/Count 2"));
        assert!(text.contains("/Title (Route \\(to\\) X)"));
    }

    #[test]
    fn the_image_decodes_to_the_pixels() {
        let (w, h) = (40usize, 30usize);
        let rgb = picture(w, h);
        let mut pdf = Pdf::new(Vec::new(), "t").unwrap();
        pdf.page(&rgb, [w as u32, h as u32], [100.0, 75.0]).unwrap();
        let bytes = pdf.finish().unwrap();
        let image = bytes
            .windows(14)
            .position(|b| b == b"Subtype /Image")
            .unwrap();
        let start = image
            + bytes[image..]
                .windows(7)
                .position(|b| b == b"stream\n")
                .unwrap()
            + 7;
        let end = start
            + bytes[start..]
                .windows(10)
                .position(|b| b == b"\nendstream")
                .unwrap();
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&bytes[start..end]).unwrap();
        assert_eq!(unpredict(&raw, w), rgb);
    }
}
