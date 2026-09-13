//! Streaming, clock-free PCM encoding. This module never fetches or decrypts audio.
//!
//! The caller supplies interleaved stereo PCM. FFmpeg consumes it as quickly as
//! the caller and encoder allow; no playback clock or `-re` option is introduced.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: usize = 2;
const STDERR_LIMIT: usize = 32 * 1024;
const CONVERSION_SAMPLES: usize = 16 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Format {
    Flac,
    Wav,
    #[default]
    Mp3,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Bitrate {
    Kbps128,
    Kbps192,
    Kbps256,
    #[default]
    Kbps320,
}

impl Bitrate {
    pub fn kbps(self) -> u32 {
        match self {
            Self::Kbps128 => 128,
            Self::Kbps192 => 192,
            Self::Kbps256 => 256,
            Self::Kbps320 => 320,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Kbps128 => "128k",
            Self::Kbps192 => "192k",
            Self::Kbps256 => "256k",
            Self::Kbps320 => "320k",
        }
    }
}

impl FromStr for Bitrate {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim_end_matches('k').trim_end_matches("kbps") {
            "128" => Ok(Self::Kbps128),
            "192" => Ok(Self::Kbps192),
            "256" => Ok(Self::Kbps256),
            "320" => Ok(Self::Kbps320),
            _ => bail!("unsupported bitrate {value:?}; choose 128, 192, 256, or 320"),
        }
    }
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
        }
    }

    pub fn supports_artwork(self) -> bool {
        self != Self::Wav
    }
}

impl FromStr for Format {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "flac" => Ok(Self::Flac),
            "wav" => Ok(Self::Wav),
            "mp3" => Ok(Self::Mp3),
            _ => bail!("unsupported output format {value:?}; choose mp3, flac, or wav"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub date: Option<String>,
    pub track: Option<String>,
    pub disc: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    /// The final filename. Existing files are never overwritten.
    pub output: PathBuf,
    pub format: Format,
    pub bitrate: Bitrate,
    pub metadata: Metadata,
    /// Optional local JPEG or PNG. Supported for FLAC and MP3, not WAV.
    pub artwork: Option<PathBuf>,
    pub ffmpeg: PathBuf,
}

impl EncoderConfig {
    pub fn new(output: impl Into<PathBuf>, format: Format) -> Self {
        Self {
            output: output.into(),
            format,
            bitrate: Bitrate::default(),
            metadata: Metadata::default(),
            artwork: None,
            ffmpeg: PathBuf::from("ffmpeg"),
        }
    }

    pub fn with_bitrate(mut self, bitrate: Bitrate) -> Self {
        self.bitrate = bitrate;
        self
    }
}

/// Owns a private partial file; cleanup is attempted on every exit path.
struct PartialFile {
    path: PathBuf,
}

impl PartialFile {
    fn create(output: &Path) -> Result<Self> {
        let name = output
            .file_name()
            .context("output must include a filename")?;
        let parent = output_parent(output);
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create output directory {}", parent.display()))?;
        // Include time and a counter so independent processes and concurrent
        // encoders do not share partial files. create_new is the authority.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let mut partial_name = OsString::from(".");
            partial_name.push(name);
            partial_name.push(format!(
                ".{}.{}.{sequence}.partial",
                std::process::id(),
                stamp
            ));
            let path = parent.join(partial_name);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    drop(file);
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot create partial file {}", path.display()))
                }
            }
        }
        bail!("could not reserve a unique partial output file")
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// A managed FFmpeg process. Dropping an unfinished encoder cancels it, reaps
/// the process, and removes its partial file. Only finish() publishes output.
pub struct Encoder {
    config: EncoderConfig,
    partial: PartialFile,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
    conversion: Vec<u8>,
    samples_written: u64,
    failed: bool,
}

impl Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        if fs::symlink_metadata(&config.output).is_ok() {
            bail!("output already exists: {}", config.output.display());
        }
        if let Some(artwork) = &config.artwork {
            if !config.format.supports_artwork() {
                bail!("WAV does not support embedded cover art; use FLAC/MP3 or omit artwork");
            }
            if !artwork.is_file() {
                bail!("artwork is not a readable file: {}", artwork.display());
            }
        }

        let partial = PartialFile::create(&config.output)?;
        let mut command = Command::new(&config.ffmpeg);
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostats",
                "-nostdin",
                // Only the private, create_new-reserved partial is overwritten.
                "-y",
                "-f",
                "f32le",
                "-ar",
                "44100",
                "-ac",
                "2",
                "-i",
                "pipe:0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(artwork) = &config.artwork {
            command.arg("-i").arg(artwork);
        }
        command.args(["-map", "0:a:0", "-map_metadata", "-1", "-map_chapters", "-1"]);
        if config.artwork.is_some() {
            command.args([
                "-map",
                "1:v:0",
                "-c:v",
                "copy",
                "-disposition:v:0",
                "attached_pic",
                "-metadata:s:v:0",
                "title=Album cover",
                "-metadata:s:v:0",
                "comment=Cover (front)",
            ]);
        }
        match config.format {
            Format::Flac => {
                command.args([
                    "-c:a",
                    "flac",
                    "-compression_level",
                    "5",
                    "-sample_fmt",
                    "s32",
                    "-bits_per_raw_sample",
                    "24",
                ]);
            }
            Format::Wav => {
                command.args(["-c:a", "pcm_s24le"]);
            }
            Format::Mp3 => {
                command.args(["-c:a", "libmp3lame", "-b:a", config.bitrate.label(), "-id3v2_version", "3"]);
            }
        }
        for (key, value) in [
            ("title", &config.metadata.title),
            ("artist", &config.metadata.artist),
            ("album", &config.metadata.album),
            ("date", &config.metadata.date),
            ("track", &config.metadata.track),
            ("disc", &config.metadata.disc),
        ] {
            if let Some(value) = value {
                command.arg("-metadata").arg(format!("{key}={value}"));
            }
        }
        // Explicit muxer: the temporary filename intentionally ends in .partial.
        command
            .arg("-f")
            .arg(config.format.extension())
            .arg(&partial.path);

        let child = command.spawn().with_context(|| {
            format!(
                "cannot start {}; install FFmpeg or set its executable path",
                config.ffmpeg.display()
            )
        })?;
        // Establish ownership before any fallible initialization: Drop then
        // handles startup errors without leaking the child or partial file.
        let mut encoder = Self {
            config,
            partial,
            child: Some(child),
            stdin: None,
            stderr: None,
            conversion: Vec::with_capacity(CONVERSION_SAMPLES * 4),
            samples_written: 0,
            failed: false,
        };
        let child = encoder.child.as_mut().expect("child initialized");
        encoder.stdin = child.stdin.take();
        let stderr = child.stderr.take().context("FFmpeg stderr pipe missing")?;
        encoder.stderr = Some(
            thread::Builder::new()
                .name("ffmpeg-stderr".into())
                .spawn(move || read_bounded_tail(stderr, STDERR_LIMIT))
                .context("cannot start FFmpeg stderr reader")?,
        );
        Ok(encoder)
    }

    /// Accepts interleaved L/R samples. Writes block under encoder backpressure,
    /// bounding memory use instead of growing an unbounded audio queue.
    pub fn write_samples(&mut self, samples: &[f64]) -> Result<()> {
        if self.failed {
            bail!("encoder cannot continue after an earlier write failure");
        }
        let new_count = self
            .samples_written
            .checked_add(samples.len() as u64)
            .context("PCM sample count overflow")?;
        for chunk in samples.chunks(CONVERSION_SAMPLES) {
            self.conversion.clear();
            for &sample in chunk {
                self.conversion.extend_from_slice(&pcm_f32(sample).to_le_bytes());
            }
            let result = self
                .stdin
                .as_mut()
                .context("FFmpeg input is closed")?
                .write_all(&self.conversion);
            if let Err(error) = result {
                self.failed = true;
                return Err(error).context("FFmpeg stopped accepting PCM audio");
            }
        }
        self.samples_written = new_count;
        Ok(())
    }

    pub fn frames_written(&self) -> u64 {
        self.samples_written / CHANNELS as u64
    }

    /// Flushes the encoder and publishes a completed file atomically. A hard
    /// link in the same directory provides create-if-absent semantics; unlike
    /// rename(), it cannot silently replace a file created by another process.
    pub fn finish(mut self) -> Result<PathBuf> {
        if self.failed {
            bail!("cannot finish after an encoder write failure");
        }
        if self.samples_written == 0 {
            bail!("no PCM audio was captured; no output was saved");
        }
        if self.samples_written % CHANNELS as u64 != 0 {
            bail!("incomplete stereo PCM frame; no output was saved");
        }
        // EOF makes FFmpeg flush codec delay, tags, duration, and stream headers.
        drop(self.stdin.take());
        let status = self
            .child
            .as_mut()
            .context("FFmpeg process is missing")?
            .wait()
            .context("cannot wait for FFmpeg")?;
        self.child.take();
        let stderr = self
            .stderr
            .take()
            .context("FFmpeg stderr reader is missing")?
            .join()
            .map_err(|_| anyhow!("FFmpeg stderr reader panicked"))?
            .context("cannot read FFmpeg diagnostics")?;
        if !status.success() {
            bail!(
                "FFmpeg failed ({status}): {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        let output_file = File::open(&self.partial.path)
            .context("FFmpeg did not produce its temporary output")?;
        if output_file.metadata()?.len() == 0 {
            bail!("FFmpeg produced an empty file; no output was saved");
        }
        output_file.sync_all().context("cannot sync encoded audio")?;
        drop(output_file);
        fs::hard_link(&self.partial.path, &self.config.output).with_context(|| {
            format!(
                "cannot publish {} without overwriting an existing file; the output filesystem must support hard links",
                self.config.output.display()
            )
        })?;
        // Publication succeeded. Cleanup and directory syncing are best effort
        // here: reporting failure after publication would misstate its outcome.
        let _ = fs::remove_file(&self.partial.path);
        if let Ok(directory) = File::open(output_parent(&self.config.output)) {
            let _ = directory.sync_all();
        }
        Ok(self.config.output.clone())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
        // PartialFile's own Drop removes the temporary only after child exit.
    }
}

fn pcm_f32(sample: f64) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0) as f32
    } else {
        0.0
    }
}

fn read_bounded_tail(mut source: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(limit);
    let mut block = [0u8; 8192];
    loop {
        let count = match source.read(&mut block) {
            Ok(0) => return Ok(tail),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let data = &block[..count];
        if count >= limit {
            tail.clear();
            tail.extend_from_slice(&data[count - limit..]);
        } else {
            let discarded = (tail.len() + count).saturating_sub(limit);
            if discarded > 0 {
                tail.drain(..discarded);
            }
            tail.extend_from_slice(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pcm-output-test-{}-{stamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn files(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn has_ffmpeg() -> bool {
        ["ffmpeg", "ffprobe"].iter().all(|program| {
            Command::new(program)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }

    fn tone(frames: usize) -> Vec<f64> {
        (0..frames)
            .flat_map(|index| {
                let sample = (index as f64 * 440.0 * std::f64::consts::TAU / SAMPLE_RATE as f64)
                    .sin()
                    * 0.1;
                [sample, sample]
            })
            .collect()
    }

    fn probe(path: &Path) -> Value {
        let result = Command::new("ffprobe")
            .args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
            .arg(path)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        serde_json::from_slice(&result.stdout).unwrap()
    }

    #[test]
    fn pcm_samples_are_finite_and_limited() {
        assert_eq!(pcm_f32(f64::NAN), 0.0);
        assert_eq!(pcm_f32(f64::INFINITY), 0.0);
        assert_eq!(pcm_f32(f64::NEG_INFINITY), 0.0);
        assert_eq!(pcm_f32(-1.5), -1.0);
        assert_eq!(pcm_f32(1.5), 1.0);
        assert_eq!(pcm_f32(0.125), 0.125);
    }

    #[test]
    fn diagnostic_reader_consumes_everything_but_retains_only_tail() {
        let source: Vec<u8> = (0..100_000).map(|index| (index % 251) as u8).collect();
        let tail = read_bounded_tail(source.as_slice(), 17_000).unwrap();
        assert_eq!(tail, source[source.len() - 17_000..]);
        assert_eq!(read_bounded_tail(source.as_slice(), 0).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn existing_output_is_never_modified() {
        let directory = TestDirectory::new();
        let output = directory.0.join("existing.flac");
        fs::write(&output, b"original").unwrap();
        assert!(Encoder::new(EncoderConfig::new(&output, Format::Flac)).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"original");
        assert_eq!(directory.files().len(), 1);
    }

    #[test]
    fn startup_failure_removes_partial() {
        let directory = TestDirectory::new();
        let mut config = EncoderConfig::new(directory.0.join("failed.flac"), Format::Flac);
        config.ffmpeg = directory.0.join("missing-ffmpeg");
        assert!(Encoder::new(config).is_err());
        assert!(directory.files().is_empty());
    }

    #[test]
    fn synthetic_audio_has_correct_duration_format_and_metadata() {
        if !has_ffmpeg() {
            eprintln!("skipping FFmpeg integration test: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = TestDirectory::new();
        let samples = tone(SAMPLE_RATE as usize);
        for format in [Format::Flac, Format::Wav, Format::Mp3] {
            let path = directory.0.join(format!("tone.{}", format.extension()));
            let mut config = EncoderConfig::new(&path, format);
            config.metadata = Metadata {
                title: Some("Synthetic tone = test".into()),
                artist: Some("Local test".into()),
                album: Some("Encoder verification".into()),
                date: Some("2026".into()),
                track: Some("2/3".into()),
                disc: Some("1/1".into()),
            };
            let mut encoder = Encoder::new(config).unwrap();
            // Split across an odd sample boundary to cover streaming frames.
            encoder.write_samples(&samples[..997]).unwrap();
            encoder.write_samples(&samples[997..]).unwrap();
            assert_eq!(encoder.frames_written(), SAMPLE_RATE as u64);
            assert!(!path.exists());
            assert_eq!(encoder.finish().unwrap(), path);
            let info = probe(&path);
            let streams = info["streams"].as_array().unwrap();
            let audio = streams.iter().find(|stream| stream["codec_type"] == "audio").unwrap();
            assert_eq!(audio["sample_rate"], "44100");
            assert_eq!(audio["channels"], 2);
            assert_eq!(audio["codec_name"], match format {
                Format::Flac => "flac",
                Format::Wav => "pcm_s24le",
                Format::Mp3 => "mp3",
            });
            let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
            assert!((duration - 1.0).abs() < 0.1, "duration was {duration}");
            assert_eq!(info["format"]["tags"]["title"], "Synthetic tone = test");
            assert_eq!(info["format"]["tags"]["artist"], "Local test");
        }
        assert_eq!(directory.files().len(), 3);
    }

    #[test]
    fn cancelled_empty_and_incomplete_captures_leave_no_output() {
        if !has_ffmpeg() {
            return;
        }
        let directory = TestDirectory::new();
        let path = directory.0.join("cancelled.flac");
        let mut encoder = Encoder::new(EncoderConfig::new(&path, Format::Flac)).unwrap();
        encoder.write_samples(&tone(8192)).unwrap();
        drop(encoder);
        assert!(directory.files().is_empty());

        let encoder = Encoder::new(EncoderConfig::new(&path, Format::Flac)).unwrap();
        assert!(encoder.finish().is_err());
        assert!(directory.files().is_empty());

        let mut encoder = Encoder::new(EncoderConfig::new(&path, Format::Flac)).unwrap();
        encoder.write_samples(&[0.1]).unwrap();
        assert!(encoder.finish().is_err());
        assert!(directory.files().is_empty());
    }

    #[test]
    fn output_created_during_capture_is_not_overwritten() {
        if !has_ffmpeg() {
            return;
        }
        let directory = TestDirectory::new();
        let path = directory.0.join("race.flac");
        let mut encoder = Encoder::new(EncoderConfig::new(&path, Format::Flac)).unwrap();
        encoder.write_samples(&tone(8192)).unwrap();
        fs::write(&path, b"another writer").unwrap();
        assert!(encoder.finish().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"another writer");
        assert_eq!(directory.files(), vec![path]);
    }

    #[test]
    fn flac_and_mp3_embed_a_synthetic_cover() {
        if !has_ffmpeg() {
            return;
        }
        let directory = TestDirectory::new();
        let cover = directory.0.join("cover.jpg");
        let result = Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i",
                "color=c=navy:s=32x32", "-frames:v", "1", "-threads:v", "1",
            ])
            .arg(&cover)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        for format in [Format::Flac, Format::Mp3] {
            let output = directory.0.join(format!("covered.{}", format.extension()));
            let mut config = EncoderConfig::new(&output, format);
            config.artwork = Some(cover.clone());
            let mut encoder = Encoder::new(config).unwrap();
            encoder.write_samples(&tone(8192)).unwrap();
            encoder.finish().unwrap();
            let info = probe(&output);
            let cover_stream = info["streams"]
                .as_array()
                .unwrap()
                .iter()
                .find(|stream| stream["codec_type"] == "video")
                .expect("embedded cover missing");
            assert_eq!(cover_stream["disposition"]["attached_pic"], 1);
        }
    }
}
