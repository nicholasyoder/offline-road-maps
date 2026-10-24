//! zstd frames as the pack uses them: one frame per payload, with the content size and
//! checksum, and no dictionary ID.

use std::io;

use serde::Serialize;
use serde::de::DeserializeOwned;
use zstd::zstd_safe::{CParameter, DCtx, DDict};

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

/// Decompresses one frame, checking its declared size and checksum.
pub fn decompress(dctx: &mut DCtx, frame: &[u8], dict: Option<&DDict>) -> io::Result<Vec<u8>> {
    let size = zstd::zstd_safe::get_frame_content_size(frame)
        .ok()
        .flatten()
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
