//! LEB128 varints and zigzag, as used by the tile, address and search encodings.

use std::io;

pub fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn len(mut v: u64) -> u64 {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

pub fn zigzag(v: i32) -> u64 {
    u64::from(((v << 1) ^ (v >> 31)) as u32)
}

pub fn unzigzag(v: u64) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

pub(crate) fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Reads varints and raw bytes from a borrowed buffer.
pub struct Reader<'a> {
    pub b: &'a [u8],
    pub i: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    pub fn varint(&mut self) -> io::Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(&byte) = self.b.get(self.i) else {
                return Err(invalid("truncated varint"));
            };
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(v);
            }
        }
        Err(invalid("varint too long"))
    }

    pub fn zvarint(&mut self) -> io::Result<i32> {
        Ok(unzigzag(self.varint()?))
    }

    pub fn bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let s = self
            .i
            .checked_add(n)
            .and_then(|end| self.b.get(self.i..end))
            .ok_or_else(|| invalid("truncated data"))?;
        self.i += n;
        Ok(s)
    }

    /// A count that will size an allocation: each item takes at least one byte, so a count
    /// larger than the bytes left is corrupt.
    pub fn count(&mut self) -> io::Result<usize> {
        let n = self.varint()?;
        if n > (self.b.len() - self.i) as u64 {
            return Err(invalid("count larger than the data"));
        }
        Ok(n as usize)
    }

    pub fn at_end(&self) -> bool {
        self.i == self.b.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut out = Vec::new();
        let vals = [0, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX];
        for v in vals {
            put(&mut out, v);
        }
        assert_eq!(out.len() as u64, vals.iter().map(|&v| len(v)).sum::<u64>());
        let mut r = Reader::new(&out);
        for v in vals {
            assert_eq!(r.varint().unwrap(), v);
        }
        assert!(r.at_end() && r.varint().is_err());
        for v in [0, -1, 1, i32::MIN, i32::MAX] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }
}
