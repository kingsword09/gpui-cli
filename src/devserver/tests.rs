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
            proto: 2,
            token: token.into(),
            project: "test".into(),
            pid: 123,
            platform: "test".into(),
            asset_reload: true,
            runtime_version: None,
            gpui_version: None,
            capabilities: Vec::new(),
        },
    );
    let reply: ServerMessage =
        protocol::decode(&protocol::read_frame(&mut socket).unwrap()).unwrap();
    assert!(matches!(reply, ServerMessage::HelloOk { .. }));
    socket
}

#[test]
fn registration_advertises_current_control_schema() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let server = ControlServer::start(session).unwrap();
    assert_eq!(server.registration.schema_version, 2);
}

#[test]
fn current_app_channel_routes_window_and_ui_probe_events() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let server = Arc::new(DevServer::start_observed(session.clone()).unwrap());
    let build = session.begin_build().unwrap();
    let run = session.begin_run(&build);
    let token = server.expect_run(run.clone()).unwrap();
    let mut socket = connect(&server, &token);

    send(
        &mut socket,
        &ClientMessage::WindowRegistered {
            window_id: "w-main".into(),
            title: "Counter".into(),
            width: 800,
            height: 600,
            scale_milli: 2000,
            foreground: true,
        },
    );
    send(
        &mut socket,
        &ClientMessage::UiProbeResult {
            request_id: "probe.1".into(),
            window_id: "w-main".into(),
            responsive: true,
            latency_ms: Some(4),
        },
    );
    send(
        &mut socket,
        &ClientMessage::WindowClosed {
            window_id: "w-main".into(),
            reason: Some("user".into()),
        },
    );
    send(
        &mut socket,
        &ClientMessage::AssetsApplied {
            transfer_id: "asset-t1-r0".into(),
            asset_revision: 0,
            applied: vec!["assets/x.png".into()],
            failed: Vec::new(),
            cache_invalidated: true,
        },
    );

    wait_until(|| {
        let events = session.store.events(0, Duration::ZERO);
        events
            .events
            .iter()
            .any(|event| event.kind == Kind::UiProbeResult && event.data["request_id"] == "probe.1")
            && events
                .events
                .iter()
                .any(|event| event.kind == Kind::WindowClosed)
            && events
                .events
                .iter()
                .any(|event| event.kind == Kind::AssetsApplied)
    });
    let events = session.store.events(0, Duration::ZERO);
    assert!(events.events.iter().any(|event| {
        event.kind == Kind::WindowRegistered && event.data["scale_milli"] == 2000
    }));
    assert!(
        session
            .store
            .state()
            .running
            .is_some_and(|run| run.assets_confirmed)
    );
}

#[test]
fn current_app_channel_queues_accepted_asset_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let server = Arc::new(DevServer::start_observed(session.clone()).unwrap());
    let build = session.begin_build().unwrap();
    let run = session.begin_run(&build);
    let token = server.expect_run(run.clone()).unwrap();
    assert!(server.set_asset_manifest(
        run.revision.asset_revision,
        vec![protocol::AssetManifestEntry {
            path: "assets/logo.png".into(),
            hash: "hash".into(),
        }]
    ));
    let transfer_id = server.current_asset_manifest().unwrap().0;
    let mut socket = connect(&server, &token);

    send(
        &mut socket,
        &ClientMessage::AssetsReconciled {
            transfer_id,
            asset_revision: run.revision.asset_revision,
            present: vec![],
            missing: vec!["assets/logo.png".into()],
            stale: vec![],
            removed: vec![],
        },
    );
    wait_until(|| {
        session
            .store
            .events(0, Duration::ZERO)
            .events
            .iter()
            .any(|event| event.kind == Kind::AssetsReconciled)
    });
    let reconciliations = server.take_asset_reconciliations();
    assert_eq!(reconciliations.len(), 1);
    assert_eq!(reconciliations[0].connection_id, 0);
    assert_eq!(reconciliations[0].missing, vec!["assets/logo.png"]);
}

#[test]
fn windows_control_query_returns_run_scoped_registration() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let control = ControlServer::start(session.clone()).unwrap();
    let server = Arc::new(DevServer::start_observed(session.clone()).unwrap());
    let build = session.begin_build().unwrap();
    let run = session.begin_run(&build);
    let token = server.expect_run(run.clone()).unwrap();
    let mut socket = connect(&server, &token);

    send(
        &mut socket,
        &ClientMessage::WindowRegistered {
            window_id: "w-main".into(),
            title: "Counter".into(),
            width: 800,
            height: 600,
            scale_milli: 1000,
            foreground: true,
        },
    );
    wait_until(|| {
        !session
            .windows
            .snapshots(Some(run.run_id.as_deref().unwrap()))
            .is_empty()
    });

    let probe: ServerMessage =
        protocol::decode(&protocol::read_frame(&mut socket).unwrap()).unwrap();
    let (request_id, window_id) = match probe {
        ServerMessage::ProbeUi {
            request_id,
            window_id,
        } => (request_id, window_id),
        other => panic!("expected heartbeat probe, got {other:?}"),
    };
    assert_eq!(window_id, "w-main");
    send(
        &mut socket,
        &ClientMessage::UiProbeResult {
            request_id,
            window_id,
            responsive: true,
            latency_ms: Some(2),
        },
    );
    wait_until(|| {
        session
            .windows
            .snapshots(Some(run.run_id.as_deref().unwrap()))
            .first()
            .is_some_and(|window| window.ui == "responsive")
    });

    let reply = control::request(&control.registration, "test.windows", Command::Windows).unwrap();
    let result = reply.result.unwrap();
    assert_eq!(result["run_id"], run.run_id.clone().unwrap());
    assert_eq!(result["windows"][0]["window_id"], "w-main");
    assert_eq!(result["windows"][0]["ui"], "responsive");
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
fn a_follower_recovers_the_closing_events_from_the_journal_after_the_supervisor_exits() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    for index in 0..3 {
        session.emit(
            Kind::AppLog,
            &Scope::default(),
            json!({"level": "info", "message": format!("before {index}")}),
        );
    }
    // A follower that already consumed these is only waiting for what comes next.
    let after = session.store.state().seq;
    session.end();
    // The supervisor is gone, but the journal still says the session ended — the
    // follower replays exactly the unseen tail instead of failing the command.
    let archived = super::events::archived(&session.dir).unwrap();
    assert_eq!(
        archived.last().map(|event| event.kind),
        Some(Kind::SessionEnded)
    );
    let unseen: Vec<u64> = archived
        .iter()
        .filter(|event| event.seq > after)
        .map(|event| event.seq)
        .collect();
    assert_eq!(unseen, vec![after + 1]);
}

#[test]
fn journal_segments_are_read_in_seq_order_across_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    // Rotate: the event journal holds 8 segments, so this rolls several times.
    for index in 0..9000 {
        session.emit(
            Kind::AppLog,
            &Scope::default(),
            json!({"level": "info", "message": format!("message {index}")}),
        );
    }
    let archived = super::events::archived(&session.dir).unwrap();
    assert!(archived.len() > 100, "expected a multi-segment journal");
    assert!(
        archived.windows(2).all(|pair| pair[0].seq < pair[1].seq),
        "segments must be concatenated in seq order"
    );
}

#[test]
fn control_authentication_selection_and_long_poll_are_independent_of_builds() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let build = session.begin_build().unwrap();
    let server = ControlServer::start(session.clone()).unwrap();
    let mut wrong = server.registration.clone();
    wrong.token = "wrong".into();
    let reply = control::request(&wrong, "test.status", Command::Status).unwrap();
    assert_eq!(reply.error.unwrap().code, "unauthorized");
    let registration = server.registration.clone();
    let after = session.store.state().seq;
    let waiting = thread::spawn(move || {
        control::request(
            &registration,
            "test.events",
            Command::Events {
                after,
                timeout_ms: 5000,
            },
        )
        .unwrap()
    });
    let status = control::request(&server.registration, "test.status", Command::Status).unwrap();
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

// Also runs as a subprocess to reproduce a leader exiting with a helper
// still holding both inherited output pipes. No shell/platform tools needed.
#[test]
#[allow(clippy::zombie_processes)] // The supervisor under test must own cleanup.
fn process_tree_fixture() {
    match std::env::var("GPUI_PROCESS_FIXTURE").as_deref() {
        Ok("helper") => thread::sleep(Duration::from_secs(10)),
        Ok("parent") => {
            process_tree_command("helper").spawn().unwrap();
            println!("parent stdout before exit");
            eprintln!("parent stderr before exit");
            std::process::exit(7);
        }
        _ => {}
    }
}

fn process_tree_command(role: &str) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "devserver::tests::process_tree_fixture",
            "--nocapture",
        ])
        .env("GPUI_PROCESS_FIXTURE", role);
    command
}

#[test]
fn build_completion_cleans_up_helpers_holding_output_pipes() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let build = session.begin_build().unwrap();
    let start = Instant::now();
    let outcome = super::output::run(
        &mut process_tree_command("parent"),
        &build,
        "fixture",
        false,
    )
    .unwrap();
    assert!(!outcome.success);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "waited for the helper instead of cleaning it up"
    );
    let state = session.store.state();
    assert!(
        state.build.unwrap().last_output["stderr"]["text"]
            .as_str()
            .unwrap()
            .contains("parent stderr")
    );
    let finished = session
        .store
        .events(0, Duration::ZERO)
        .events
        .into_iter()
        .find(|event| event.kind == Kind::StageFinished)
        .unwrap();
    assert_eq!(finished.data["exit_code"], 7);
}

#[test]
fn app_exit_and_drop_do_not_wait_for_orphaned_helpers() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::start(dir.path(), "test", "desktop:test").unwrap();
    let build = session.begin_build().unwrap();
    let scope = session.begin_run(&build);
    let start = Instant::now();
    let app = super::output::AppProcess::spawn(
        &mut process_tree_command("parent"),
        session.clone(),
        scope,
    )
    .unwrap();
    wait_until(|| {
        session
            .store
            .state()
            .running
            .is_some_and(|run| run.process == "exited")
    });
    assert_eq!(
        session.store.state().running.unwrap().exit.unwrap()["exit_code"],
        7
    );
    drop(app);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "app cleanup waited for the helper"
    );
}
