// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use super::core::{GITHUB_LATEST_RELEASE_API, GithubRelease, ProxyConfig, UPDATE_USER_AGENT};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::io::Read;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const UPDATE_HTTP_TIMEOUT_SECS: u64 = 30;
const DOWNLOAD_CONNECT_TIMEOUT_SECS: u64 = 5;
const DOWNLOAD_IDLE_TIMEOUT_SECS: u64 = 3;
const READ_CHUNK_SIZE: usize = 1024 * 1024;

pub const MAX_UPDATE_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_SHA256SUMS_DOWNLOAD_BYTES: u64 = 1024 * 1024;

fn apply_proxy(
    builder: reqwest::blocking::ClientBuilder,
    proxy: Option<&ProxyConfig>,
) -> Result<reqwest::blocking::ClientBuilder, String> {
    let mut builder = builder.user_agent(UPDATE_USER_AGENT);
    if let Some(url) = proxy.and_then(super::core::proxy_url) {
        let proxy = reqwest::Proxy::all(url).map_err(|err| err.to_string())?;
        builder = builder.proxy(proxy);
    }
    Ok(builder)
}

fn api_client(proxy: Option<&ProxyConfig>) -> Result<reqwest::blocking::Client, String> {
    apply_proxy(
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(UPDATE_HTTP_TIMEOUT_SECS)),
        proxy,
    )?
    .build()
    .map_err(|err| err.to_string())
}

fn download_client(proxy: Option<&ProxyConfig>) -> Result<reqwest::blocking::Client, String> {
    // No total request timeout: large updates may take a long time on slow links.
    // Stall detection is handled by idle timeout while reading the response body.
    apply_proxy(
        reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(DOWNLOAD_CONNECT_TIMEOUT_SECS)),
        proxy,
    )?
    .build()
    .map_err(|err| err.to_string())
}

pub fn fetch_latest_release(proxy: Option<&ProxyConfig>) -> Result<GithubRelease, String> {
    let response = api_client(proxy)?
        .get(GITHUB_LATEST_RELEASE_API)
        .send()
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?;
    response.json().map_err(|err| err.to_string())
}

pub fn download_bytes_with_progress(
    url: &str,
    proxy: Option<&ProxyConfig>,
    max_bytes: u64,
    on_progress: impl FnMut(u64, Option<u64>) + Send + 'static,
) -> Result<Vec<u8>, String> {
    let response = download_client(proxy)?
        .get(url)
        .send()
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?;
    let content_len = response.content_length();
    if content_len.is_some_and(|len| len > max_bytes) {
        return Err(rust_i18n::t!(
            "update.err_download_too_large",
            size = content_len.unwrap_or_default(),
            limit = max_bytes
        )
        .to_string());
    }

    read_with_idle_timeout(
        response,
        max_bytes,
        content_len,
        Duration::from_secs(DOWNLOAD_IDLE_TIMEOUT_SECS),
        on_progress,
    )
}

enum ReadChunk {
    Data(Vec<u8>),
    Done,
}

fn read_with_idle_timeout(
    reader: impl Read + Send + 'static,
    max_bytes: u64,
    content_len: Option<u64>,
    idle_timeout: Duration,
    mut on_progress: impl FnMut(u64, Option<u64>) + Send + 'static,
) -> Result<Vec<u8>, String> {
    let (chunk_tx, chunk_rx) = crossbeam_channel::bounded(1);
    let reader = spawn_download_reader(reader, chunk_tx)?;

    let result = collect_download_chunks(
        chunk_rx,
        max_bytes,
        content_len,
        idle_timeout,
        &mut on_progress,
    );
    finish_download_reader(reader, result)
}

fn spawn_download_reader(
    mut reader: impl Read + Send + 'static,
    chunk_tx: Sender<Result<ReadChunk, String>>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("siv-update-download".to_string())
        .spawn(move || {
            let mut buf = [0u8; READ_CHUNK_SIZE];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = chunk_tx.send(Ok(ReadChunk::Done));
                        break;
                    }
                    Ok(n) => {
                        if chunk_tx
                            .send(Ok(ReadChunk::Data(buf[..n].to_vec())))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = chunk_tx.send(Err(err.to_string()));
                        break;
                    }
                }
            }
        })
        .map_err(|err| err.to_string())
}

fn collect_download_chunks(
    chunk_rx: Receiver<Result<ReadChunk, String>>,
    max_bytes: u64,
    content_len: Option<u64>,
    idle_timeout: Duration,
    on_progress: &mut impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(content_len.unwrap_or(0).min(max_bytes) as usize);
    loop {
        match chunk_rx.recv_timeout(idle_timeout) {
            Ok(Ok(ReadChunk::Done)) => return Ok(bytes),
            Ok(Ok(ReadChunk::Data(chunk))) => {
                bytes.extend_from_slice(&chunk);
                if bytes.len() as u64 > max_bytes {
                    return Err(rust_i18n::t!(
                        "update.err_download_exceeded_limit",
                        limit = max_bytes
                    )
                    .to_string());
                }
                on_progress(bytes.len() as u64, content_len);
            }
            Ok(Err(err)) => return Err(err),
            Err(RecvTimeoutError::Timeout) => {
                return Err(rust_i18n::t!("update.err_download_stalled").to_string());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(rust_i18n::t!("update.err_download_stalled").to_string());
            }
        }
    }
}

fn finish_download_reader(
    reader: JoinHandle<()>,
    result: Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    if result.is_ok() {
        let _ = reader.join();
    }
    // On error or idle timeout the reader may still be blocked on network I/O.
    // Drop the join handle without joining so the caller is not stalled.
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ChannelReader {
        rx: Receiver<Vec<u8>>,
        reads: Arc<AtomicUsize>,
    }

    impl Read for ChannelReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            match self.rx.recv() {
                Ok(data) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok(n)
                }
                Err(_) => Ok(0),
            }
        }
    }

    #[test]
    fn download_idle_timeout_triggers_when_reader_stalls() {
        let (data_tx, data_rx) = crossbeam_channel::unbounded();
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = ChannelReader {
            rx: data_rx,
            reads: Arc::clone(&reads),
        };
        data_tx.send(b"hello".to_vec()).expect("seed first chunk");

        let err = read_with_idle_timeout(reader, 1024, None, Duration::from_millis(200), |_, _| {})
            .expect_err("stall should fail with idle timeout");

        assert_eq!(
            err,
            rust_i18n::t!("update.err_download_stalled").to_string()
        );
        assert!(reads.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn download_collects_all_chunks_before_idle_timeout() {
        let (data_tx, data_rx) = crossbeam_channel::unbounded();
        let reader = ChannelReader {
            rx: data_rx,
            reads: Arc::new(AtomicUsize::new(0)),
        };
        data_tx.send(b"abc".to_vec()).expect("first chunk");
        data_tx.send(b"def".to_vec()).expect("second chunk");
        drop(data_tx);

        let bytes = read_with_idle_timeout(
            reader,
            1024,
            Some(6),
            Duration::from_secs(1),
            |received, total| {
                assert_eq!(total, Some(6));
                assert!(received <= 6);
            },
        )
        .expect("download should finish");

        assert_eq!(bytes, b"abcdef");
    }
}
