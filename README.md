# VRChat TTS For Mute / Mugen TTS

A lightweight Windows TTS companion for mute and semi-mute VRChat players.

Mugen TTS lets you type text, play synthesized speech through a virtual audio device such as VB-CABLE, and use that audio as your microphone input in VRChat. It supports fully offline local voices, built-in Edge TTS, and OpenAI-compatible remote TTS endpoints in one small desktop app.

## Highlights

- Built for Windows and easy VRChat voice routing
- `Windows Offline` mode using native SAPI voices
- `Edge TTS` mode with built-in online synthesis and no Python dependency
- `OpenAI-Compatible` mode for `/v1/audio/speech` style APIs
- Output device selection for VB-CABLE and similar virtual devices
- Fast app refocus with `Right Shift`
- Optional `VRChat OSC` chatbox sync for recent spoken lines
- Handy desktop controls such as opacity, always-on-top, and enter-to-speak mode

## Requirements

- Windows
- A virtual audio cable such as [VB-CABLE](https://vb-audio.com/Cable/index.htm)

## Quick Start

1. Download and extract the application.
2. Run the `.exe`.
3. Open `Settings`.
4. Choose your TTS mode.
5. Set the app output device to `CABLE Input` or your preferred virtual output device.
6. In VRChat, set your microphone input to `CABLE Output`.
7. Type text in the app and press `Enter` to speak.
8. Use `Right Shift` in-game to bring the app back to focus quickly.

## TTS Modes

### `Windows Offline`

Uses local Windows SAPI voices.

- Works without an internet connection
- Voice selection
- Local rate control
- Local volume control

### `Edge TTS`

Uses Microsoft Edge online voices with built-in synthesis.

- No Python setup required
- Large voice selection
- Adjustable voice, rate, volume, and pitch
- A good default option for natural-sounding speech

### `OpenAI-Compatible`

Uses a configurable remote TTS endpoint compatible with the `/v1/audio/speech` request style.

- Custom API URL
- API key support
- Configurable model
- Configurable voice
- Adjustable speed

## VRChat Setup

The most common routing setup is:

1. Mugen TTS output -> `CABLE Input`
2. VRChat microphone input -> `CABLE Output`

This allows the app's generated speech to be heard in VRChat as your microphone audio.

## Controls

| Key | Action |
| :--- | :--- |
| `Right Shift` | Refocus the application window |

## Configuration

Settings are saved automatically to `settings.json` next to the executable.

This makes the app easy to keep portable: move the executable and its settings together if needed.
