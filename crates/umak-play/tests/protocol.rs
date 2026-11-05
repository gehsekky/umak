//! Drives the real executable over its JSON protocol, with no sound.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);

fn wav(path: &Path, seconds: u32) {
    let rate = 44_100_u32;
    let mut samples = Vec::new();
    for frame in 0..rate * seconds {
        let value = (frame as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin();
        let sample = (value * 8000.0) as i16;
        samples.extend_from_slice(&sample.to_le_bytes());
        samples.extend_from_slice(&sample.to_le_bytes());
    }
    let mut file = b"RIFF".to_vec();
    file.extend((36 + samples.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend(16_u32.to_le_bytes());
    file.extend(1_u16.to_le_bytes());
    file.extend(2_u16.to_le_bytes());
    file.extend(rate.to_le_bytes());
    file.extend((rate * 4).to_le_bytes());
    file.extend(4_u16.to_le_bytes());
    file.extend(16_u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend((samples.len() as u32).to_le_bytes());
    file.extend(samples);
    fs::write(path, file).unwrap();
}

struct Player {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Value>,
}

fn player() -> Player {
    let mut child = Command::new(env!("CARGO_BIN_EXE_umak-play"))
        .arg("--null-output")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let (sender, events) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let _ = sender.send(serde_json::from_str::<Value>(&line).unwrap());
        }
    });
    Player {
        child,
        stdin,
        events,
    }
}

impl Player {
    fn send(&mut self, command: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{command}").unwrap();
        stdin.flush().unwrap();
    }

    fn wait_for(&self, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self.events.recv_timeout(left).expect("timed out");
            if wanted(&event) {
                return event;
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_exit(child: &mut Child) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn commands_control_playback_and_events_report_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    wav(&path, 4);
    let mut player = player();

    let ready = player.wait_for(|event| event["type"] == "ready");
    assert_eq!(ready["sampleRate"], 48_000);
    assert_eq!(ready["device"], "null");

    player.send(json!({
        "type": "load",
        "tracks": [{ "id": "track-1", "path": path }],
        "startIndex": 0,
        "startMs": 0,
    }));
    let playing = player.wait_for(|event| event["type"] == "state" && event["state"] == "playing");
    assert_eq!(playing["trackId"], "track-1");
    assert_eq!(playing["index"], 0);
    assert_eq!(playing["durationMs"], 4000);

    player.wait_for(|event| event["type"] == "position");

    player.send(json!({ "type": "pause" }));
    player.wait_for(|event| event["state"] == "paused");
    player.send(json!({ "type": "seek", "ms": 3500 }));
    let seeked = player.wait_for(|event| event["type"] == "state");
    assert_eq!(seeked["positionMs"], 3500);
    player.send(json!({ "type": "play" }));
    player.wait_for(|event| event["state"] == "playing");

    // Half a second left, in real time.
    let stopped = player.wait_for(|event| event["state"] == "stopped");
    assert_eq!(stopped["trackId"], Value::Null);
}

#[test]
fn a_bad_command_is_reported_and_the_player_keeps_working() {
    let mut player = player();
    player.wait_for(|event| event["type"] == "ready");
    let stdin = player.stdin.as_mut().unwrap();
    writeln!(stdin, "this is not json").unwrap();
    stdin.flush().unwrap();
    let error = player.wait_for(|event| event["type"] == "error");
    assert!(error["message"].as_str().unwrap().contains("bad command"));

    player.send(json!({ "type": "stop" }));
    player.wait_for(|event| event["state"] == "stopped");
}

#[test]
fn the_player_exits_when_its_stdin_closes_or_on_quit() {
    let mut first = player();
    first.wait_for(|event| event["type"] == "ready");
    first.stdin = None;
    assert!(
        wait_for_exit(&mut first.child),
        "still running after stdin closed"
    );

    let mut second = player();
    second.wait_for(|event| event["type"] == "ready");
    second.send(json!({ "type": "quit" }));
    assert!(wait_for_exit(&mut second.child), "still running after quit");
}

#[test]
fn check_reports_each_file_and_fails_if_any_is_unplayable() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.wav");
    wav(&good, 1);
    let bad = dir.path().join("bad.mp3");
    fs::write(&bad, b"not audio").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_umak-play"))
        .arg("--check")
        .args([&good, &bad])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["ok"], true);
    assert_eq!(lines[0]["sampleRate"], 44_100);
    assert_eq!(lines[0]["frames"], 44_100);
    assert_eq!(lines[1]["ok"], false);
}

#[test]
fn files_on_the_command_line_play_through_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    wav(&path, 1);
    let output = Command::new(env!("CARGO_BIN_EXE_umak-play"))
        .arg("--null-output")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\"state\":\"playing\""), "{text}");
    assert!(text.contains("\"state\":\"stopped\""), "{text}");
}
