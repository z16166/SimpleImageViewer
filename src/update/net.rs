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
use std::time::Duration;

const UPDATE_HTTP_TIMEOUT_SECS: u64 = 30;

fn client(proxy: Option<&ProxyConfig>) -> Result<reqwest::blocking::Client, String> {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(UPDATE_HTTP_TIMEOUT_SECS))
        .user_agent(UPDATE_USER_AGENT);
    if let Some(url) = proxy.and_then(super::core::proxy_url) {
        let proxy = reqwest::Proxy::all(url).map_err(|err| err.to_string())?;
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(|err| err.to_string())
}

pub fn fetch_latest_release(proxy: Option<&ProxyConfig>) -> Result<GithubRelease, String> {
    let response = client(proxy)?
        .get(GITHUB_LATEST_RELEASE_API)
        .send()
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?;
    response.json().map_err(|err| err.to_string())
}

pub fn download_bytes(url: &str, proxy: Option<&ProxyConfig>) -> Result<Vec<u8>, String> {
    let bytes = client(proxy)?
        .get(url)
        .send()
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?
        .bytes()
        .map_err(|err| err.to_string())?;
    Ok(bytes.to_vec())
}
