//! Capture Spotify Desktop audio via PipeWire system output.
//!
//! Flow:
//! 1. Verify Spotify Desktop is running (MPRIS D-Bus)
//! 2. Create PipeWire null sink (silent capture)
//! 3. Redirect Spotify audio to null sink
//! 4. Start FFmpeg encoder from null sink monitor
//! 5. Open track in Spotify (from beginning)
//! 6. Monitor playback position until track ends
//! 7. Stop capture, finalize output with metadata + artwork
//! 8. Restore Spotify audio to speakers

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use crate::{auth, events::{emit, status}, metadata, pipewire_capture, spotify_dbus};
use crate::output::{Bitrate, Format, Metadata};

/// Configuration for a capture session.
pub struct CaptureConfig {
    pub format: Format,
    pub bitrate: Bitrate,
    pub output_dir: PathBuf,
    pub client_id: Option<String>,
    pub cache_dir: PathBuf,
}

pub async fn run(track_url: &str, config: &CaptureConfig) -> Result<PathBuf> {
    // 1. Parse track ID
    let id = metadata::track_id(track_url)?;
    let spotify_uri = format!("spotify:track:{id}");

    // 2. Check Spotify Desktop is running
    status("Checking Spotify Desktop…");
    ensure!(
        spotify_dbus::is_spotify_running(),
        "Spotify Desktop is not running. Start Spotify and play any song first"
    );

    // 3. Get track metadata (Web API if available, else MPRIS fallback)
    status("Loading track metadata…");
    let mut track_info = load_metadata(&id, config.client_id.as_deref(), &config.cache_dir).await;
    let display_name = build_display_name(&track_info.tags, &id);

    // 4. Create null sink for silent capture
    status("Creating silent capture sink…");
    let null_sink = pipewire_capture::create_null_sink()
        .context("Failed to create PipeWire null sink for silent capture")?;
    status(format!("Null sink created (node {})", null_sink.node_id));

    // Run the capture with cleanup on all exit paths
    let result = run_capture_session(&id, &spotify_uri, &display_name, &null_sink, &mut track_info, config).await;

    // Cleanup: always destroy the null sink
    let _ = pipewire_capture::destroy_null_sink(&null_sink);

    result
}

async fn run_capture_session(
    id: &str,
    spotify_uri: &str,
    display_name: &str,
    null_sink: &pipewire_capture::NullSink,
    track_info: &mut metadata::TrackInfo,
    config: &CaptureConfig,
) -> Result<PathBuf> {
    // 5. Find Spotify's audio stream in PipeWire
    status("Finding Spotify audio stream…");
    let mut stream = pipewire_capture::find_spotify_stream();
    if stream.is_err() {
        status("Waking up Spotify to create audio stream…");
        // Spotify destroys its PipeWire node when paused for a while.
        // We need to trigger playback to create the node.
        let _ = spotify_dbus::open_uri(spotify_uri);
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            if let Ok(s) = pipewire_capture::find_spotify_stream() {
                stream = Ok(s);
                // Pause it briefly so we can redirect and seek silently
                let _ = spotify_dbus::pause();
                break;
            }
        }
    }
    let stream = stream.context("Could not find Spotify's audio in PipeWire. Make sure Spotify Desktop is running")?;
    status(format!("Found Spotify stream (node {})", stream.node_id));

    // 6. Redirect Spotify to null sink (silent capture)
    status("Redirecting Spotify audio for silent capture…");
    pipewire_capture::redirect_to_sink(&stream, null_sink)?;

    // Ensure we restore audio on any exit
    let restore_stream = stream.clone();
    let _restore_guard = scopeguard::defer(|| {
        let _ = pipewire_capture::restore_audio(&restore_stream);
    });

    // 7. Prepare output file
    std::fs::create_dir_all(&config.output_dir).context("Could not create output folder")?;
    let output_path = config.output_dir.join(format!(
        "{}.{}",
        metadata::safe_filename(&display_name),
        config.format.extension()
    ));
    ensure!(
        !output_path.exists(),
        "Output file already exists: {}",
        output_path.display()
    );

    // 8. Prepare the track in Spotify (load it and seek to 0:00)
    status(format!("Preparing {display_name} in Spotify…"));
    spotify_dbus::open_uri(spotify_uri)?;
    
    // Give Spotify a moment to load the track and start playing
    tokio::time::sleep(Duration::from_millis(400)).await;
    
    // Pause it and seek to the exact beginning
    let _ = spotify_dbus::pause();
    let _ = spotify_dbus::seek_to_start();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 9. Start the capture (FFmpeg pulse pipeline)
    status(format!("Starting {} {} capture…", config.bitrate.label(), config.format.extension().to_uppercase()));
    let capture = pipewire_capture::start_ffmpeg_capture(
        null_sink,
        &output_path,
        config.format.extension(),
        &config.bitrate.kbps().to_string(),
    ).context("Failed to start audio capture")?;

    // 10. Start playback for recording
    status(format!("Recording {display_name}…"));
    let _ = spotify_dbus::play();

    // Wait for Spotify to confirm it's playing our track
    spotify_dbus::wait_for_playing(id, Duration::from_secs(15)).await
        .context("Spotify did not start playing the track in time")?;

    // 10. Get the actual duration from MPRIS (most reliable)
    let mpris_meta = spotify_dbus::get_metadata()?;
    let duration_us = mpris_meta.duration_us;
    let duration_ms = mpris_meta.duration_ms();
    ensure!(duration_ms > 0, "Track has no duration information");

    let duration_display = format!(
        "{}:{:02}",
        (duration_ms / 1000) / 60,
        (duration_ms / 1000) % 60
    );
    status(format!("Recording {display_name} ({duration_display})…"));

    // 11. Monitor playback until track ends
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let start_time = Instant::now();

    // Progress reporting task
    let progress_handle = {
        let cancel_rx = cancel_rx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                if *cancel_rx.borrow() {
                    break;
                }
                if let Ok(pos) = spotify_dbus::get_position() {
                    let captured_seconds = pos as f64 / 1_000_000.0;
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let total_seconds = duration_us as f64 / 1_000_000.0;
                    emit(json!({
                        "type": "progress",
                        "captured_seconds": captured_seconds,
                        "elapsed_seconds": elapsed,
                        "speed": "1.0×",
                        "duration_seconds": total_seconds,
                    }));
                }
            }
        })
    };

    // Wait for track to end or cancellation
    let capture_result = tokio::select! {
        result = spotify_dbus::wait_for_track_end(duration_us, &cancel_rx) => result,
        _ = tokio::signal::ctrl_c() => {
            let _ = cancel_tx.send(true);
            bail!("Capture cancelled; no file was saved")
        },
    };

    let _ = cancel_tx.send(true);
    progress_handle.abort();

    // 12. Stop the capture
    status("Stopping capture…");

    // Pause Spotify before stopping capture to avoid extra audio
    let _ = spotify_dbus::pause();
    tokio::time::sleep(Duration::from_millis(500)).await;

    capture.stop().context("Failed to stop capture process")?;

    // 13. Restore audio to speakers
    pipewire_capture::restore_audio(&stream)?;
    // scopeguard will also try, but that's ok

    capture_result?;

    // 14. Check output file
    let file_meta = std::fs::metadata(&output_path)
        .context("Capture output file was not created")?;
    ensure!(file_meta.len() > 0, "Capture output file is empty; no audio was recorded");

    // 15. If we need to embed metadata, do a second-pass with FFmpeg
    let final_path = if needs_metadata_embed(track_info) {
        status("Embedding metadata and artwork…");
        embed_metadata(&output_path, track_info, config)?
    } else {
        output_path.clone()
    };

    let elapsed = start_time.elapsed().as_secs_f64();
    let captured_seconds = duration_ms as f64 / 1000.0;
    emit(json!({
        "type": "done",
        "message": "Capture complete",
        "path": final_path.to_string_lossy(),
        "captured_seconds": captured_seconds,
        "elapsed_seconds": elapsed,
        "speed": 1.0,
    }));

    Ok(final_path)
}

/// Load metadata from Web API if available, otherwise use MPRIS D-Bus data.
async fn load_metadata(id: &str, client_id: Option<&str>, cache: &Path) -> metadata::TrackInfo {
    // Try Web API first
    match auth::load(cache, client_id).await {
        Ok(tokens) => {
            let info = metadata::fetch(id, &tokens.access_token, cache).await;
            if info.tags.title.is_some() {
                return info;
            }
        }
        Err(_) => {
            status("Web API not connected; using Spotify Desktop metadata");
        }
    }

    // Fall back to MPRIS D-Bus metadata
    mpris_metadata_fallback(id)
}

/// Build track info from MPRIS D-Bus metadata.
fn mpris_metadata_fallback(id: &str) -> metadata::TrackInfo {
    match spotify_dbus::get_metadata() {
        Ok(meta) => {
            let mut info = metadata::TrackInfo::fallback(id);
            if !meta.title.is_empty() {
                info.tags.title = Some(meta.title.clone());
            }
            if !meta.artist.is_empty() {
                info.tags.artist = Some(meta.artist.clone());
            }
            if !meta.album.is_empty() {
                info.tags.album = Some(meta.album.clone());
            }
            info.duration_ms = Some(meta.duration_ms());
            info
        }
        Err(_) => metadata::TrackInfo::fallback(id),
    }
}

fn build_display_name(tags: &Metadata, id: &str) -> String {
    let title = tags.title.as_deref().unwrap_or(id);
    match tags.artist.as_deref() {
        Some(artist) => format!("{title} - {artist}"),
        None => title.to_string(),
    }
}

fn needs_metadata_embed(info: &metadata::TrackInfo) -> bool {
    info.tags.title.is_some() || info.tags.artist.is_some() || info.artwork.is_some()
}

/// Embed metadata and artwork into an existing audio file using FFmpeg.
fn embed_metadata(
    input_path: &Path,
    info: &metadata::TrackInfo,
    config: &CaptureConfig,
) -> Result<PathBuf> {
    let temp_path = input_path.with_extension(format!("tmp.{}", config.format.extension()));

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args([
        "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i",
    ]);
    cmd.arg(input_path);

    // Add artwork as second input if available
    if let Some(artwork) = &info.artwork {
        if config.format.supports_artwork() {
            cmd.arg("-i").arg(&artwork.0);
        }
    }

    cmd.args(["-map", "0:a:0"]);

    // Map artwork
    if info.artwork.is_some() && config.format.supports_artwork() {
        cmd.args([
            "-map", "1:v:0",
            "-c:v", "copy",
            "-disposition:v:0", "attached_pic",
            "-metadata:s:v:0", "title=Album cover",
            "-metadata:s:v:0", "comment=Cover (front)",
        ]);
    }

    // Copy audio codec (no re-encoding)
    cmd.args(["-c:a", "copy"]);

    // Add metadata tags
    if let Some(title) = &info.tags.title {
        cmd.arg("-metadata").arg(format!("title={title}"));
    }
    if let Some(artist) = &info.tags.artist {
        cmd.arg("-metadata").arg(format!("artist={artist}"));
    }
    if let Some(album) = &info.tags.album {
        cmd.arg("-metadata").arg(format!("album={album}"));
    }
    if let Some(date) = &info.tags.date {
        cmd.arg("-metadata").arg(format!("date={date}"));
    }
    if let Some(track) = &info.tags.track {
        cmd.arg("-metadata").arg(format!("track={track}"));
    }
    if let Some(disc) = &info.tags.disc {
        cmd.arg("-metadata").arg(format!("disc={disc}"));
    }

    cmd.arg(&temp_path);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.spawn()?.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        status(format!("Metadata embedding warning: {}", stderr.trim()));
        // Return original file if metadata embedding fails
        return Ok(input_path.to_path_buf());
    }

    // Replace original with tagged version
    std::fs::rename(&temp_path, input_path)?;
    Ok(input_path.to_path_buf())
}

// Simple scope guard without external crate dependency
mod scopeguard {
    pub struct ScopeGuard<F: FnOnce()>(Option<F>);

    impl<F: FnOnce()> Drop for ScopeGuard<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }

    pub fn defer<F: FnOnce()>(f: F) -> ScopeGuard<F> {
        ScopeGuard(Some(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_with_artist_and_title() {
        let tags = Metadata {
            title: Some("Khola Haowa".into()),
            artist: Some("Nachiketa".into()),
            ..Default::default()
        };
        assert_eq!(build_display_name(&tags, "abc123"), "Khola Haowa - Nachiketa");
    }

    #[test]
    fn display_name_without_artist() {
        let tags = Metadata {
            title: Some("Khola Haowa".into()),
            ..Default::default()
        };
        assert_eq!(build_display_name(&tags, "abc123"), "Khola Haowa");
    }

    #[test]
    fn display_name_fallback_to_id() {
        let tags = Metadata::default();
        assert_eq!(build_display_name(&tags, "abc123"), "abc123");
    }
}
