use super::DevServer;
use super::control::{self, Command, ControlServer};
use super::events::{Kind, RollingFile, Scope};
use super::protocol::{self, ClientMessage, ServerMessage};
use super::session::Session;
use serde_json::{Value, json};
use std::fs;
use std::net::TcpStream;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::sleep(Duration::from_millis(10));
    }
}

fn connect(server: &DevServer, token: &str) -> TcpStream {
    let mut socket = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    send(
        &mut socket,
        &ClientMessage::Hello {
            proto: 1,
            token: token.into(),
            project: "test".into(),
            pid: 123,
            platform: "test".into(),
            asset_reload: true,
        },
    );
    let reply: ServerMessage =
        protocol::decode(&protocol::read_frame(&mut socket).unwrap()).unwrap();
    assert!(matches!(reply, ServerMessage::HelloOk { .. }));
    socket
}

fn send(socket: &mut TcpStream, message: &ClientMessage) {
    protocol::write_frame(socket, &protocol::encode(message).unwrap()).unwrap();
}

#[test]
fn failed_build_keeps_running_identity_and_late_old_messages_cannot_change_new_run() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.rs"), "old").unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let first = session.begin_build().unwrap();
    let old = session.begin_run(&first);
    session.emit(Kind::AppStarted, &old, json!({"pid": 1}));
    first.finish(true, None);
    fs::write(dir.path().join("main.rs"), "broken").unwrap();
    let failed = session.begin_build().unwrap();
    session.emit(
        Kind::Diagnostic,
        &failed.scope,
        json!({"level": "error", "code": "E0308"}),
    );
    failed.finish(false, None);
    let state = session.store.state();
    assert!(state.stale());
    assert_eq!(state.running.unwrap().scope.run_id, old.run_id);
    assert_eq!(state.build.unwrap().status, "failed");
    assert_eq!(state.diagnostics[0]["diagnostic"]["code"], "E0308");

    fs::write(dir.path().join("main.rs"), "fixed").unwrap();
    let fixed = session.begin_build().unwrap();
    let new = session.begin_run(&fixed);
    session.emit(Kind::AppStarted, &new, json!({"pid": 2}));
    session.emit(Kind::AppConnected, &new, json!({"pid": 2}));
    fixed.finish(true, None);
    session.emit(Kind::AppPanic, &old, json!({"message": "late panic"}));
    session.emit(Kind::AppDisconnected, &old, json!({}));
    let state = session.store.state();
    assert!(!state.stale());
    assert!(state.diagnostics.is_empty());
    assert_eq!(state.running.as_ref().unwrap().scope.run_id, new.run_id);
    assert_eq!(state.running.as_ref().unwrap().channel, "connected");
    assert_eq!(state.running.unwrap().health, "unknown");
    assert_eq!(
        state.runtime_issues[0]["scope"]["run_id"],
        old.run_id.unwrap()
    );
}

#[test]
fn event_flood_is_bounded_and_expired_cursors_report_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    for i in 0..2200 {
        session.emit(
            Kind::AppLog,
            &Scope::default(),
            json!({"level": "info", "message": format!("message {i}")}),
        );
    }
    let expired = session.store.events(0, Duration::ZERO);
    assert!(expired.gap);
    assert!(expired.earliest_seq > 1);
    let mut cursor = expired.earliest_seq - 1;
    let mut count = 0;
    loop {
        let page = session.store.events(cursor, Duration::ZERO);
        assert!(!page.gap);
        for event in &page.events {
            assert_eq!(event.seq, cursor + 1);
            cursor = event.seq;
            count += 1;
        }
        assert!(serde_json::to_vec(&page).unwrap().len() < protocol::MAX_FRAME_LEN as usize);
        if !page.has_more {
            break;
        }
    }
    assert_eq!(count, 2048);
    assert_eq!(cursor, session.store.state().seq);
    session.emit(
        Kind::AppLog,
        &Scope::default(),
        json!({"message": "x".repeat(600_000)}),
    );
    let page = session.store.events(cursor, Duration::ZERO);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].data["payload_truncated"], true);
}

#[test]
fn journal_rotation_removes_old_segments_and_references_locate_records() {
    use std::io::{Read, Seek, SeekFrom};
    let dir = tempfile::tempdir().unwrap();
    let mut log = RollingFile::new(dir.path(), "output", 64, 2).unwrap();
    let first = log.append(br#"{"message":"first"}"#).unwrap();
    let mut last = first.clone();
    for _ in 0..20 {
        last = log.append(br#"{"message":"next"}"#).unwrap();
    }
    assert!(!first.path.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    let mut file = fs::File::open(&last.path).unwrap();
    file.seek(SeekFrom::Start(last.offset)).unwrap();
    let mut bytes = vec![0; last.bytes];
    file.read_exact(&mut bytes).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["message"],
        "next"
    );
    assert!(log.append(&[b'x'; 65]).is_err());
}

#[test]
fn control_authentication_selection_and_long_poll_are_independent_of_builds() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let build = session.begin_build().unwrap();
    let server = ControlServer::start(session.clone()).unwrap();
    let mut wrong = server.registration.clone();
    wrong.token = "wrong".into();
    let reply = control::request(&wrong, Command::Status).unwrap();
    assert_eq!(reply.error.unwrap().code, "unauthorized");
    let registration = server.registration.clone();
    let after = session.store.state().seq;
    let waiting = thread::spawn(move || {
        control::request(
            &registration,
            Command::Events {
                after,
                timeout_ms: 5000,
            },
        )
        .unwrap()
    });
    let status = control::request(&server.registration, Command::Status).unwrap();
    assert_eq!(status.result.unwrap()["build"]["status"], "building");
    session.emit(
        Kind::Diagnostic,
        &build.scope,
        json!({"level": "error", "code": "E0308"}),
    );
    let events = waiting.join().unwrap().result.unwrap();
    assert_eq!(events["events"][0]["kind"], "diagnostic");
    assert_eq!(events["events"][0]["seq"], after + 1);

    let other = Session::start(dir.path(), "test", "android:test").unwrap();
    let second = ControlServer::start(other.clone()).unwrap();
    assert_eq!(
        control::discover(dir.path(), None).unwrap_err().code,
        "ambiguous_session"
    );
    assert_eq!(
        control::discover(dir.path(), Some(&session.id))
            .unwrap()
            .session_id,
        session.id
    );
    drop(second);
    assert_eq!(
        control::discover(dir.path(), None).unwrap().session_id,
        session.id
    );
    let serialized = serde_json::to_string(&session.store.events(0, Duration::ZERO)).unwrap();
    assert!(!serialized.contains(&server.registration.token));
}

#[test]
fn legacy_launch_tokens_bind_logs_and_route_snapshot_replies_without_consuming_logs() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let server = Arc::new(DevServer::start_observed(session.clone()).unwrap());
    let first = session.begin_build().unwrap();
    let old = session.begin_run(&first);
    let old_token = server.expect_run(old.clone()).unwrap();
    let mut old_socket = connect(&server, &old_token);
    assert!(server.wait_for_client(Duration::from_secs(1)));
    let next = session.begin_build().unwrap();
    let new = session.begin_run(&next);
    let new_token = server.expect_run(new.clone()).unwrap();
    let mut new_socket = connect(&server, &new_token);
    assert!(server.wait_for_client(Duration::from_secs(1)));
    let requester = server.clone();
    let (tx, rx) = mpsc::channel();
    let request = thread::spawn(move || {
        tx.send(requester.save_state("same-request", Duration::from_secs(3)))
            .unwrap();
    });
    let command: ServerMessage =
        protocol::decode(&protocol::read_frame(&mut new_socket).unwrap()).unwrap();
    assert!(matches!(command, ServerMessage::PrepareRestart { .. }));
    send(
        &mut old_socket,
        &ClientMessage::StateSaved {
            session: "same-request".into(),
            data: "old".into(),
        },
    );
    send(
        &mut old_socket,
        &ClientMessage::Panic {
            message: "old crash".into(),
            location: "old.rs:1".into(),
            backtrace: "trace".into(),
        },
    );
    wait_until(|| !session.store.state().runtime_issues.is_empty());
    assert!(rx.try_recv().is_err());
    assert_eq!(session.store.state().running.unwrap().health, "unknown");
    send(
        &mut new_socket,
        &ClientMessage::Log {
            level: "error".into(),
            target: "save".into(),
            message: "save failed".into(),
        },
    );
    send(
        &mut new_socket,
        &ClientMessage::StateSaved {
            session: "same-request".into(),
            data: "new".into(),
        },
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)).unwrap().as_deref(),
        Some("new")
    );
    request.join().unwrap();
    wait_until(|| session.store.state().runtime_issues.len() == 2);
    let events = serde_json::to_string(&session.store.events(0, Duration::ZERO)).unwrap();
    assert!(!events.contains(&old_token) && !events.contains(&new_token));
    assert!(events.contains("save failed"));
}

#[cfg(unix)]
#[test]
fn compiler_diagnostics_arrive_before_exit_and_capture_both_pipes() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let build = session.begin_build().unwrap();
    let gate = session.dir.join("continue");
    let release = gate.clone();
    let worker = thread::spawn(move || {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", r#"printf '%s\n' '{"reason":"compiler-message","message":{"level":"error","message":"live error","spans":[]}}'; printf 'stderr before exit\n' >&2; while [ ! -f "$1" ]; do sleep 0.01; done; exit 1"#, "fixture"]).arg(release);
        super::output::run(&mut command, &build, "fixture", true).unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.store.state().diagnostics.is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    // Always release the child, even if an assertion below fails.
    let before_exit = !worker.is_finished();
    fs::write(gate, "continue").unwrap();
    assert!(!worker.join().unwrap().success);
    assert!(before_exit);
    assert_eq!(
        session.store.state().diagnostics[0]["diagnostic"]["message"],
        "live error"
    );
    assert!(session.store.state().diagnostics[0]["diagnostic"]["file"].is_null());
    assert_eq!(
        session.store.state().build.unwrap().last_output["stderr"]["text"],
        "stderr before exit\n"
    );
    let events = session.store.events(0, Duration::ZERO);
    assert!(
        events
            .events
            .iter()
            .any(|e| e.kind == Kind::Output && e.data["stream"] == "stderr")
    );
}
