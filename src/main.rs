mod auth;
mod capture;
mod events;
mod metadata;
mod output;
mod pipewire_capture;
mod spotify_dbus;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use output::{Bitrate, Format};

#[derive(Parser)]
#[command(version, about = "Capture Spotify Desktop audio via PipeWire system output")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a Spotify track. Requires Spotify Desktop running.
    Download {
        /// Spotify track URL, URI, or track ID
        url: String,
        /// Output format: mp3, flac, or wav
        #[arg(long, default_value = "mp3")]
        format: String,
        /// MP3 bitrate: 128, 192, 256, or 320 (ignored for FLAC/WAV)
        #[arg(long, default_value = "320")]
        bitrate: String,
        /// Output directory (default: ~/Music/SpotCapture)
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Spotify Developer app Client ID for Web API metadata (optional)
        #[arg(long)]
        client_id: Option<String>,
        /// Cache directory for login tokens
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Connect optional Web API metadata (artwork, tags) using your Spotify app Client ID.
    Login {
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Capture one track (alias for download).
    Capture {
        track: String,
        #[arg(long, default_value = "mp3")]
        format: String,
        #[arg(long, default_value = "320")]
        bitrate: String,
        #[arg(long)]
        output_dir: Option<PathBuf>,
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Extract track URIs from a Spotify album or playlist URL.
    Extract {
        url: String,
    },
}

#[tokio::main]
async fn main() {
    env_logger::Builder::new().parse_filters("warn").try_init().ok();
    let args = Args::parse();
    if let Err(error) = execute(args).await {
        events::emit(json!({"type":"error", "message":format!("{error:#}")}));
        std::process::exit(1);
    }
}

async fn execute(args: Args) -> Result<()> {
    match args.command {
        Command::Login { client_id, cache_dir } => {
            let cache = auth::cache_dir(cache_dir)?;
            tokio::select! {
                result = auth::login(client_id, &cache) => { result?; },
                _ = tokio::signal::ctrl_c() => anyhow::bail!("Login cancelled"),
            }
            events::emit(json!({"type":"done", "message":"Metadata connected. You can now download tracks with artwork and tags."}));
        }
        Command::Extract { url } => {
            if let Err(e) = metadata::extract_tracks(&url).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        Command::Download { url, format, bitrate, output_dir, client_id, cache_dir }
        | Command::Capture { track: url, format, bitrate, output_dir, client_id, cache_dir } => {
            let format: Format = format.parse()?;
            let bitrate: Bitrate = bitrate.parse()?;
            let cache = auth::cache_dir(cache_dir)?;
            let output_dir = output_dir.unwrap_or_else(default_output_dir);
            let config = capture::CaptureConfig {
                format,
                bitrate,
                output_dir,
                client_id,
                cache_dir: cache,
            };
            capture::run(&url, &config).await?;
        }
    }
    Ok(())
}

fn default_output_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Music").join("SpotCapture")
}
