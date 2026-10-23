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

/// Streams `url` into `dest`, hashing as it goes. Prints a progress line every GiB.
pub fn download(agent: &Agent, url: &str, dest: &Path, label: &str) -> Result<Downloaded> {
    const GIB: u64 = 1 << 30;
    let resp = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = resp.into_body().into_reader();
    let mut out = BufWriter::with_capacity(
        1 << 20,
        File::create(dest).with_context(|| format!("create {}", dest.display()))?,
    );
    let mut sha256 = Sha256::new();
    let mut md5 = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
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
