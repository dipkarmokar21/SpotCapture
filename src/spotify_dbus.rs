//! Control Spotify Desktop via MPRIS2 D-Bus.
//!
//! Uses `dbus-send` subprocess calls so no native D-Bus library is needed.
//! Spotify must be running and visible on the session bus as
//! `org.mpris.MediaPlayer2.spotify`.

use anyhow::{Context, Result, bail, ensure};
use std::process::Command;
use std::time::Duration;

const DEST: &str = "org.mpris.MediaPlayer2.spotify";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Metadata retrieved from Spotify via MPRIS.
#[derive(Clone, Debug, Default)]
pub struct MprisMetadata {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub duration_us: u64,
}

impl MprisMetadata {
    pub fn duration_ms(&self) -> u64 {
        self.duration_us / 1000
    }

    pub fn spotify_uri(&self) -> Option<String> {
        // track_id looks like "/com/spotify/track/XXXXX"
        let id = self.track_id.strip_prefix("/com/spotify/track/")?;
        if id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            Some(format!("spotify:track:{id}"))
        } else {
            None
        }
    }

    pub fn track_id_short(&self) -> Option<&str> {
        self.track_id.strip_prefix("/com/spotify/track/")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

/// Check if Spotify is running and accessible via D-Bus.
pub fn is_spotify_running() -> bool {
    Command::new("dbus-send")
        .args([
            "--print-reply", "--dest=org.freedesktop.DBus",
            "/org/freedesktop/DBus", "org.freedesktop.DBus.NameHasOwner",
            &format!("string:{DEST}"),
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("boolean true"))
        .unwrap_or(false)
}

/// Get the current playback status.
pub fn get_playback_status() -> Result<PlaybackStatus> {
    let value = get_property("PlaybackStatus")?;
    match extract_string_variant(&value).as_str() {
        "Playing" => Ok(PlaybackStatus::Playing),
        "Paused" => Ok(PlaybackStatus::Paused),
        _ => Ok(PlaybackStatus::Stopped),
    }
}

/// Get the current playback position in microseconds.
pub fn get_position() -> Result<u64> {
    let value = get_property("Position")?;
    extract_int64(&value)
}

/// Get metadata of the currently loaded track.
pub fn get_metadata() -> Result<MprisMetadata> {
    let output = dbus_send_raw(&[
        "--print-reply", &format!("--dest={DEST}"), PATH,
        &format!("{PROPS}.Get"),
        &format!("string:{PLAYER}"), "string:Metadata",
    ])?;
    parse_metadata(&output)
}

/// Open and play a specific Spotify track URI (e.g. spotify:track:XXXXX).
/// This opens the track from the beginning.
pub fn open_uri(uri: &str) -> Result<()> {
    ensure!(
        uri.starts_with("spotify:track:") || uri.starts_with("https://open.spotify.com/"),
        "URI must be a Spotify track URI or URL"
    );
    let output = Command::new("dbus-send")
        .args([
            "--print-reply", &format!("--dest={DEST}"), PATH,
            &format!("{PLAYER}.OpenUri"),
            &format!("string:{uri}"),
        ])
        .output()
        .context("Failed to send OpenUri via D-Bus")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Spotify OpenUri failed: {stderr}");
    }
    Ok(())
}

/// Pause playback.
pub fn pause() -> Result<()> {
    player_method("Pause")
}

/// Resume playback.
pub fn play() -> Result<()> {
    player_method("Play")
}

/// Seek to the start of the current track.
pub fn seek_to_start() -> Result<()> {
    // SetPosition requires the track object path and position in microseconds.
    let meta = get_metadata()?;
    let output = Command::new("dbus-send")
        .args([
            "--print-reply", &format!("--dest={DEST}"), PATH,
            &format!("{PLAYER}.SetPosition"),
            &format!("objpath:{}", meta.track_id),
            "int64:0",
        ])
        .output()
        .context("Failed to seek to start")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Seek to start failed: {stderr}");
    }
    Ok(())
}

/// Wait for Spotify to start playing a specific track (polling).
pub async fn wait_for_playing(expected_track_id_short: &str, timeout: Duration) -> Result<()> {
    let start = tokio::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!("Timed out waiting for Spotify to start playing the track");
        }
        if let Ok(status) = get_playback_status() {
            if status == PlaybackStatus::Playing {
                if let Ok(meta) = get_metadata() {
                    if let Some(current_id) = meta.track_id_short() {
                        if current_id == expected_track_id_short {
                            return Ok(());
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn wait_for_track_end(duration_us: u64, cancel: &tokio::sync::watch::Receiver<bool>) -> Result<()> {
    let duration_ms = duration_us / 1000;
    let mut last_position_ms: u64 = 0;
    let mut max_position_ms: u64 = 0;
    let mut stall_count: u32 = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(500));

    loop {
        tick.tick().await;

        if *cancel.borrow() {
            bail!("Capture cancelled");
        }

        let pos = get_position().unwrap_or(0) / 1000;
        max_position_ms = max_position_ms.max(pos);

        let status = get_playback_status().unwrap_or(PlaybackStatus::Stopped);
        if status == PlaybackStatus::Paused || status == PlaybackStatus::Stopped {
            // Check if we reached near the end before it paused/stopped
            if duration_ms > 0 && max_position_ms >= duration_ms.saturating_sub(2000) {
                // Track finished
                return Ok(());
            }
            if status == PlaybackStatus::Stopped {
                return Ok(());
            }
            // If paused mid-track, wait
            stall_count += 1;
            if stall_count > 120 {
                bail!("Spotify paused for over 60 seconds; capture stopped");
            }
            continue;
        }

        stall_count = 0;

        // Check if we've reached (or passed) the end
        if duration_ms > 0 && max_position_ms >= duration_ms.saturating_sub(500) {
            // Wait a tiny bit more for the actual audio to flush
            tokio::time::sleep(Duration::from_millis(800)).await;
            return Ok(());
        }

        // Detect if position stopped advancing (track ended without status change)
        if pos > 0 && pos == last_position_ms {
            stall_count += 1;
            if stall_count > 10 {
                // Position hasn't moved in 5 seconds while "Playing"
                return Ok(());
            }
        } else {
            stall_count = 0;
        }
        last_position_ms = pos;
    }
}

// ─── Internal helpers ───

fn player_method(method: &str) -> Result<()> {
    let output = Command::new("dbus-send")
        .args([
            "--print-reply", &format!("--dest={DEST}"), PATH,
            &format!("{PLAYER}.{method}"),
        ])
        .output()
        .with_context(|| format!("Failed to call {method} via D-Bus"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Spotify {method} failed: {stderr}");
    }
    Ok(())
}

fn get_property(name: &str) -> Result<String> {
    dbus_send_raw(&[
        "--print-reply", &format!("--dest={DEST}"), PATH,
        &format!("{PROPS}.Get"),
        &format!("string:{PLAYER}"), &format!("string:{name}"),
    ])
}

fn dbus_send_raw(args: &[&str]) -> Result<String> {
    let output = Command::new("dbus-send")
        .args(args)
        .output()
        .context("dbus-send failed; is dbus-send installed?")?;
    ensure!(output.status.success(), "D-Bus call failed: {}",
        String::from_utf8_lossy(&output.stderr).trim());
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn extract_string_variant(text: &str) -> String {
    // Look for: variant  string "VALUE"
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("variant") {
            if let Some(rest) = trimmed.strip_prefix("variant") {
                let rest = rest.trim();
                if let Some(rest) = rest.strip_prefix("string") {
                    let rest = rest.trim();
                    if let Some(value) = rest.strip_prefix('"') {
                        if let Some(value) = value.strip_suffix('"') {
                            return value.to_string();
                        }
                    }
                }
            }
        }
        // Also handle: string "VALUE" on its own line after variant
        if trimmed.starts_with("string \"") {
            if let Some(value) = trimmed.strip_prefix("string \"") {
                if let Some(value) = value.strip_suffix('"') {
                    return value.to_string();
                }
            }
        }
    }
    String::new()
}

fn extract_int64(text: &str) -> Result<u64> {
    for line in text.lines() {
        let mut trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("variant") {
            trimmed = rest.trim();
        }
        for prefix in ["int64 ", "uint64 "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return rest.trim().parse::<u64>().context("Invalid integer in D-Bus response");
            }
        }
    }
    bail!("No integer value found in D-Bus response")
}

fn parse_metadata(text: &str) -> Result<MprisMetadata> {
    let mut meta = MprisMetadata::default();

    // Parse the D-Bus metadata dict.
    // We look for key-value pairs in the dict entries.
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("string \"mpris:trackid\"") || line.contains("\"mpris:trackid\"") {
            if let Some(val) = find_next_string_value(&lines, i + 1) {
                meta.track_id = val;
            }
        } else if line.starts_with("string \"mpris:length\"") || line.contains("\"mpris:length\"") {
            if let Some(val) = find_next_uint64_value(&lines, i + 1) {
                meta.duration_us = val;
            }
        } else if line.starts_with("string \"mpris:artUrl\"") || line.contains("\"mpris:artUrl\"") {
            if let Some(val) = find_next_string_value(&lines, i + 1) {
                meta.art_url = val;
            }
        } else if line.starts_with("string \"xesam:title\"") || line.contains("\"xesam:title\"") {
            if let Some(val) = find_next_string_value(&lines, i + 1) {
                meta.title = val;
            }
        } else if line.starts_with("string \"xesam:album\"") || line.contains("\"xesam:album\"") {
            if let Some(val) = find_next_string_value(&lines, i + 1) {
                meta.album = val;
            }
        } else if line.starts_with("string \"xesam:artist\"") || line.contains("\"xesam:artist\"") {
            // Artists is an array of strings
            if let Some(val) = find_next_string_value(&lines, i + 1) {
                meta.artist = val;
            }
        }
        i += 1;
    }

    ensure!(!meta.track_id.is_empty(), "No track ID in Spotify metadata");
    Ok(meta)
}

fn find_next_string_value(lines: &[&str], start: usize) -> Option<String> {
    for i in start..lines.len().min(start + 5) {
        let trimmed = lines[i].trim();
        // variant string "VALUE"
        if let Some(rest) = trimmed.strip_prefix("variant") {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix("string") {
                let rest = rest.trim();
                if let Some(val) = rest.strip_prefix('"') {
                    return val.strip_suffix('"').map(String::from);
                }
            }
        }
        // string "VALUE"
        if let Some(rest) = trimmed.strip_prefix("string \"") {
            if let Some(val) = rest.strip_suffix('"') {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn find_next_uint64_value(lines: &[&str], start: usize) -> Option<u64> {
    for i in start..lines.len().min(start + 5) {
        let mut trimmed = lines[i].trim();
        if let Some(rest) = trimmed.strip_prefix("variant") {
            trimmed = rest.trim();
        }
        for prefix in ["uint64 ", "int64 "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return rest.trim().parse().ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dbus_metadata_output() {
        let sample = r#"method return sender=:1.188 -> destination=:1.274
   variant       array [
         dict entry(
            string "mpris:trackid"
            variant               string "/com/spotify/track/5erh9j6o7dOzSDxLnBirqK"
         )
         dict entry(
            string "mpris:length"
            variant               uint64 227841000
         )
         dict entry(
            string "mpris:artUrl"
            variant               string "https://i.scdn.co/image/ab67616d0000b273b9d93bdf77c7d1fb148f71fe"
         )
         dict entry(
            string "xesam:title"
            variant               string "Khola Haowa"
         )
         dict entry(
            string "xesam:album"
            variant               string "Khola Haowa"
         )
         dict entry(
            string "xesam:artist"
            variant               array [
                  string "Nachiketa"
               ]
         )
      ]"#;
        let meta = parse_metadata(sample).unwrap();
        assert_eq!(meta.track_id, "/com/spotify/track/5erh9j6o7dOzSDxLnBirqK");
        assert_eq!(meta.duration_us, 227841000);
        assert_eq!(meta.duration_ms(), 227841);
        assert_eq!(meta.title, "Khola Haowa");
        assert_eq!(meta.album, "Khola Haowa");
        assert_eq!(meta.artist, "Nachiketa");
        assert_eq!(meta.art_url, "https://i.scdn.co/image/ab67616d0000b273b9d93bdf77c7d1fb148f71fe");
        assert_eq!(meta.spotify_uri().unwrap(), "spotify:track:5erh9j6o7dOzSDxLnBirqK");
        assert_eq!(meta.track_id_short().unwrap(), "5erh9j6o7dOzSDxLnBirqK");
    }

    #[test]
    fn extract_playback_status_string() {
        let sample = r#"method return
   variant       string "Playing""#;
        assert_eq!(extract_string_variant(sample), "Playing");
    }

    #[test]
    fn extract_position_int64() {
        let sample = r#"method return
   variant       int64 193540000"#;
        assert_eq!(extract_int64(sample).unwrap(), 193540000);
    }
}
