# VRChat TTS For Mute

A lightweight Text-to-Speech (TTS) application for mute players in VRChat.

## Features
- Windows offline TTS mode (native SAPI voices).
- Edge-TTS mode (built-in online synthesis, no Python dependency required).
- OpenAI-compatible remote TTS mode (`/v1/audio/speech` style endpoint).
- Output-device routing for VB-CABLE.
- Fast refocus hotkey (`Right Shift`).

## Prerequisites
- OS: Windows.
- Audio routing: [VB-CABLE](https://vb-audio.com/Cable/index.htm).

## Modes
Open `Settings` and choose one mode:
- `Windows Offline`: uses local Windows voices and local rate/volume controls.
- `Edge TTS`: uses Microsoft Edge online voices, with Edge voice/rate/volume/pitch controls.
- `OpenAI-Compatible`: uses a configurable remote endpoint, API key, model, voice, and speed.

## How To Use
1. Download and unzip the app.
2. Run the `.exe`.
3. Set app output to `CABLE Input`.
4. In VRChat, set microphone input to `CABLE Output`.
5. Use `Right Shift` in-game to refocus the app quickly.
6. Open `Settings` to choose mode and tune parameters.

## Controls
| Key | Function |
| :--- | :--- |
| `Right Shift` | Refocus application window |
