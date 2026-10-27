//! The pack container: header, section table, chunk files and checksums, as specified in
//! `docs/format.md`. Section contents are opaque bytes here.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

pub const MAGIC: [u8; 8] = *b"\x89OMP\r\n\x1a\n";
pub const MAJOR: u16 = 0;
pub const MINOR: u16 = 4;
/// The oldest minor version the reader opens: 0.4 only added sections (`SGRD`) and
/// changed provisional ones that the 0.3 reader did not read.
pub const MIN_MINOR: u16 = 3;
/// Chunk pages and section alignment.
pub const PAGE: u64 = 4096;
/// Payload bytes per chunk file, so a file stays under FAT32's 4 GiB − 1 byte.
pub const DEFAULT_CHUNK_CAPACITY: u64 = (1 << 32) - (1 << 20);
/// Section flag: the section may cross chunk boundaries (its records never do).
pub const SPANNING: u32 = 1;

const ENTRY_LEN: usize = 32;
const MAX_STR: usize = 255;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// What the pack header says about the region and build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub pack_id: [u8; 16],
    /// West, south, east, north in 10⁻⁷ degrees.
    pub bbox: [i32; 4],
    /// Unix seconds, UTC.
    pub build_time: i64,
    pub name: String,
    pub snapshot: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: [u8; 4],
    pub flags: u32,
    /// Virtual offset, a multiple of [`PAGE`].
    pub offset: u64,
    pub len: u64,
    pub checksum: u64,
}

impl Section {
    pub fn spanning(&self) -> bool {
        self.flags & SPANNING != 0
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.checksum.to_le_bytes());
    }

    fn decode(b: &[u8]) -> Self {
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        Self {
            id: b[0..4].try_into().unwrap(),
            flags: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            offset: u64_at(8),
            len: u64_at(16),
            checksum: u64_at(24),
        }
    }
}

/// `<dir>/<name>.omp.NNN`.
pub fn chunk_path(dir: &Path, name: &str, index: u32) -> PathBuf {
    dir.join(format!("{name}.omp.{index:03}"))
}

fn put_str(out: &mut Vec<u8>, s: &str) -> io::Result<()> {
    if s.len() > MAX_STR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("header string longer than {MAX_STR} bytes"),
        ));
    }
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn round_up(v: u64, to: u64) -> u64 {
    v.div_ceil(to) * to
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_write(file, buf, offset)?;
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_read(file, buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// A section being written: its start and running checksum.
struct Open {
    id: [u8; 4],
    flags: u32,
    offset: u64,
    hash: Xxh3,
}

/// Writes a pack. Sections are appended in order; [`Writer::finish`] adds the section
/// table and headers.
pub struct Writer {
    dir: PathBuf,
    name: String,
    capacity: u64,
    files: Vec<File>,
    /// Next free virtual offset.
    pos: u64,
    sections: Vec<Section>,
    open: Option<Open>,
}

impl Writer {
    /// Creates `<dir>/<name>.omp.000`; later chunks are created as needed.
    pub fn create(dir: &Path, name: &str, capacity: u64) -> io::Result<Self> {
        if capacity == 0 || !capacity.is_multiple_of(PAGE) || PAGE + capacity > u64::from(u32::MAX)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk capacity must be a non-zero multiple of 4096 that fits FAT32",
            ));
        }
        let mut w = Self {
            dir: dir.to_owned(),
            name: name.to_owned(),
            capacity,
            files: Vec::new(),
            pos: 0,
            sections: Vec::new(),
            open: None,
        };
        w.chunk(0)?;
        Ok(w)
    }

    fn chunk(&mut self, index: usize) -> io::Result<&File> {
        while self.files.len() <= index {
            let path = chunk_path(&self.dir, &self.name, self.files.len() as u32);
            self.files.push(File::create(path)?);
        }
        Ok(&self.files[index])
    }

    /// Writes `buf` at virtual offset `v`; it must not cross a chunk boundary.
    fn put(&mut self, v: u64, buf: &[u8]) -> io::Result<()> {
        let (chunk, local) = (v / self.capacity, v % self.capacity);
        debug_assert!(local + buf.len() as u64 <= self.capacity);
        write_at(self.chunk(chunk as usize)?, buf, PAGE + local)
    }

    /// Room left in the chunk that holds `v`.
    fn room(&self, v: u64) -> u64 {
        self.capacity - v % self.capacity
    }

    fn too_big(&self, what: &str, len: usize) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{what} of {len} bytes exceeds the chunk capacity {}",
                self.capacity
            ),
        )
    }

    /// Adds a contiguous section: it never crosses a chunk boundary.
    pub fn section(&mut self, id: [u8; 4], data: &[u8]) -> io::Result<Section> {
        assert!(self.open.is_none(), "a spanning section is still open");
        if data.len() as u64 > self.capacity {
            return Err(self.too_big("contiguous section", data.len()));
        }
        let mut offset = round_up(self.pos, PAGE);
        if (data.len() as u64) > self.room(offset) {
            offset = round_up(offset, self.capacity);
        }
        self.put(offset, data)?;
        self.pos = offset + data.len() as u64;
        let s = Section {
            id,
            flags: 0,
            offset,
            len: data.len() as u64,
            checksum: xxh3_64(data),
        };
        self.sections.push(s);
        Ok(s)
    }

    /// Starts a spanning section; add its records with [`Writer::record`].
    pub fn begin_spanning(&mut self, id: [u8; 4]) {
        assert!(self.open.is_none(), "a spanning section is still open");
        self.pos = round_up(self.pos, PAGE);
        self.open = Some(Open {
            id,
            flags: SPANNING,
            offset: self.pos,
            hash: Xxh3::new(),
        });
    }

    /// Appends one record to the open spanning section and returns its virtual offset. A
    /// record that would cross a chunk boundary starts at the next chunk instead.
    pub fn record(&mut self, data: &[u8]) -> io::Result<u64> {
        if data.len() as u64 > self.capacity {
            return Err(self.too_big("record", data.len()));
        }
        let mut at = self.pos;
        if data.len() as u64 > self.room(at) {
            at = round_up(at, self.capacity);
        }
        let pad = (at - self.pos) as usize;
        let open = self.open.as_mut().expect("no spanning section is open");
        open.hash.update(&vec![0; pad]);
        open.hash.update(data);
        self.put(at, data)?;
        self.pos = at + data.len() as u64;
        Ok(at)
    }

    /// Ends the open spanning section.
    pub fn end_spanning(&mut self) -> Section {
        let open = self.open.take().expect("no spanning section is open");
        let s = Section {
            id: open.id,
            flags: open.flags,
            offset: open.offset,
            len: self.pos - open.offset,
            checksum: open.hash.digest(),
        };
        self.sections.push(s);
        s
    }

    /// Writes the section table and headers, and sets each chunk's final length.
    pub fn finish(mut self, info: &Info) -> io::Result<Vec<PathBuf>> {
        assert!(self.open.is_none(), "a spanning section is still open");
        let mut ids: Vec<[u8; 4]> = self.sections.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "duplicate section ID",
            ));
        }
        let mut table = Vec::with_capacity(self.sections.len() * ENTRY_LEN);
        for s in &self.sections {
            s.encode(&mut table);
        }
        let n_sections = self.sections.len() as u32;
        let t = self.section(*b"\0\0\0\0", &table)?;
        self.sections.pop();

        let chunks = self.pos.div_ceil(self.capacity).max(1) as u32;
        let mut h = Vec::with_capacity(PAGE as usize);
        h.extend_from_slice(&MAGIC);
        h.extend_from_slice(&MAJOR.to_le_bytes());
        h.extend_from_slice(&MINOR.to_le_bytes());
        h.extend_from_slice(&[0; 4]); // header length, filled in below
        h.extend_from_slice(&info.pack_id);
        h.extend_from_slice(&self.capacity.to_le_bytes());
        h.extend_from_slice(&chunks.to_le_bytes());
        h.extend_from_slice(&n_sections.to_le_bytes());
        h.extend_from_slice(&t.offset.to_le_bytes());
        h.extend_from_slice(&t.len.to_le_bytes());
        h.extend_from_slice(&t.checksum.to_le_bytes());
        for v in info.bbox {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.extend_from_slice(&info.build_time.to_le_bytes());
        put_str(&mut h, &info.name)?;
        put_str(&mut h, &info.snapshot)?;
        let len = (h.len() + 8) as u32;
        h[12..16].copy_from_slice(&len.to_le_bytes());
        h.extend_from_slice(&xxh3_64(&h).to_le_bytes());
        write_at(self.chunk(0)?, &h, 0)?;

        for i in 1..chunks {
            let mut c = Vec::with_capacity(40);
            c.extend_from_slice(&MAGIC);
            c.extend_from_slice(&MAJOR.to_le_bytes());
            c.extend_from_slice(&MINOR.to_le_bytes());
            c.extend_from_slice(&i.to_le_bytes());
            c.extend_from_slice(&info.pack_id);
            c.extend_from_slice(&xxh3_64(&c).to_le_bytes());
            write_at(self.chunk(i as usize)?, &c, 0)?;
        }
        let mut paths = Vec::new();
        for i in 0..chunks {
            let end = if i + 1 < chunks {
                self.capacity
            } else {
                self.pos - u64::from(i) * self.capacity
            };
            let f = self.chunk(i as usize)?;
            f.set_len(PAGE + end)?;
            f.sync_all()?;
            paths.push(chunk_path(&self.dir, &self.name, i));
        }
        Ok(paths)
    }
}

/// An open pack.
pub struct Pack {
    pub info: Info,
    pub minor: u16,
    pub capacity: u64,
    pub sections: Vec<Section>,
    files: Vec<File>,
}

impl Pack {
    /// Opens a pack from its first chunk, `<name>.omp.000`. Checks the header, every chunk
    /// header and length, and the section table; section checksums are left to
    /// [`Pack::verify`].
    pub fn open(first: &Path) -> io::Result<Self> {
        let file_name = first.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let name = file_name
            .strip_suffix(".omp.000")
            .ok_or_else(|| invalid("expected the path of a `.omp.000` file"))?;
        let dir = first.parent().unwrap_or(Path::new("."));
        let f0 = File::open(first)?;
        let mut page = vec![0; PAGE as usize];
        read_at(&f0, &mut page, 0)?;
        if page[0..8] != MAGIC {
            return Err(invalid("not a pack (bad magic)"));
        }
        let u16_at = |i: usize| u16::from_le_bytes(page[i..i + 2].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(page[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(page[i..i + 8].try_into().unwrap());
        let i32_at = |i: usize| i32::from_le_bytes(page[i..i + 4].try_into().unwrap());
        let (major, minor) = (u16_at(8), u16_at(10));
        // Before 1.0, minor versions may break the format too.
        if major != MAJOR || (MAJOR == 0 && !(MIN_MINOR..=MINOR).contains(&minor)) {
            return Err(invalid(format!(
                "pack format {major}.{minor}, this reader supports {MAJOR}.{MINOR}"
            )));
        }
        let len = u32_at(12) as usize;
        if !(106..=PAGE as usize).contains(&len) || xxh3_64(&page[..len - 8]) != u64_at(len - 8) {
            return Err(invalid("pack header checksum mismatch"));
        }
        let pack_id: [u8; 16] = page[16..32].try_into().unwrap();
        let capacity = u64_at(32);
        let chunks = u32_at(40);
        let n_sections = u32_at(44) as usize;
        let (t_off, t_len, t_sum) = (u64_at(48), u64_at(56), u64_at(64));
        let bbox = [i32_at(72), i32_at(76), i32_at(80), i32_at(84)];
        let build_time = u64_at(88) as i64;
        let mut at = 96;
        let mut take_str = || -> io::Result<String> {
            let n = *page.get(at).ok_or_else(|| invalid("truncated header"))? as usize;
            let s = page
                .get(at + 1..at + 1 + n)
                .filter(|_| at + 1 + n <= len - 8)
                .ok_or_else(|| invalid("truncated header"))?;
            at += 1 + n;
            String::from_utf8(s.to_vec()).map_err(|_| invalid("header string is not UTF-8"))
        };
        let region = take_str()?;
        let snapshot = take_str()?;
        if capacity == 0 || !capacity.is_multiple_of(PAGE) || chunks == 0 {
            return Err(invalid("bad chunk layout in header"));
        }

        let mut files = vec![f0];
        for i in 1..chunks {
            let f = File::open(chunk_path(dir, name, i))?;
            let mut c = [0; 40];
            read_at(&f, &mut c, 0)?;
            let ok = c[0..8] == MAGIC
                && c[8..12] == page[8..12]
                && u32::from_le_bytes(c[12..16].try_into().unwrap()) == i
                && c[16..32] == pack_id
                && xxh3_64(&c[..32]) == u64::from_le_bytes(c[32..40].try_into().unwrap());
            if !ok {
                return Err(invalid(format!(
                    "chunk {i:03} does not belong to this pack"
                )));
            }
            files.push(f);
        }
        for (i, f) in files.iter().enumerate() {
            let len = f.metadata()?.len();
            let short = i + 1 < files.len() && len != PAGE + capacity;
            if short || len < PAGE || len > PAGE + capacity {
                return Err(invalid(format!("chunk {i:03} has the wrong length {len}")));
            }
        }

        let mut pack = Self {
            info: Info {
                pack_id,
                bbox,
                build_time,
                name: region,
                snapshot,
            },
            minor,
            capacity,
            sections: Vec::new(),
            files,
        };
        if t_len != (n_sections * ENTRY_LEN) as u64 {
            return Err(invalid("section table length mismatch"));
        }
        let table = pack.read(t_off, t_len as usize)?;
        if xxh3_64(&table) != t_sum {
            return Err(invalid("section table checksum mismatch"));
        }
        pack.sections = table
            .as_chunks::<ENTRY_LEN>()
            .0
            .iter()
            .map(|b| Section::decode(b))
            .collect();
        // Checked, so a crafted table cannot wrap around and pass.
        let end = u64::from(chunks).checked_mul(capacity);
        for s in &pack.sections {
            let fits = |limit: Option<u64>, at: u64| matches!((at.checked_add(s.len), limit), (Some(e), Some(l)) if e <= l);
            let in_chunk = s.spanning() || fits(Some(capacity), s.offset % capacity);
            if !s.offset.is_multiple_of(PAGE) || !fits(end, s.offset) || !in_chunk {
                return Err(invalid(format!(
                    "section {} is out of bounds",
                    String::from_utf8_lossy(&s.id)
                )));
            }
        }
        Ok(pack)
    }

    pub fn chunks(&self) -> usize {
        self.files.len()
    }

    pub fn section(&self, id: [u8; 4]) -> Option<&Section> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// Reads `len` bytes at virtual offset `v`. The range must lie in one chunk, as every
    /// contiguous section and every record does.
    pub fn read(&self, v: u64, len: usize) -> io::Result<Vec<u8>> {
        let (chunk, local) = (v / self.capacity, v % self.capacity);
        if local
            .checked_add(len as u64)
            .is_none_or(|e| e > self.capacity)
        {
            return Err(invalid("read crosses a chunk boundary"));
        }
        let f = self
            .files
            .get(chunk as usize)
            .ok_or_else(|| invalid("read past the last chunk"))?;
        let mut buf = vec![0; len];
        read_at(f, &mut buf, PAGE + local)?;
        Ok(buf)
    }

    /// The whole of a contiguous section.
    pub fn read_section(&self, s: &Section) -> io::Result<Vec<u8>> {
        if s.spanning() {
            return Err(invalid("a spanning section is read record by record"));
        }
        self.read(s.offset, s.len as usize)
    }

    /// Hashes every section; returns the IDs whose checksum does not match.
    pub fn verify(&self) -> io::Result<Vec<[u8; 4]>> {
        self.verify_with(|_, _| {})
    }

    /// [`Pack::verify`], calling `progress(bytes hashed, total)` every 8 MiB.
    pub fn verify_with(&self, mut progress: impl FnMut(u64, u64)) -> io::Result<Vec<[u8; 4]>> {
        self.verify_until(|done, total| {
            progress(done, total);
            true
        })
    }

    /// [`Pack::verify_with`], stopped with [`io::ErrorKind::Interrupted`] when `progress`
    /// returns `false` (the app's "Verify pack", which can be cancelled).
    pub fn verify_until(
        &self,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> io::Result<Vec<[u8; 4]>> {
        const STEP: u64 = 8 << 20;
        let total: u64 = self.sections.iter().map(|s| s.len).sum();
        let mut done = 0;
        let mut bad = Vec::new();
        for s in &self.sections {
            let mut h = Xxh3::new();
            let (mut v, end) = (s.offset, s.offset + s.len);
            while v < end {
                let n = (end - v).min(STEP).min(self.capacity - v % self.capacity);
                h.update(&self.read(v, n as usize)?);
                v += n;
                done += n;
                if !progress(done, total) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
            }
            if h.digest() != s.checksum {
                bad.push(s.id);
            }
        }
        Ok(bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info {
        Info {
            pack_id: [7; 16],
            bbox: [-1_000_000_000, 400_000_000, -800_000_000, 420_000_000],
            build_time: 1_793_000_000,
            name: "Test".into(),
            snapshot: "osm 2026-10-01".into(),
        }
    }

    #[test]
    fn round_trips_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let cap = 3 * PAGE;
        let mut w = Writer::create(dir.path(), "t", cap).unwrap();
        let meta = w.section(*b"META", b"{\"a\":1}").unwrap();
        w.begin_spanning(*b"TDAT");
        let records: Vec<Vec<u8>> = (0..12u8)
            .map(|i| vec![i; 1500 + usize::from(i) * 100])
            .collect();
        let offsets: Vec<u64> = records.iter().map(|r| w.record(r).unwrap()).collect();
        let tdat = w.end_spanning();
        let big = vec![9; 2 * PAGE as usize + 10];
        let tidx = w.section(*b"TIDX", &big).unwrap();
        let paths = w.finish(&info()).unwrap();
        assert!(paths.len() >= 4, "{paths:?}");

        assert_eq!(meta.offset, 0);
        assert!(tdat.spanning() && tdat.offset == PAGE);
        // The contiguous section moved to a chunk start rather than cross a boundary.
        assert_eq!(tidx.offset % cap, 0);
        for (o, r) in offsets.iter().zip(&records) {
            assert!(o % cap + r.len() as u64 <= cap, "record crosses a boundary");
        }

        let p = Pack::open(&paths[0]).unwrap();
        assert_eq!(p.info, info());
        assert_eq!(p.sections.len(), 3);
        assert_eq!(
            p.read_section(p.section(*b"META").unwrap()).unwrap(),
            b"{\"a\":1}"
        );
        assert_eq!(p.read_section(p.section(*b"TIDX").unwrap()).unwrap(), big);
        for (o, r) in offsets.iter().zip(&records) {
            assert_eq!(&p.read(*o, r.len()).unwrap(), r);
        }
        assert!(p.verify().unwrap().is_empty());
        let mut last = (0, 0);
        p.verify_with(|done, total| last = (done, total)).unwrap();
        assert_eq!(last, (last.1, meta.len + tdat.len + tidx.len));
        let mut calls = 0;
        let stopped = p.verify_until(|_, _| {
            calls += 1;
            false
        });
        assert_eq!(stopped.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(calls, 1);
        assert!(p.read_section(&tdat).is_err());
    }

    #[test]
    fn detects_damage() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::create(dir.path(), "t", 2 * PAGE).unwrap();
        w.section(*b"META", &[1; 100]).unwrap();
        w.section(*b"TIDX", &[2; 5000]).unwrap();
        let paths = w.finish(&info()).unwrap();

        // A flipped byte inside a section is found by `verify`, not by `open`.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&paths[0])
            .unwrap();
        write_at(&f, &[0xff], PAGE + 10).unwrap();
        let p = Pack::open(&paths[0]).unwrap();
        assert_eq!(p.verify().unwrap(), vec![*b"META"]);

        // A damaged header is refused on open.
        write_at(&f, b"X", 100).unwrap();
        assert!(Pack::open(&paths[0]).is_err());
    }

    #[test]
    fn opens_older_minor_versions() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::create(dir.path(), "t", 2 * PAGE).unwrap();
        w.section(*b"META", &[1; 100]).unwrap();
        let paths = w.finish(&info()).unwrap();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&paths[0])
            .unwrap();
        let set_minor = |minor: u16| {
            let mut page = vec![0; PAGE as usize];
            read_at(&f, &mut page, 0).unwrap();
            page[10..12].copy_from_slice(&minor.to_le_bytes());
            let len = u32::from_le_bytes(page[12..16].try_into().unwrap()) as usize;
            let h = xxh3_64(&page[..len - 8]);
            page[len - 8..len].copy_from_slice(&h.to_le_bytes());
            write_at(&f, &page, 0).unwrap();
        };
        set_minor(MIN_MINOR);
        assert_eq!(Pack::open(&paths[0]).unwrap().minor, MIN_MINOR);
        set_minor(MIN_MINOR - 1);
        assert!(Pack::open(&paths[0]).is_err());
        set_minor(MINOR + 1);
        assert!(Pack::open(&paths[0]).is_err());
    }

    #[test]
    fn refuses_mixed_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let build = |name: &str, id: u8| {
            let mut w = Writer::create(dir.path(), name, PAGE).unwrap();
            w.section(*b"AAAA", &[1; 4000]).unwrap();
            w.section(*b"BBBB", &[2; 4000]).unwrap();
            let mut i = info();
            i.pack_id = [id; 16];
            w.finish(&i).unwrap()
        };
        let a = build("a", 1);
        let b = build("b", 2);
        assert!(Pack::open(&a[0]).is_ok());
        std::fs::copy(&b[1], &a[1]).unwrap();
        assert!(Pack::open(&a[0]).is_err());
    }

    #[test]
    fn refuses_section_bounds_that_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::create(dir.path(), "t", 4 * PAGE).unwrap();
        w.section(*b"META", &[1; 100]).unwrap();
        w.section(*b"TIDX", &[2; 100]).unwrap();
        let paths = w.finish(&info()).unwrap();
        assert!(Pack::open(&paths[0]).is_ok());

        // Give TIDX a length that wraps `offset + len`, with valid checksums.
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&paths[0])
            .unwrap();
        let mut page = vec![0; PAGE as usize];
        read_at(&f, &mut page, 0).unwrap();
        let u64_at = |p: &[u8], i: usize| u64::from_le_bytes(p[i..i + 8].try_into().unwrap());
        let (t_off, t_len) = (u64_at(&page, 48), u64_at(&page, 56));
        let mut table = vec![0; t_len as usize];
        read_at(&f, &mut table, PAGE + t_off).unwrap();
        let tidx = &mut table[ENTRY_LEN..2 * ENTRY_LEN];
        assert_eq!(&tidx[0..4], b"TIDX");
        assert_ne!(u64_at(tidx, 8), 0);
        tidx[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        write_at(&f, &table, PAGE + t_off).unwrap();
        page[64..72].copy_from_slice(&xxh3_64(&table).to_le_bytes());
        let len = u32::from_le_bytes(page[12..16].try_into().unwrap()) as usize;
        let h = xxh3_64(&page[..len - 8]);
        page[len - 8..len].copy_from_slice(&h.to_le_bytes());
        write_at(&f, &page, 0).unwrap();

        let err = Pack::open(&paths[0]).err().unwrap();
        assert!(err.to_string().contains("TIDX is out of bounds"), "{err}");
    }

    #[test]
    fn rejects_oversized_and_duplicate_sections() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer::create(dir.path(), "t", PAGE).unwrap();
        assert!(w.section(*b"BIG!", &[0; PAGE as usize + 1]).is_err());
        w.begin_spanning(*b"SPAN");
        assert!(w.record(&[0; PAGE as usize + 1]).is_err());
        w.end_spanning();
        w.section(*b"SPAN", &[0; 10]).unwrap();
        assert!(w.finish(&info()).is_err());
    }
}
