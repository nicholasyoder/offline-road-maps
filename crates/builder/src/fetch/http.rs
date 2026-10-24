//! Blocking HTTP helpers: small text/JSON requests and hashed file downloads.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use md5::Md5;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use ureq::Agent;

/// Listings and metadata are small; anything bigger than this is a bug or a wrong URL.
const SMALL_BODY_LIMIT: u64 = 64 * 1024 * 1024;

pub fn agent() -> Agent {
    Agent::config_builder()
        .user_agent(concat!(
            "omp-builder/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/nicholasyoder/offline-road-maps)"
        ))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .timeout_per_read(Some(Duration::from_secs(120)))
        .build()
        .into()
}

pub fn get_string(agent: &Agent, url: &str) -> Result<String> {
    agent
        .get(url)
        .call()
        .and_then(|mut r| {
            r.body_mut()
                .with_config()
                .limit(SMALL_BODY_LIMIT)
                .read_to_string()
        })
        .with_context(|| format!("GET {url}"))
}

pub fn get_json<T: DeserializeOwned>(agent: &Agent, url: &str) -> Result<T> {
    agent
        .get(url)
        .call()
        .and_then(|mut r| {
            r.body_mut()
                .with_config()
                .limit(SMALL_BODY_LIMIT)
                .read_json()
        })
        .with_context(|| format!("GET {url}"))
}

pub struct Downloaded {
    pub size: u64,
    pub sha256: String,
    pub md5: String,
}

/// Streams `url` into `dest`, hashing as it goes. Prints a progress line every GiB. If `dest`
/// holds part of the file from an earlier attempt, asks the server for the rest.
pub fn download(agent: &Agent, url: &str, dest: &Path, label: &str) -> Result<Downloaded> {
    const GIB: u64 = 1 << 30;
    let mut sha256 = Sha256::new();
    let mut md5 = Md5::new();
    let mut size = 0u64;
    let partial = std::fs::metadata(dest).map_or(0, |m| m.len());
    let mut resp = None;
    if partial > 0 {
        match agent
            .get(url)
            .header("Range", format!("bytes={partial}-"))
            .call()
        {
            Ok(r)
                if r.status().as_u16() == 206
                    && r.headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.starts_with(&format!("bytes {partial}-"))) =>
            {
                let mut f = File::open(dest).with_context(|| format!("open {}", dest.display()))?;
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    sha256.update(&buf[..n]);
                    md5.update(&buf[..n]);
                    size += n as u64;
                }
                println!(
                    "  {label}: resuming at {:.1} MiB",
                    size as f64 / (1 << 20) as f64
                );
                resp = Some(r);
            }
            // 416 (the partial file is already complete or wrong), or a server that ignores
            // ranges: start over.
            Ok(_) | Err(ureq::Error::StatusCode(416)) => {}
            Err(e) => return Err(e).with_context(|| format!("GET {url}")),
        }
    }
    let resp = match resp {
        Some(r) => r,
        None => agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?,
    };
    let file = if size > 0 {
        std::fs::OpenOptions::new().append(true).open(dest)
    } else {
        File::create(dest)
    };
    let mut reader = resp.into_body().into_reader();
    let mut out = BufWriter::with_capacity(
        1 << 20,
        file.with_context(|| format!("open {}", dest.display()))?,
    );
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("read {url}"))?;
        if n == 0 {
            break;
        }
        sha256.update(&buf[..n]);
        md5.update(&buf[..n]);
        out.write_all(&buf[..n])
            .with_context(|| format!("write {}", dest.display()))?;
        if (size + n as u64) / GIB > size / GIB {
            println!("  {label}: {} GiB", (size + n as u64) / GIB);
        }
        size += n as u64;
    }
    out.into_inner()
        .map_err(|e| e.into_error())
        .and_then(|f| f.sync_all())
        .with_context(|| format!("flush {}", dest.display()))?;
    Ok(Downloaded {
        size,
        sha256: hex::encode(sha256.finalize()),
        md5: hex::encode(md5.finalize()),
    })
}
