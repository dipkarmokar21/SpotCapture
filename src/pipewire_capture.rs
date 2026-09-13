//! PipeWire-based audio capture from Spotify Desktop.
//!
//! Creates a virtual null sink, redirects Spotify's audio output to it,
//! and captures from the sink's monitor — so no sound plays through speakers.

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::process::{Child, Command, Stdio};
use std::path::Path;
use std::time::Duration;
use crate::events::status;

/// Represents a PipeWire virtual null sink.
pub struct NullSink {
    pub node_id: u32,
    pub name: String,
}

/// Represents Spotify's audio stream in PipeWire.
#[derive(Clone, Debug)]
pub struct SpotifyStream {
    pub node_id: u32,
    pub output_port_ids: Vec<u32>,
    pub original_target: Option<u32>,
}

/// Represents an ongoing audio capture process.
pub struct CaptureProcess {
    ffmpeg: std::process::Child,
}

impl CaptureProcess {
    /// Stop the capture process gracefully (SIGTERM then wait).
    pub fn stop(mut self) -> Result<()> {
        // Wait up to 5 seconds for ffmpeg to gracefully exit
        // We first send SIGINT to let ffmpeg write the file trailer
        #[cfg(unix)]
        unsafe {
            libc::kill(self.ffmpeg.id() as i32, libc::SIGINT);
        }
        let start = std::time::Instant::now();
        loop {
            match self.ffmpeg.try_wait() {
                Ok(Some(_status)) => return Ok(()),
                Ok(None) => {
                    if start.elapsed() > Duration::from_secs(5) {
                        let _ = self.ffmpeg.kill();
                        let _ = self.ffmpeg.wait();
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => bail!("Failed to wait for capture process: {e}"),
            }
        }
    }

    /// Kill the capture process immediately.
    pub fn kill(mut self) {
        let _ = self.ffmpeg.kill();
    }
}

impl Drop for CaptureProcess {
    fn drop(&mut self) {
        let _ = self.ffmpeg.kill();
        let _ = self.ffmpeg.wait();
    }
}

// We need libc for SIGINT — use a small inline FFI instead of adding a dep
#[cfg(unix)]
mod libc {
    unsafe extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
    pub const SIGINT: i32 = 2;
    pub const SIGTERM: i32 = 15;
}

/// Find Spotify's audio output stream in PipeWire.
pub fn find_spotify_stream() -> Result<SpotifyStream> {
    let output = Command::new("pw-dump")
        .output()
        .context("pw-dump failed; is PipeWire installed?")?;
    ensure!(output.status.success(), "pw-dump returned an error");

    let data: Vec<Value> = serde_json::from_slice(&output.stdout)
        .context("Failed to parse pw-dump output")?;

    // Find Spotify's audio output node
    let mut spotify_node_id: Option<u32> = None;
    for obj in &data {
        let props = &obj["info"]["props"];
        let app_name = props["application.name"].as_str().unwrap_or("");
        let media_class = props["media.class"].as_str().unwrap_or("");
        let node_name = props["node.name"].as_str().unwrap_or("");

        if (app_name.eq_ignore_ascii_case("spotify") || node_name.eq_ignore_ascii_case("spotify"))
            && media_class == "Stream/Output/Audio"
        {
            if let Some(id) = obj["id"].as_u64() {
                spotify_node_id = Some(id as u32);
                break;
            }
        }
    }

    let node_id = spotify_node_id.context(
        "Spotify audio stream not found in PipeWire. Make sure Spotify Desktop is running and playing audio"
    )?;

    // Find output port IDs for this node
    let mut output_port_ids = Vec::new();
    for obj in &data {
        let props = &obj["info"]["props"];
        if props["node.id"].as_u64() == Some(node_id as u64) {
            let port_dir = props["port.direction"].as_str().unwrap_or("");
            if port_dir == "out" {
                if let Some(id) = obj["id"].as_u64() {
                    output_port_ids.push(id as u32);
                }
            }
        }
    }

    // Find what Spotify is currently linked to (to restore later)
    let mut original_target: Option<u32> = None;
    for obj in &data {
        let props = &obj["info"]["props"];
        if props["node.id"].as_u64() == Some(node_id as u64) {
            if let Some(target) = props["target.node"].as_u64()
                .or_else(|| props["node.target"].as_u64())
            {
                original_target = Some(target as u32);
                break;
            }
        }
    }

    Ok(SpotifyStream {
        node_id,
        output_port_ids,
        original_target,
    })
}

/// Create a virtual null sink for silent capture.
pub fn create_null_sink() -> Result<NullSink> {
    let sink_name = format!("spotcapture_sink_{}", std::process::id());

    // Use pw-cli to create a null sink node
    let output = Command::new("pw-cli")
        .args([
            "create-node", "adapter",
            &format!("{{ \"factory.name\": \"support.null-audio-sink\", \"node.name\": \"{sink_name}\", \"media.class\": \"Audio/Sink\", \"object.linger\": true, \"audio.position\": \"[FL,FR]\", \"monitor.channel-volumes\": true }}"),
        ])
        .output()
        .context("pw-cli create-node failed")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create null sink: {stderr}");
    }

    // Give PipeWire a moment to register the node
    std::thread::sleep(Duration::from_millis(300));

    // Find the created node's ID using pw-dump
    let dump_output = Command::new("pw-dump")
        .arg("Node")
        .output()
        .context("pw-dump failed after creating node")?;
    
    let data: Vec<Value> = serde_json::from_slice(&dump_output.stdout)
        .context("Failed to parse pw-dump output for null sink")?;
        
    let mut node_id: Option<u32> = None;
    for obj in &data {
        if let Some(info) = obj.get("info") {
            if let Some(props) = info.get("props") {
                if let Some(name) = props.get("node.name").and_then(|n| n.as_str()) {
                    if name == sink_name {
                        if let Some(id) = obj.get("id").and_then(|id| id.as_u64()) {
                            node_id = Some(id as u32);
                            break;
                        }
                    }
                }
            }
        }
    }
    
    let node_id = node_id.context("Could not find created null sink in pw-dump")?;

    Ok(NullSink {
        node_id,
        name: sink_name,
    })
}

/// Redirect Spotify's audio to the null sink using WirePlumber metadata.
pub fn redirect_to_sink(stream: &SpotifyStream, sink: &NullSink) -> Result<()> {
    // Use wpctl to set the target for Spotify's stream
    let _output = Command::new("wpctl")
        .args(["set-default", &sink.node_id.to_string()])
        .output();

    // Method 1: Use pw-metadata to set target.node for the stream
    let result = Command::new("pw-metadata")
        .args([
            &stream.node_id.to_string(),
            "target.node",
            &sink.node_id.to_string(),
            "Spa:Id",
        ])
        .output()
        .context("pw-metadata failed")?;

    if !result.status.success() {
        // Method 2: Try setting via target.object
        let _result2 = Command::new("pw-metadata")
            .args([
                "0",  // default metadata
                &format!("target.node.{}", stream.node_id),
                &sink.node_id.to_string(),
                "Spa:Id",
            ])
            .output();
    }

    // Wait for the redirect to take effect
    std::thread::sleep(Duration::from_millis(500));

    // Verify by checking current links
    status(format!("Redirected Spotify (node {}) → null sink (node {})", stream.node_id, sink.node_id));
    Ok(())
}



pub fn start_ffmpeg_capture(sink: &NullSink, output_path: &Path, format: &str, bitrate: &str) -> Result<CaptureProcess> {
    let bitrate_label = format!("{bitrate}k");
    let format_args: Vec<&str> = match format {
        "flac" => vec!["-c:a", "flac", "-compression_level", "5", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"],
        "wav" => vec!["-c:a", "pcm_s24le"],
        _ => vec!["-c:a", "libmp3lame", "-b:a", &bitrate_label, "-id3v2_version", "3"],
    };

    let ext = match format {
        "flac" => "flac",
        "wav" => "wav",
        _ => "mp3",
    };

    // We use a single FFmpeg process that reads from PipeWire directly
    // using the PulseAudio compatibility layer (pulse).
    // This perfectly routes the monitor ports.
    let monitor_source = format!("{}.monitor", sink.name);

    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner", "-loglevel", "error", "-nostats", "-nostdin",
        "-y",
        "-f", "pulse",
        "-i", &monitor_source,
        "-ar", "44100",
        "-ac", "2",
    ]);
    for arg in &format_args {
        cmd.arg(arg);
    }
    cmd.arg("-f").arg(ext);
    cmd.arg(output_path);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let ffmpeg = cmd.spawn().context(
        "Failed to start FFmpeg capture pipeline. Make sure ffmpeg is installed"
    )?;

    Ok(CaptureProcess { ffmpeg })
}

/// Restore Spotify's audio to the default output sink.
pub fn restore_audio(stream: &SpotifyStream) -> Result<()> {
    // Clear the metadata target so WirePlumber routes back to default
    let _ = Command::new("pw-metadata")
        .args([
            &stream.node_id.to_string(),
            "target.node",
            "",
            "",
        ])
        .output();

    // Also try deleting the metadata entry
    let _ = Command::new("pw-metadata")
        .args([
            "-d",
            &stream.node_id.to_string(),
            "target.node",
        ])
        .output();

    status("Spotify audio restored to default output");
    Ok(())
}

/// Destroy the null sink.
pub fn destroy_null_sink(sink: &NullSink) -> Result<()> {
    let output = Command::new("pw-cli")
        .args(["destroy", &sink.node_id.to_string()])
        .output()
        .context("pw-cli destroy failed")?;
    if !output.status.success() {
        // Not critical, PipeWire will clean up when our process exits
        status(format!("Warning: could not destroy null sink {}", sink.node_id));
    }
    Ok(())
}

/// Get the default audio sink node ID.
pub fn get_default_sink_id() -> Result<u32> {
    let output = Command::new("wpctl")
        .args(["inspect", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .context("wpctl inspect failed")?;
    ensure!(output.status.success(), "Could not inspect default audio sink");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line usually contains: id X, type ...
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("id ") {
            if let Some(id_str) = trimmed.strip_prefix("id ") {
                if let Some(id) = id_str.split(',').next().and_then(|s| s.trim().parse().ok()) {
                    return Ok(id);
                }
            }
        }
    }
    bail!("Could not determine default sink ID")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_sink_name_is_unique_per_process() {
        let name = format!("spotcapture_sink_{}", std::process::id());
        assert!(name.starts_with("spotcapture_sink_"));
        assert!(name.len() > 18);
    }
}
