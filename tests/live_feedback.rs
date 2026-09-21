//! Exercise the shipped CLI against real Cargo builds, without a display/GPU.
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    child: Child,
}

impl Fixture {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir_all(root.join("crates/desktop/src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/desktop\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        fs::write(
            root.join("gpui.toml"),
            "[app]\nname = \"feedback-probe\"\ntitle = \"Feedback probe\"\n",
        )
        .unwrap();
        fs::write(root.join("crates/desktop/Cargo.toml"), "[package]\nname = \"feedback-probe-desktop\"\nversion = \"0.1.0\"\nedition = \"2024\"\n").unwrap();
        fs::write(
            root.join("crates/desktop/src/main.rs"),
            good_source("first"),
        )
        .unwrap();
        let trace = fs::File::create(dir.path().join("live.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_gpui"))
            .current_dir(&root)
            .args(["run", "--live"])
            .env("CARGO_NET_OFFLINE", "true")
            .env_remove("CARGO_TARGET_DIR")
            .stdin(Stdio::piped())
            .stdout(trace.try_clone().unwrap())
            .stderr(trace)
            .spawn()
            .unwrap();
        Self { dir, root, child }
    }

    fn query(&self, command: &[&str]) -> Value {
        query(&self.root, command)
    }

    fn wait(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "live exited: {}",
                self.trace()
            );
            let value = self.query(&["status", "--json"]);
            if predicate(&value["result"]) {
                return value["result"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "last status: {value}; trace: {}",
                self.trace()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn source(&self, source: &str) {
        fs::write(self.root.join("crates/desktop/src/main.rs"), source).unwrap();
    }

    fn trace(&self) -> String {
        fs::read_to_string(self.dir.path().join("live.log")).unwrap_or_default()
    }

    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_some() {
            return;
        }
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = stdin.write_all(b"q\n");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn good_source(marker: &str) -> String {
    format!(
        "fn main() {{ println!(\"{marker}\"); loop {{ std::thread::sleep(std::time::Duration::from_millis(50)); }} }}\n"
    )
}

fn query(root: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(root)
        .arg("dev")
        .args(args)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(output.status.success(), value["ok"].as_bool().unwrap());
    value
}

#[test]
fn real_live_build_failure_recovery_supersession_crash_and_event_follow() {
    let mut fixture = Fixture::start();
    let initial = fixture
        .wait(|s| s["build"]["status"] == "succeeded" && s["running"]["process"] == "running");
    let run = initial["running"]["run_id"].clone();
    let pid = initial["running"]["pid"].clone();
    assert_eq!(initial["stale"], false);
    assert_eq!(initial["capabilities"]["ui_observation"], false);

    fixture.source("fn main() { let _: u32 = \"wrong\"; }\n");
    let failed = fixture.wait(|s| s["build"]["status"] == "failed");
    assert_eq!(failed["running"]["run_id"], run);
    assert_eq!(failed["running"]["pid"], pid);
    assert_eq!(failed["stale"], true);
    let diagnostics = fixture.query(&["diagnostics", "--json"]);
    let records = diagnostics["result"]["diagnostics"].as_array().unwrap();
    let error = records
        .iter()
        .find(|d| d["diagnostic"]["code"] == "E0308")
        .unwrap();
    assert!(
        error["diagnostic"]["spans"]
            .as_array()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(
        error["scope"]["source_revision"],
        failed["desired"]["source_revision"]
    );
    assert!(
        error["diagnostic"]["log"]["path"]
            .as_str()
            .is_some_and(|p| Path::new(p).exists())
    );

    // Hold the next real build open. Queries must work while the compiler runs,
    // and an edit made during that build must supersede its original input.
    fs::write(fixture.root.join("crates/desktop/build.rs"), r#"
fn main() {
    println!("cargo:rerun-if-changed=src/main.rs");
    let gate = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../.gpui/release-build");
    while !gate.exists() { std::thread::sleep(std::time::Duration::from_millis(20)); }
}
"#).unwrap();
    fixture.source(&good_source("intermediate"));
    let building = fixture.wait(|s| s["build"]["status"] == "building");
    assert_eq!(building["running"]["run_id"], run);
    fixture.source(&good_source("latest"));
    fs::write(fixture.root.join(".gpui/release-build"), "continue").unwrap();
    let recovered = fixture.wait(|s| {
        s["build"]["status"] == "succeeded" && s["running"]["run_id"] != run && s["stale"] == false
    });
    assert!(
        recovered["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["diagnostic"]["level"] != "error")
    );
    assert_eq!(
        recovered["desired"]["source_revision"],
        recovered["running"]["source_revision"]
    );
    let events = fixture.query(&["events", "--json"]);
    let events = events["result"]["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["kind"] == "build.superseded"));
    assert!(
        events
            .iter()
            .any(|e| e["kind"] == "app.exited" && e["data"]["expected"] == true)
    );

    // A panic before a dev-channel handshake is still visible via process exit
    // and the captured stderr, rather than disappearing with the socket.
    fixture
        .source("fn main() { eprintln!(\"early runtime error\"); panic!(\"runtime failure\"); }\n");
    fixture.wait(|s| {
        s["running"]["process"] == "exited"
            && s["running"]["run_id"] != recovered["running"]["run_id"]
    });
    let problems = fixture.query(&["diagnostics", "--json"]);
    assert!(
        problems["result"]["runtime_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "app.exited")
    );
    assert!(fixture.trace().contains("early runtime error"));

    let seq = fixture.query(&["status", "--json"])["result"]["seq"]
        .as_u64()
        .unwrap();
    let follow_file = fixture.dir.path().join("follow.ndjson");
    let follow_err = fixture.dir.path().join("follow.err");
    let mut follower = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(&fixture.root)
        .args([
            "dev",
            "events",
            "--follow",
            "--json",
            "--after",
            &seq.to_string(),
        ])
        .stdout(fs::File::create(&follow_file).unwrap())
        .stderr(fs::File::create(&follow_err).unwrap())
        .spawn()
        .unwrap();
    // Wait until it has received a new event, proving the subscription attached.
    fixture.source(&good_source("restored"));
    fixture.wait(|s| s["running"]["process"] == "running" && s["stale"] == false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::metadata(&follow_file).unwrap().len() == 0 {
        assert!(Instant::now() < deadline, "event follower did not attach");
        thread::sleep(Duration::from_millis(25));
    }
    fixture.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = follower.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = follower.kill();
            panic!("event follower did not stop");
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert!(
        status.success(),
        "event follower failed: {}",
        fs::read_to_string(&follow_err).unwrap_or_default()
    );
    let lines = fs::read_to_string(&follow_file).unwrap();
    let values: Vec<Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(values.iter().any(|v| v["kind"] == "session.ended"));
    assert!(
        values
            .windows(2)
            .all(|w| w[0]["seq"].as_u64() < w[1]["seq"].as_u64())
    );
    assert_eq!(
        fixture.query(&["status", "--json"])["error"]["code"],
        "session_unavailable"
    );
}
