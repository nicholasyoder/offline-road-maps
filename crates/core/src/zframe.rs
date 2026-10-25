//! zstd frames as the pack uses them: one frame per payload, with the content size and no
//! dictionary ID. JSON sections are ordinary frames with a checksum; tile frames
//! ([`tile_compressor`]) leave out the magic number and the checksum (format 0.3), since
//! `TIDX` says where each one is and the section checksums cover corruption.

use std::io;

use serde::Serialize;
use serde::de::DeserializeOwned;
use zstd::zstd_safe::{CParameter, DCtx, DDict, DParameter, FrameFormat};

use crate::varint::invalid;

/// Refuse frames that claim more than this; a layer payload is at most a few MiB.
pub const MAX_FRAME: u64 = 256 << 20;

/// A compressor for pack frames, with `dict` (empty for none).
pub fn compressor(level: i32, dict: &[u8]) -> io::Result<zstd::bulk::Compressor<'static>> {
    let mut c = zstd::bulk::Compressor::with_dictionary(level, dict)?;
    c.set_parameter(CParameter::DictIdFlag(false))?;
    c.set_parameter(CParameter::ChecksumFlag(true))?;
    Ok(c)
}

/// A compressor for tile frames: magicless, without a checksum.
pub fn tile_compressor(level: i32, dict: &[u8]) -> io::Result<zstd::bulk::Compressor<'static>> {
    let mut c = zstd::bulk::Compressor::with_dictionary(level, dict)?;
    c.set_parameter(CParameter::DictIdFlag(false))?;
    c.set_parameter(CParameter::ChecksumFlag(false))?;
    c.set_parameter(CParameter::Format(FrameFormat::Magicless))?;
    Ok(c)
}

/// A decompression context for tile frames.
pub fn tile_dctx() -> io::Result<DCtx<'static>> {
    let mut d = DCtx::create();
    d.set_parameter(DParameter::Format(FrameFormat::Magicless))
        .map_err(|code| invalid(format!("zstd: {}", zstd::zstd_safe::get_error_name(code))))?;
    Ok(d)
}

/// The content size a magicless frame declares in its header, if it declares one.
pub fn magicless_content_size(frame: &[u8]) -> Option<u64> {
    let &fhd = frame.first()?;
    let single_segment = fhd & 0x20 != 0;
    let dict_id = [0, 1, 2, 4][usize::from(fhd & 3)];
    let fcs = match fhd >> 6 {
        0 if single_segment => 1,
        0 => return None,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let at = 1 + usize::from(!single_segment) + dict_id;
    let b = frame.get(at..at + fcs)?;
    let mut v = [0u8; 8];
    v[..fcs].copy_from_slice(b);
    let v = u64::from_le_bytes(v);
    Some(if fcs == 2 { v + 256 } else { v })
}

/// Decompresses one ordinary frame, checking its declared size and checksum.
pub fn decompress(dctx: &mut DCtx, frame: &[u8], dict: Option<&DDict>) -> io::Result<Vec<u8>> {
    decompress_sized(
        dctx,
        frame,
        dict,
        zstd::zstd_safe::get_frame_content_size(frame)
            .ok()
            .flatten(),
    )
}

/// Decompresses one tile frame with a [`tile_dctx`].
pub fn decompress_tile(dctx: &mut DCtx, frame: &[u8], dict: Option<&DDict>) -> io::Result<Vec<u8>> {
    decompress_sized(dctx, frame, dict, magicless_content_size(frame))
}

fn decompress_sized(
    dctx: &mut DCtx,
    frame: &[u8],
    dict: Option<&DDict>,
    size: Option<u64>,
) -> io::Result<Vec<u8>> {
    let size = size
        .filter(|&n| n <= MAX_FRAME)
        .ok_or_else(|| invalid("zstd frame without a valid content size"))?;
    let mut out = Vec::with_capacity(size as usize);
    let res = match dict {
        Some(d) => dctx.decompress_using_ddict(&mut out, frame, d),
        None => dctx.decompress(&mut out, frame),
    };
    let n =
        res.map_err(|code| invalid(format!("zstd: {}", zstd::zstd_safe::get_error_name(code))))?;
    if n as u64 != size {
        return Err(invalid("zstd frame shorter than its content size"));
    }
    Ok(out)
}

/// A JSON section (`META`, `SCHM`): one zstd frame of JSON.
pub fn to_json_section<T: Serialize>(v: &T) -> io::Result<Vec<u8>> {
    let json = serde_json::to_vec(v).map_err(io::Error::other)?;
    compressor(19, &[])?.compress(&json)
}

pub fn from_json_section<T: DeserializeOwned>(b: &[u8]) -> io::Result<T> {
    let json = decompress(&mut DCtx::create(), b, None)?;
    serde_json::from_slice(&json).map_err(|e| invalid(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_frames_round_trip_without_magic_or_checksum() {
        let dict: Vec<u8> = (0..2000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        // Content sizes stored in 1, 2 and 4 bytes, and an empty payload.
        for n in [0usize, 10, 255, 256, 300, 65_791, 70_000] {
            let payload: Vec<u8> = (0..n).map(|i| (i * 7 % 13) as u8).collect();
            for d in [&[][..], &dict[..]] {
                let frame = tile_compressor(19, d).unwrap().compress(&payload).unwrap();
                assert_ne!(frame.get(..4), Some(&0xFD2F_B528u32.to_le_bytes()[..]));
                assert_eq!(magicless_content_size(&frame), Some(n as u64), "{n}");
                let ddict = (!d.is_empty()).then(|| DDict::create(d));
                let out = decompress_tile(&mut tile_dctx().unwrap(), &frame, ddict.as_ref());
                assert_eq!(out.unwrap(), payload);
            }
        }
        assert_eq!(magicless_content_size(&[]), None);
    }
}
