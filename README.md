# SpotCapture

SpotCapture is a desktop application for Ubuntu/Linux that records high-quality audio directly from the official **Spotify Desktop App** using PipeWire. 

Because it captures the system audio stream using a virtual "null sink", the recording is **completely silent** (you won't hear the song playing through your speakers while it records).

## Features

- **Highest Quality:** Records the exact audio stream Spotify produces.
- **Silent Capture:** Uses PipeWire virtual sinks to record without playing sound through speakers.
- **Auto-Sync:** Uses MPRIS D-Bus to automatically play the track from the beginning and stop when it finishes.
- **Multiple Formats:** Export to MP3 (128-320 kbps), FLAC, or WAV.
- **Full Metadata:** Automatically embeds cover art, title, artist, and album tags (can use Spotify Web API for extended metadata).

## Requirements

1. **Ubuntu/Linux** with a graphical desktop environment.
2. **Spotify Desktop App** installed and running.
3. **PipeWire** audio server (`pipewire`, `wireplumber`, `pipewire-pulse`).
4. **FFmpeg** installed on your system.

## Build and Install

1. Install system dependencies:
   ```bash
   sudo apt install ffmpeg tcl8.6-dev tk8.6-dev libssl-dev pkg-config
   ```

2. Build the capture engine (Rust) and UI (C++):
   ```bash
   ./scripts/build.sh
   ```

3. Run the application:
   ```bash
   ./build/spotcapture-ui
   ```

## How It Works

SpotCapture v1.0.0 uses a real-time system audio capture approach:
1. **D-Bus Control:** SpotCapture talks to Spotify via MPRIS D-Bus. When you paste a link, it tells Spotify to open that specific track and play it from the beginning.
2. **Virtual Sink:** It creates a temporary PipeWire "null sink" (a virtual speaker that makes no sound).
3. **Redirection:** It routes Spotify's audio output exclusively to this null sink.
4. **Capture:** It runs `pw-record` connected to the null sink's monitor, piping the raw PCM audio into `ffmpeg` for encoding (MP3/FLAC/WAV).
5. **Auto-Stop:** It monitors the track position via D-Bus and stops recording exactly when the track finishes.
6. **Restoration:** Finally, it restores Spotify's audio back to your default speakers.

*Note: Because this is a real-time capture from the official client, recording a 3-minute song takes exactly 3 minutes.*

## Command Line Usage

You can use the capture engine directly from the terminal without the UI:

```bash
# Simple MP3 capture (defaults to 320 kbps)
./build/spotcapture download "https://open.spotify.com/track/..."

# Capture in FLAC
./build/spotcapture download "spotify:track:..." --format flac

# Custom bitrate and output directory
./build/spotcapture download "..." --format mp3 --bitrate 192 --output-dir ~/Desktop
```

## Troubleshooting

- **"Spotify Desktop is not running"**: Make sure the official Spotify Linux app is open and you have played at least one second of audio (so PipeWire registers it).
- **No metadata/tags**: The fallback MPRIS metadata provides basic tags. For high-resolution artwork and full tags, click "Connect metadata" in the UI to sign in with your own Spotify Developer App Client ID.

## Architecture

- `ui/`: Tcl/Tk-based C++ frontend (`spotcapture-ui`)
- `src/`: Rust backend capture engine (`spotcapture`)
  - `pipewire_capture.rs`: Handles virtual sinks and audio redirection
  - `spotify_dbus.rs`: MPRIS controls for the Spotify client
  - `capture.rs`: The main recording orchestrator
# SpotCapture
