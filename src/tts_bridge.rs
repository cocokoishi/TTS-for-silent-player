use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

const TTS_SCRIPT: &str = r#"
[Console]::InputEncoding = [System.Text.Encoding]::UTF8
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$voice = New-Object -ComObject SAPI.SpVoice

function Get-AudioOutputs {
    $cat = New-Object -ComObject SAPI.SpObjectTokenCategory
    $cat.SetId("HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\AudioOutput")
    $tokens = $cat.EnumerateTokens()
    $results = @()
    for ($i = 0; $i -lt $tokens.Count; $i++) {
        $results += $tokens.Item($i).GetDescription()
    }
    return $results
}

function Get-Voices {
    $cat = New-Object -ComObject SAPI.SpObjectTokenCategory
    $cat.SetId("HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices")
    $tokens = $cat.EnumerateTokens()
    $results = @()
    for ($i = 0; $i -lt $tokens.Count; $i++) {
        $results += $tokens.Item($i).GetDescription()
    }
    return $results
}

function Set-AudioOutput($name) {
    if ([string]::IsNullOrWhiteSpace($name)) { return }
    $cat = New-Object -ComObject SAPI.SpObjectTokenCategory
    $cat.SetId("HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\AudioOutput")
    $tokens = $cat.EnumerateTokens()
    for ($i = 0; $i -lt $tokens.Count; $i++) {
        $token = $tokens.Item($i)
        if ($token.GetDescription() -eq $name) {
            $voice.AudioOutput = $token
            return
        }
    }
    for ($i = 0; $i -lt $tokens.Count; $i++) {
        $token = $tokens.Item($i)
        if ($token.GetDescription().ToLower().Contains($name.ToLower())) {
            $voice.AudioOutput = $token
            return
        }
    }
}

function Set-Voice($name) {
    if ([string]::IsNullOrWhiteSpace($name)) { return }
    $cat = New-Object -ComObject SAPI.SpObjectTokenCategory
    $cat.SetId("HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices")
    $tokens = $cat.EnumerateTokens()
    for ($i = 0; $i -lt $tokens.Count; $i++) {
        $token = $tokens.Item($i)
        if ($token.GetDescription() -eq $name) {
            $voice.Voice = $token
            return
        }
    }
    for ($i = 0; $i -lt $tokens.Count; $i++) {
        $token = $tokens.Item($i)
        if ($token.GetDescription().ToLower().Contains($name.ToLower())) {
            $voice.Voice = $token
            return
        }
    }
}

while ($true) {
    $line = [Console]::ReadLine()
    if ($null -eq $line) { break }
    try {
        $cmd = $line | ConvertFrom-Json
        $r = @{ok=$true}
        switch ($cmd.a) {
            "speak"   { $voice.Speak($cmd.t, 1) | Out-Null }
            "stop"    { $voice.Speak("", 3) | Out-Null }
            "rate"    { $voice.Rate = $cmd.v }
            "vol"     { $voice.Volume = $cmd.v }
            "dev"     { Set-AudioOutput $cmd.v }
            "voice"   { Set-Voice $cmd.v }
            "voices"  { $r.voices = @(Get-Voices) }
            "devs"    { $r.devs = @(Get-AudioOutputs) }
            "status"  { $r.s = ($voice.Status.RunningState -eq 2) }
        }
        Write-Output (ConvertTo-Json $r -Compress)
    } catch {
        Write-Output (ConvertTo-Json @{ok=$false;e=$_.Exception.Message} -Compress)
    }
}
"#;

#[derive(Debug)]
pub enum TtsCommand {
    Speak(String),
    Stop,
    SetRate(i32),
    SetVolume(i32),
    SetDevice(String),
    SetVoice(String),
    ListVoices,
    ListDevices,
    QueryStatus,
}

#[derive(Debug, Clone)]
pub enum TtsEvent {
    SpeakingState(bool),
    Voices(Vec<String>),
    Devices(Vec<String>),
    Error(String),
    Ready,
}

pub struct TtsBridge {
    pub cmd_tx: mpsc::Sender<TtsCommand>,
    pub event_rx: mpsc::Receiver<TtsEvent>,
}

impl TtsBridge {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TtsCommand>();
        let (event_tx, event_rx) = mpsc::channel::<TtsEvent>();

        thread::spawn(move || {
            Self::run_bridge(cmd_rx, event_tx);
        });

        Self { cmd_tx, event_rx }
    }

    fn run_bridge(cmd_rx: mpsc::Receiver<TtsCommand>, event_tx: mpsc::Sender<TtsEvent>) {
        // Write script to temp file
        let script_path = std::env::temp_dir().join("mugen_tts_bridge.ps1");
        if std::fs::write(&script_path, TTS_SCRIPT).is_err() {
            let _ = event_tx.send(TtsEvent::Error("Failed to write TTS script".into()));
            return;
        }

        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &script_path.to_string_lossy(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        // Hide the PowerShell console window on Windows
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(TtsEvent::Error(format!("Failed to start PowerShell: {e}")));
                return;
            }
        };

        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        // Reader thread for stdout
        let ev_tx2 = event_tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(voices) = val.get("voices").and_then(|d| d.as_array()) {
                        let list: Vec<String> = voices
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        let _ = ev_tx2.send(TtsEvent::Voices(list));
                    } else if let Some(devs) = val.get("devs").and_then(|d| d.as_array()) {
                        let list: Vec<String> = devs
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        let _ = ev_tx2.send(TtsEvent::Devices(list));
                    } else if let Some(speaking) = val.get("s").and_then(|s| s.as_bool()) {
                        let _ = ev_tx2.send(TtsEvent::SpeakingState(speaking));
                    } else if let Some(err) = val.get("e").and_then(|e| e.as_str()) {
                        let _ = ev_tx2.send(TtsEvent::Error(err.to_string()));
                    }
                }
            }
        });

        let _ = event_tx.send(TtsEvent::Ready);

        // Process commands
        while let Ok(cmd) = cmd_rx.recv() {
            let json = match cmd {
                TtsCommand::Speak(text) => {
                    let escaped = text
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t");
                    format!("{{\"a\":\"speak\",\"t\":\"{escaped}\"}}")
                }
                TtsCommand::Stop => r#"{"a":"stop"}"#.to_string(),
                TtsCommand::SetRate(r) => format!("{{\"a\":\"rate\",\"v\":{r}}}"),
                TtsCommand::SetVolume(v) => format!("{{\"a\":\"vol\",\"v\":{v}}}"),
                TtsCommand::SetDevice(d) => {
                    format!("{{\"a\":\"dev\",\"v\":\"{d}\"}}")
                }
                TtsCommand::SetVoice(v) => {
                    format!("{{\"a\":\"voice\",\"v\":\"{v}\"}}")
                }
                TtsCommand::ListVoices => r#"{"a":"voices"}"#.to_string(),
                TtsCommand::ListDevices => r#"{"a":"devs"}"#.to_string(),
                TtsCommand::QueryStatus => r#"{"a":"status"}"#.to_string(),
            };
            if writeln!(stdin, "{json}").is_err() {
                break;
            }
            let _ = stdin.flush();
        }
    }

    pub fn send(&self, cmd: TtsCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn poll_events(&self) -> Vec<TtsEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }
}
