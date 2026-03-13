# ChronoSub ⚡

A blazing-fast, native **Rust** desktop application for non-linear subtitle auto-synchronization. No Python. No FFmpeg installation. No virtual environments.

Drop a video file and an `.srt` subtitle file into the window; ChronoSub analyses the audio and corrects the sync in seconds.

---

## Features

| Feature | ChronoSub | Python tools |
|---|---|---|
| Setup | Double-click the binary | Install Python + pip + virtualenv + system FFmpeg |
| Speed | < 2 s for a feature film | 1–3 minutes |
| Errors | Human-readable status messages | 40-line Python tracebacks |
| GUI | GPU-accelerated, drag & drop | None (CLI only) |

### How it works

1. **Audio decoding** – [`symphonia`](https://crates.io/crates/symphonia) decodes the first audio track from any supported container (MP4, MKV, WebM, …) into raw PCM samples, entirely in-process.
2. **Energy envelope** – [`rayon`](https://crates.io/crates/rayon) parallelises the computation of a 100 Hz short-time RMS energy signal (one sample per 10 ms window) across all CPU cores.
3. **Subtitle signal** – The `.srt` timestamps are converted into a matching 100 Hz binary signal (1 while a cue is active, 0 otherwise).
4. **FFT cross-correlation** – [`rustfft`](https://crates.io/crates/rustfft) computes the O(N log N) cross-correlation between the two signals to find the global time offset.
5. **Apply & save** – The detected offset is applied to every timestamp and the result is written to `<name>_synced.srt` next to the original.

---

## Building from source

```sh
cargo build --release
# Binary is at target/release/chronosub
```

Requires Rust 1.72+ (matches the `eframe` MSRV).

---

## Usage

1. Run `chronosub` (or double-click the binary).
2. Drag & drop your **video file** (`.mp4`, `.mkv`, `.avi`, …) onto the window.
3. Drag & drop your **subtitle file** (`.srt`) onto the window.
4. Click **⚡ Synchronize Subtitles**.
5. Once the offset is shown, click **💾 Save Synced SRT**.

---

## Versioning and updates

- ChronoSub uses `MAJOR.MINOR.PATCH`.
- `MINOR` is derived from repository commit count at build time.
- `MAJOR` is reserved for massive refactors / initial release and is always prompted in-app.
- `MINOR` updates are treated as significant and are prompted in-app.
- Install-aware update guidance is shown:
  - Cargo install: `cargo install --force chronosub`
  - Standalone binary: download the platform-matching release asset and replace the executable.

---

## CI and release automation

- Pull requests and pushes to `main` run `cargo check` + `cargo test`.
- Release workflows auto-compute the release tag as `vMAJOR.<commit-count>.PATCH`; pushing a tag must match this computed value.
- `workflow_dispatch` release runs use the auto-computed tag to publish standalone binaries for Linux/macOS/Windows and create a GitHub release.
- Crates publishing is automated from the published release workflow.
- Release workflows validate that the release tag matches the computed version from VCS state.

---

## Dependencies

| Crate | Purpose |
|---|---|
| `eframe 0.24` | GPU-accelerated egui desktop framework |
| `symphonia 0.5` | Pure-Rust audio decoding (replaces external FFmpeg) |
| `rustfft 6` | O(N log N) FFT for cross-correlation |
| `rayon 1.8` | Zero-friction data parallelism |

---

## License

MPL-2.0 (same as the underlying `symphonia` library).
