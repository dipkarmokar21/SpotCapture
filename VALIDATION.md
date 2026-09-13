# Validation status

Validated locally on Ubuntu 26.04, x86_64, with Rust 1.98.1 and system FFmpeg.

- Release build: passed; `build/spotcapture` and `build/spotcapture-ui` produced.
- Rust tests: 22 passed, zero failed. These exercise authentication callback
  validation, token-error redaction, URL/filename handling, duration validation,
  FFmpeg output/duration/tags, attached artwork, cancellation cleanup and
  no-overwrite publication including a destination created during encoding,
  native playback OAuth identity, separate private playback credentials,
  audio-file selection and distinct key-refusal/timeout diagnostics.
- Native UI headless checks: passed (separate audio/metadata commands, manual
  retry vs. automatic queue advancement, event parsing, Unicode, track input
  and embedded Tcl script completeness).
- Native UI graphical smoke test: opened on the Ubuntu desktop and exited
  successfully after 1.5 seconds, including the updated separate audio/metadata
  interface. No Spotify login or capture was started by the GUI smoke test.
- Synthetic encoder benchmark: 240 seconds of generated stereo 44.1 kHz audio
  encoded to FLAC in 0.469 seconds, approximately 512×. This measures tone
  generation plus PCM-to-file encoding/finalization only, excluding Spotify
  authentication, networking, metadata, artwork and source decoding.

**Live authentication findings (2026-09-10):** the user completed Developer app
OAuth. The first capture failed with login5 `BAD_REQUEST` because that app ID
was also passed as the decoder client identity. Restoring librespot's platform
identity removed that error; a live retest reached login5 `INVALID_CREDENTIALS`.
This matches the external-token compatibility failure reported upstream since
August 2026 ([librespot #1737](https://github.com/librespot-org/librespot/issues/1737)).

Version 0.1.1 adds separate native playback OAuth using librespot's default
client identity and `/login` callback. Developer app OAuth remains separate for
metadata. Native login verifies login5 access before saving reusable playback
credentials. This follows the published
[Retune playback fix](https://github.com/open-cli-collective/Retune/pull/16).

**Live decoder finding (2026-09-10):** the user completed Connect audio. A live
capture of `spotify:track:0hFUtSsV2itYEUTZGj6w5H` then passed session login,
login5 authorization and metadata/artwork retrieval. Spotify refused the audio
key (`error audio key 0 1`). librespot continued without decryption, scanned
about 13.9 MB as MP3 junk and failed with decoder EOF. Zero PCM was captured,
and no completed file was saved. This reproduces the chain reported in
[librespot #1735](https://github.com/librespot-org/librespot/issues/1735), a
duplicate of the unresolved [#1649](https://github.com/librespot-org/librespot/issues/1649).

Version 0.1.2 adds an early audio-key access check before artwork/audio download
and encoder/player startup, using the resolved track and matching audio file. It
distinguishes server refusal from a network timeout. This is failure handling,
not a fix for the upstream refusal; Player still performs its own key request
if the initial check succeeds.

**Version 0.1.2 live retest:** the same track passed session/login5 and track
lookup, selected `OGG_VORBIS_320`, then received `error audio key 0 1`. The
engine emitted the explicit audio-key refusal and exited with status 1 before
starting the player. There were no MP3 junk or decoder EOF warnings, no PCM
progress, and no final or partial files in `test-output/decoder-validation/`.
This verifies the early failure handling on the actual reported failure.
It does not establish that all tracks or accounts fail.

**Still pending:** successful live Spotify capture and speed measurement. No
Spotify audio file has been saved in validation so far. The synthetic benchmark
above is not a Spotify speed measurement.

The application uses a custom librespot PCM sink with no speaker-clock pacing.
Source-code inspection supports the design, but the actual upstream throughput
and availability remain dependent on Spotify, account access and the network.

Limitations: single-track links, sequential queue, 750 ms duration tolerance,
cover art only in FLAC/MP3. The output filesystem must support hard links for
atomic no-overwrite publication. Forced process termination or a machine crash
may leave a hidden `.partial` file; normal failure/cancellation removes it.
