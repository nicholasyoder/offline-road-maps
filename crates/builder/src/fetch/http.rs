//! Blocking HTTP helpers: small text/JSON requests and hashed file downloads.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use md5::Md5;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use ureq::http::Response;
use ureq::{Agent, Body};

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

/// `<dest>.meta`: the validator (`ETag` or `Last-Modified`) of the response `dest` was started
/// from, so a resume can ask for the rest of the same version.
fn meta_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".meta");
    PathBuf::from(p)
}

/// The response's strong `ETag`, else its `Last-Modified`: what `If-Range` accepts.
fn validator(r: &Response<Body>) -> Option<String> {
    let h = |name| r.headers().get(name).and_then(|v| v.to_str().ok());
    h("etag")
        .filter(|e| !e.starts_with("W/"))
        .or_else(|| h("last-modified"))
        .map(str::to_owned)
}

/// Streams `url` into `dest`, hashing as it goes. Prints a progress line every GiB. If `dest`
/// holds part of the file from an earlier attempt, asks the server for the rest, with
/// `If-Range` so that a file changed on the server since then is fetched whole again.
pub fn download(agent: &Agent, url: &str, dest: &Path, label: &str) -> Result<Downloaded> {
    const GIB: u64 = 1 << 30;
    let mut sha256 = Sha256::new();
    let mut md5 = Md5::new();
    let mut size = 0u64;
    let meta = meta_path(dest);
    let partial = std::fs::metadata(dest).map_or(0, |m| m.len());
    // Without a validator the part could be of any version, so it is not resumed.
    let since = std::fs::read_to_string(&meta)
        .ok()
        .filter(|v| !v.is_empty());
    let mut resp = None;
    if let (true, Some(since)) = (partial > 0, since) {
        match agent
            .get(url)
            .header("Range", format!("bytes={partial}-"))
            .header("If-Range", since)
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
        None => {
            let r = agent
                .get(url)
                .call()
                .with_context(|| format!("GET {url}"))?;
            match validator(&r) {
                Some(v) => std::fs::write(&meta, v),
                None => std::fs::remove_file(&meta).or_else(not_found_ok),
            }
            .with_context(|| format!("write {}", meta.display()))?;
            r
        }
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
    std::fs::remove_file(&meta)
        .or_else(not_found_ok)
        .with_context(|| format!("remove {}", meta.display()))?;
    Ok(Downloaded {
        size,
        sha256: hex::encode(sha256.finalize()),
        md5: hex::encode(md5.finalize()),
    })
}

fn not_found_ok(e: std::io::Error) -> std::io::Result<()> {
    match e.kind() {
        std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A request's `Range` and `If-Range`.
    type Seen = (Option<String>, Option<String>);

    /// Serves `body` with `etag`, honouring `Range` only when `If-Range` matches, and records
    /// each request's `Range` and `If-Range`.
    struct Server {
        url: String,
        file: Arc<Mutex<(Vec<u8>, String)>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    fn serve(body: &[u8], etag: &str) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/f", listener.local_addr().unwrap());
        let file = Arc::new(Mutex::new((body.to_vec(), etag.to_owned())));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (f, s) = (file.clone(), seen.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let mut conn = conn.unwrap();
                let mut lines = BufReader::new(conn.try_clone().unwrap()).lines();
                let (mut range, mut if_range) = (None, None);
                for line in lines.by_ref() {
                    let line = line.unwrap();
                    if line.is_empty() {
                        break;
                    }
                    let (k, v) = line.split_once(": ").unwrap_or((&line, ""));
                    match k.to_ascii_lowercase().as_str() {
                        "range" => range = Some(v.to_owned()),
                        "if-range" => if_range = Some(v.to_owned()),
                        _ => {}
                    }
                }
                s.lock().unwrap().push((range.clone(), if_range.clone()));
                let (body, etag) = f.lock().unwrap().clone();
                let from = range
                    .filter(|_| if_range.as_deref() == Some(etag.as_str()))
                    .and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok());
                let head = match from {
                    Some(from) => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{}\r\n",
                        body.len() - 1,
                        body.len()
                    ),
                    None => "HTTP/1.1 200 OK\r\n".to_owned(),
                };
                let part = &body[from.unwrap_or(0)..];
                let head = format!(
                    "{head}ETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    part.len()
                );
                conn.write_all(head.as_bytes()).unwrap();
                conn.write_all(part).unwrap();
            }
        });
        Server { url, file, seen }
    }

    #[test]
    fn resumes_only_the_same_version() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.part");
        let server = serve(b"0123456789", "\"v1\"");
        let agent = agent();

        // A whole download leaves no .meta behind.
        let d = download(&agent, &server.url, &dest, "f").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
        assert_eq!(d.size, 10);
        assert!(!meta_path(&dest).exists());

        // An interrupted one of the same version resumes.
        let resume = (Some("bytes=4-".to_owned()), Some("\"v1\"".to_owned()));
        std::fs::write(&dest, b"0123").unwrap();
        std::fs::write(meta_path(&dest), "\"v1\"").unwrap();
        server.seen.lock().unwrap().clear();
        let d = download(&agent, &server.url, &dest, "f").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
        assert_eq!(d.sha256, hex::encode(Sha256::digest(b"0123456789")));
        assert_eq!(*server.seen.lock().unwrap(), std::slice::from_ref(&resume));

        // After the server's copy changed, it starts over rather than joining the two.
        std::fs::write(&dest, b"0123").unwrap();
        std::fs::write(meta_path(&dest), "\"v1\"").unwrap();
        *server.file.lock().unwrap() = (b"abcdefghijkl".to_vec(), "\"v2\"".to_owned());
        server.seen.lock().unwrap().clear();
        download(&agent, &server.url, &dest, "f").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abcdefghijkl");
        assert_eq!(*server.seen.lock().unwrap(), [resume, (None, None)]);

        // A part with no validator is not resumed.
        std::fs::write(&dest, b"abcd").unwrap();
        server.seen.lock().unwrap().clear();
        download(&agent, &server.url, &dest, "f").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abcdefghijkl");
        assert_eq!(*server.seen.lock().unwrap(), [(None, None)]);
    }
}
