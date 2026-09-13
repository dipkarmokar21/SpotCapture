use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::{Path, PathBuf}};
use crate::{auth, events::status, output::Metadata};

pub struct TrackInfo {
    pub tags: Metadata,
    pub duration_ms: Option<u64>,
    pub artwork: Option<Artwork>,
}

impl TrackInfo {
    pub fn fallback(id: &str) -> Self {
        Self { tags: Metadata { title:Some(id.into()), ..Default::default() }, duration_ms:None, artwork:None }
    }
}

pub struct Artwork(pub PathBuf);
impl Drop for Artwork {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

pub fn track_id(input: &str) -> Result<String> {
    let input = input.trim();
    let id = if let Some(id) = input.strip_prefix("spotify:track:") {
        id.to_string()
    } else if input.starts_with("https://") {
        let url = url::Url::parse(input)?;
        ensure!(url.host_str() == Some("open.spotify.com"), "Use an open.spotify.com track link");
        let segments: Vec<_> = url.path_segments().context("Invalid track link")?.collect();
        match segments.as_slice() {
            ["track", id] => id.to_string(),
            [locale, "track", id] if locale.starts_with("intl-") => id.to_string(),
            _ => anyhow::bail!("Paste a single track link; playlist/album links are not supported yet"),
        }
    } else { input.to_string() };
    ensure!(id.len() == 22 && id.bytes().all(|c| c.is_ascii_alphanumeric()), "Expected a Spotify track URL, URI, or 22-character track ID");
    Ok(id)
}

pub fn safe_filename(value: &str) -> String {
    // Linux limits a filename component in bytes. Leave room for a second
    // sanitized artist/title component plus separators and a file extension.
    let mut text = String::new();
    for c in value.chars() {
        let c = if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c };
        if text.len() + c.len_utf8() > 100 { break; }
        text.push(c);
    }
    let trimmed = text.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() { "Track".into() } else { trimmed.into() }
}

pub async fn fetch(id: &str, access_token: &str, cache: &Path) -> TrackInfo {
    let mut info = TrackInfo::fallback(id);
    match fetch_inner(id, access_token).await {
        Ok(data) => {
            if let Some(title) = data["name"].as_str().filter(|title| !title.is_empty()) {
                info.tags.title = Some(title.to_owned());
            }
            info.tags.artist = names(&data["artists"]);
            info.tags.album = data["album"]["name"].as_str().map(str::to_owned);
            info.tags.date = data["album"]["release_date"].as_str().map(str::to_owned);
            info.tags.track = data["track_number"].as_u64().map(|v|v.to_string());
            info.tags.disc = data["disc_number"].as_u64().map(|v|v.to_string());
            info.duration_ms = data["duration_ms"].as_u64();
            if let Some(url) = data["album"]["images"].as_array().and_then(|a|a.first()).and_then(|i|i["url"].as_str()) {
                match artwork(url, cache).await {
                    Ok(art) => info.artwork = Some(art),
                    Err(_) => status("Artwork could not be downloaded; continuing with audio and text tags"),
                }
            }
        }
        Err(error) => status(format!("Web API metadata unavailable ({error}); using decoder metadata")),
    }
    info
}

fn names(value: &Value) -> Option<String> {
    let names: Vec<_> = value.as_array()?.iter().filter_map(|v|v["name"].as_str()).collect();
    if names.is_empty() { None } else { Some(names.join(", ")) }
}

async fn fetch_inner(id: &str, token: &str) -> Result<Value> {
    let response = auth::http_client()?.get(format!("https://api.spotify.com/v1/tracks/{id}"))
        .bearer_auth(token).send().await.context("network request failed")?;
    if response.status().as_u16() == 429 {
        let wait = response.headers().get("retry-after").and_then(|h|h.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        match wait {
            Some(wait) => anyhow::bail!("rate limited; retry after {wait} seconds"),
            None => anyhow::bail!("rate limited; retry later"),
        }
    }
    ensure!(response.status().is_success(), "HTTP {}", response.status());
    Ok(response.json().await?)
}

pub async fn artwork(url: &str, cache: &Path) -> Result<Artwork> {
    let parsed = url::Url::parse(url)?;
    ensure!(parsed.scheme() == "https" && parsed.host_str() == Some("i.scdn.co"), "Unexpected artwork host");
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20)).build()?;
    let mut response = client.get(parsed).send().await?;
    ensure!(response.status().is_success(), "Artwork download failed");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= 8 * 1024 * 1024, "Artwork exceeds 8 MiB");
        bytes.extend_from_slice(&chunk);
    }
    ensure!(bytes.starts_with(&[0xff, 0xd8]) || bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "Unsupported artwork image");
    let path = cache.join(format!(".artwork-{}-{}.image", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    let art = Artwork(path);
    file.write_all(&bytes)?;
    Ok(art)
}

pub async fn extract_tracks(url: &str) -> Result<()> {
    let url_parsed = url::Url::parse(url).context("Invalid URL format")?;
    ensure!(url_parsed.host_str() == Some("open.spotify.com"), "Must be an open.spotify.com URL");
    
    let path = url_parsed.path();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (kind, id) = match parts.as_slice() {
        ["album", id] | [_, "album", id] => ("albums", *id),
        ["playlist", id] | [_, "playlist", id] => ("playlists", *id),
        _ => anyhow::bail!("Unsupported URL. Must be an album or playlist."),
    };

    let cache = crate::auth::cache_dir(None).unwrap_or_default();
    let tokens_opt = crate::auth::load(&cache, None).await.ok();
    
    let mut api_success = false;
    
    if let Some(tokens) = tokens_opt {
        let client = reqwest::Client::new();
        let mut next_url = Some(format!("https://api.spotify.com/v1/{kind}/{id}/tracks?limit=50"));
        let mut success_in_loop = true;
        
        while let Some(current_url) = next_url {
            let res_result = client.get(&current_url)
                .header("Authorization", format!("Bearer {}", tokens.access_token))
                .send().await;
                
            let res = match res_result {
                Ok(r) => r,
                Err(_) => { success_in_loop = false; break; }
            };
                
            if !res.status().is_success() {
                success_in_loop = false; break;
            }
            
            let json_result: Result<serde_json::Value, _> = res.json().await;
            let json = match json_result {
                Ok(j) => j,
                Err(_) => { success_in_loop = false; break; }
            };
            
            let items = match json.get("items").and_then(|i| i.as_array()) {
                Some(i) => i,
                None => { success_in_loop = false; break; }
            };
            
            for item in items {
                let track = if kind == "playlists" {
                    item.get("track").unwrap_or(item)
                } else {
                    item
                };
                
                if let Some(track_id) = track.get("id").and_then(|id| id.as_str()) {
                    let name = track.get("name").and_then(|n| n.as_str()).unwrap_or("Unknown Track");
                    
                    let artist_name = track.get("artists")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.first())
                        .and_then(|a| a.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("Unknown Artist");
                        
                    println!("spotify:track:{}|{} - {}", track_id, name, artist_name);
                }
            }
            
            next_url = json.get("next").and_then(|n| n.as_str()).map(|s| s.to_string());
        }
        
        if success_in_loop {
            api_success = true;
        }
    }

    if api_success {
        return Ok(());
    }

    // Fallback: Web scraping
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()?;
        
    let body = client.get(url).send().await?.text().await?;
    
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let b64_re = regex::Regex::new(r">eyJ([^<]+)</").unwrap();
    
    let mut tracks = std::collections::HashMap::new();
    
    fn extract_from_json(val: &serde_json::Value, tracks: &mut std::collections::HashMap<String, String>) {
        if let Some(obj) = val.as_object() {
            let mut track_id = None;
            let mut track_name = None;
            
            if let Some(uri) = obj.get("uri").and_then(|u| u.as_str()) {
                if uri.starts_with("spotify:track:") {
                    track_id = Some(uri[14..].to_string());
                }
            } else if let Some(id) = obj.get("id").and_then(|i| i.as_str()) {
                if id.len() == 22 {
                    track_id = Some(id.to_string());
                }
            }
            
            if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
                track_name = Some(name.to_string());
            }
            
            if let (Some(id), Some(name)) = (track_id, track_name) {
                tracks.insert(id, name);
            }
            
            for v in obj.values() {
                extract_from_json(v, tracks);
            }
        } else if let Some(arr) = val.as_array() {
            for v in arr {
                extract_from_json(v, tracks);
            }
        }
    }
    
    for cap in b64_re.captures_iter(&body) {
        if let Some(b64_data) = cap.get(1) {
            let mut full_b64 = format!("eyJ{}", b64_data.as_str());
            while full_b64.len() % 4 != 0 {
                full_b64.push('=');
            }
            if let Ok(decoded_bytes) = STANDARD.decode(&full_b64) {
                let decoded_str = String::from_utf8_lossy(&decoded_bytes);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&decoded_str) {
                    extract_from_json(&json, &mut tracks);
                }
            }
        }
    }
    
    let id_re = regex::Regex::new(r"spotify:track:([a-zA-Z0-9]{22})|spotify\.com/track/([a-zA-Z0-9]{22})").unwrap();
    for cap in id_re.captures_iter(&body) {
        if let Some(id) = cap.get(1).or_else(|| cap.get(2)) {
            let id_str = id.as_str().to_string();
            if !tracks.contains_key(&id_str) {
                tracks.insert(id_str, "Unknown Track".to_string());
            }
        }
    }
    
    for (id, name) in tracks {
        println!("spotify:track:{}|{}", id, name);
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_tracks_rejects_other_hosts_and_collections() {
        let id = "4uLU6hMCjMI75M1A2tKUQC";
        assert_eq!(track_id(&format!("https://open.spotify.com/intl-bn/track/{id}?si=abc")).unwrap(), id);
        assert_eq!(track_id(&format!("spotify:track:{id}")).unwrap(), id);
        assert!(track_id(&format!("https://evil.example/track/{id}")).is_err());
        assert!(track_id(&format!("https://open.spotify.com/album/{id}")).is_err());
        assert!(track_id("../../secret").is_err());
    }
    #[test]
    fn filenames_cannot_escape_output_dir() {
        assert_eq!(safe_filename("../a/b\\c\n"), "_a_b_c_");
        assert_eq!(safe_filename("..."), "Track");
        assert_eq!(safe_filename("বাংলা গান"), "বাংলা গান");
    }
    #[test]
    fn unicode_filenames_fit_linux_component_limit_without_splitting_characters() {
        for input in ["গান".repeat(100), "🎵".repeat(100), "a".repeat(99) + "🎵"] {
            let name = safe_filename(&input);
            assert!(name.len() <= 100);
            assert!(input.starts_with(&name));
            let component = format!("{name} - {name} (9999).flac");
            assert!(component.len() <= 255);
        }
    }
}
