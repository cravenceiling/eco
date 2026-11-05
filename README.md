# eco

Dictate on your phone, transcribe on your computer.

eco is a small speech-to-text server for macOS and Linux. It runs a local Parakeet model with [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) and answers the OpenAI transcription API, so any app that speaks that API can use it. Point an Android dictation keyboard at it and every app on your phone gets dictation that never leaves your network.

On an M-series Mac, a 7-second clip goes from upload to text in about 0.1 seconds. While idle, eco uses about 165 MB of memory and no CPU.

If you use [Hex](https://github.com/kitlangton/Hex) on macOS, eco reuses the model Hex already downloaded. Hex itself doesn't need to be running.

## Install

You need Rust, CMake and ffmpeg. The first build compiles transcribe.cpp, which takes a few minutes.

```sh
# macOS
brew install cmake ffmpeg
# Debian/Ubuntu
sudo apt install cmake ffmpeg build-essential

cargo install --git https://github.com/cravenceiling/eco
```

macOS builds use Metal. Linux builds run on the CPU. For a GPU on Linux, add `--features transcribe-cpp/vulkan` or `--features transcribe-cpp/cuda`.

On ARM Linux (a Raspberry Pi 5, Graviton, a Linux VM on a Mac), GCC can fail on ggml's half-precision code with "inlining failed ... target specific option mismatch". Name the CPU features explicitly:

```sh
TRANSCRIBE_CMAKE_ARGS="-DGGML_NATIVE=OFF -DGGML_CPU_ARM_ARCH=armv8.2-a+fp16" cargo install --git https://github.com/cravenceiling/eco
```

## Set up

```sh
eco init
eco service install
eco pair
```

1. **`eco init`** writes `~/.config/eco/config.toml` without asking anything. It generates an API key, picks a model (Hex's copy if present, otherwise it downloads the 730 MB English Parakeet model), finds ffmpeg, and records your home network.
2. **`eco service install`** runs eco in the background and starts it at login. It uses launchd on macOS and a systemd user service on Linux. Running it again repairs and restarts the service.
3. **`eco pair`** prints the endpoint and API key for your phone, each with a QR code.

### On the phone

Any client for OpenAI-compatible transcription works. On Android, [Voice Keyboard](https://f-droid.org/packages/com.tyraen.voicekeyboard/) is a keyboard that sends dictation to a custom endpoint. In its settings, enter the endpoint and API key from `eco pair`; scanning each QR code with the camera app and pasting saves typing. The model field takes any value.

Give your computer a fixed address in your router's settings, so the endpoint keeps working.

## Commands

| Command | |
| --- | --- |
| `eco init` | Write a config. `--model PATH` picks a model, `--no-home-check` turns off the home-network check, `--force` replaces an existing config and its API key. |
| `eco pair` | Show the endpoint and API key. `--no-qr` skips the QR codes. |
| `eco service install` | Install and start the background service, or repair it. |
| `eco service status` | Show whether eco is running, answering, and at home. Exits 0 when answering, 1 when running but not answering, 3 when not running. |
| `eco service restart` | Restart, e.g. after editing the config. |
| `eco service uninstall` | Stop and remove the service. The config stays. |
| `eco serve` | Run in the foreground. |

Every command takes `--config PATH`.

## Configuration

`eco init` writes `$XDG_CONFIG_HOME/eco/config.toml`, or `~/.config/eco/config.toml`, readable only by you. Run `eco service restart` after editing it.

```toml
token = "..."
model = "/path/to/model.gguf"
listen = "0.0.0.0:8816"
ffmpeg = "/opt/homebrew/bin/ffmpeg"

[home_network]
subnet = "192.168.1.0/24"
router_mac = "aa:bb:cc:dd:ee:ff"
```

| Key | |
| --- | --- |
| `token` | The API key clients send as `Authorization: Bearer <token>`. |
| `model` | A transcribe.cpp GGUF model. Other models are on [Hugging Face](https://huggingface.co/handy-computer). |
| `listen` | Address and port. Set a specific address, such as a Tailscale IP, to limit where eco can be reached. |
| `ffmpeg` | Found on PATH when unset. Background services get a minimal PATH, so `eco init` writes the absolute path. |
| `home_network` | Optional. See below. |

### The home-network check

With `[home_network]` set, eco answers other devices only while your computer is on that network. Café and office networks often use the same `192.168.1.x` range, so eco also checks that your computer's current router has the hardware address in `router_mac`. On any other network eco closes connections before reading them. Requests from the computer itself are always allowed.

Remove the section to answer on every network, for example on a server or behind Tailscale, where devices connect from outside your home subnet. If you replace your router, run `eco init --force` or update `router_mac`.

## API

| Method | Path | |
| --- | --- | --- |
| `POST` | `/v1/audio/transcriptions` | Multipart with a `file` field holding any audio ffmpeg can read. Optional `response_format`: `json` (default), `text`, or `verbose_json`. |
| `GET` | `/v1/models` | Lists the loaded model. |
| `GET` | `/health` | Needs no key. |

```sh
curl -H "Authorization: Bearer $TOKEN" -F file=@note.m4a http://localhost:8816/v1/audio/transcriptions
```

## Network exposure

Traffic is plain HTTP. Other devices on your network could read the audio, the transcripts and the key. To encrypt traffic, or to dictate away from home, put eco behind something like Tailscale: set `listen` to the Tailscale address and remove `[home_network]`.

Uploads are limited to 50 MB, and audio longer than 15 minutes is cut off.

## License

MIT
