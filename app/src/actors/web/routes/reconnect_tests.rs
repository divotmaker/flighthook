//! Tests of `POST /api/actors/{id}/reconnect`.

use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::super::tests::test_state;
use super::*;
use crate::actors::Actor as _;
use crate::bus::{BusReceiver, BusSender};
use crate::state::SystemState;

/// A web state whose config holds a mock monitor and a camera.
fn configured() -> (
    Arc<WebState>,
    crate::state::SessionWriter,
    super::super::tests::TempDir,
) {
    let (state, writer, dir) = test_state();
    state.root.system.update(|c| {
        c.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                name: "Mock".into(),
                enabled: true,
                actor_id: None,
                impact_lead_s: None,
                driver: Default::default(),
            },
        );
        c.camera
            .insert("0".into(), flighthook::CameraSection::default());
    });
    (Arc::new(state), writer, dir)
}

/// Stand-in for the system actor: answers every reconnect after
/// `delay`, as restarted unless `refuse`. Counts the commands.
fn answer(state: &Arc<WebState>, delay: Duration, refuse: bool) -> Arc<AtomicUsize> {
    let mut rx = state.bus_tx.subscribe();
    let bus_tx = state.bus_tx.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if let BusMsg::Local {
                event: LocalEvent::ConfigCommand { request_id, action },
                ..
            } = msg
            {
                let ConfigAction::Reconnect { id } = *action else {
                    panic!("unexpected action {action:?}");
                };
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                let restarted = if refuse { Vec::new() } else { vec![id] };
                let _ = bus_tx.send(BusMsg::local(
                    "system",
                    LocalEvent::ConfigOutcome {
                        request_id,
                        restarted,
                        stopped: Vec::new(),
                        started: Vec::new(),
                    },
                ));
            }
        }
    });
    count
}

type Reply = Result<(StatusCode, Json<PostSettingsResponse>), SettingsError>;

fn code(result: &Reply) -> StatusCode {
    match result {
        Ok((status, _)) => *status,
        Err((status, _)) => *status,
    }
}

fn error_of(result: Reply) -> serde_json::Value {
    let Err((_, Json(body))) = result else {
        panic!("not an error")
    };
    body["error"].clone()
}

/// Forget pairing deletes the card's token, then reconnects it; any
/// other ID is `404` and touches no token.
#[tokio::test]
async fn forget_pairing_deletes_the_token_and_reconnects_the_card() {
    use crate::actors::pureplay_accessory::token::Token;
    let (state, _writer, _dir) = configured();
    // `localhost` is an address the card never runs with (it needs a
    // literal socket address); it is here to show the token key is the
    // normalized address, as `127.0.0.1`.
    state.root.system.update(|c| {
        c.pureplay_accessory.insert(
            "0".into(),
            flighthook::PureplayAccessorySection {
                address: Some("localhost:9770".into()),
                ..flighthook::PureplayAccessorySection::default()
            },
        );
    });
    let tokens = &state.root.pureplay_tokens;
    tokens
        .save("127.0.0.1:9770", &Token::new("PPA-1"), "c")
        .expect("save");
    tokens
        .save("192.168.1.9:9770", &Token::new("PPA-2"), "c")
        .expect("save");
    let mut rx = state.bus_tx.subscribe();
    for id in [
        "pureplay_accessory.3",
        "pureplay.0",
        "mock_monitor.0",
        "camera.0",
    ] {
        let result = forget_pairing(&state, id, Duration::from_millis(50)).await;
        assert_eq!(code(&result), StatusCode::NOT_FOUND, "{id}");
    }
    assert!(rx.try_recv().is_err(), "nothing reached the bus");
    assert!(tokens.get("127.0.0.1:9770").is_some());

    // A reconnect of the card in progress: 409, and the token is kept.
    {
        let _busy = InFlight::begin(&state, "pureplay_accessory.0").expect("guard");
        let result =
            forget_pairing(&state, "pureplay_accessory.0", Duration::from_millis(50)).await;
        assert_eq!(code(&result), StatusCode::CONFLICT);
        assert!(rx.try_recv().is_err(), "nothing reached the bus");
        assert!(tokens.get("127.0.0.1:9770").is_some(), "kept");
    }

    let count = answer(&state, Duration::ZERO, false);
    let (status, Json(body)) = forget_pairing(&state, "pureplay_accessory.0", CONFIG_TIMEOUT)
        .await
        .expect("reconnected");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.restarted, ["pureplay_accessory.0"]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(tokens.get("127.0.0.1:9770"), None, "localhost is 127.0.0.1");
    assert!(tokens.get("192.168.1.9:9770").is_some(), "another card's");
}

#[tokio::test]
async fn a_reconnect_answers_with_the_restarted_actor() {
    let (state, _writer, _dir) = configured();
    let count = answer(&state, Duration::ZERO, false);
    let (status, Json(body)) = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT)
        .await
        .expect("reconnected");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.restarted, ["mock_monitor.0"]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(state.reconnecting.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cameras_general_cards_and_unknown_ids_are_refused_without_a_command() {
    let (state, _writer, _dir) = configured();
    let mut rx = state.bus_tx.subscribe();
    for (id, status) in [
        ("camera.0", StatusCode::BAD_REQUEST),
        ("webserver.0", StatusCode::BAD_REQUEST),
        ("system", StatusCode::BAD_REQUEST),
        ("mock_monitor.4", StatusCode::NOT_FOUND),
        ("mevo.0", StatusCode::NOT_FOUND),
    ] {
        let result = reconnect(&state, id, Duration::from_millis(50)).await;
        assert_eq!(code(&result), status, "{id}");
        let error = error_of(result);
        assert!(error.as_str().is_some_and(|e| e.contains(id)), "{error}");
    }
    assert!(rx.try_recv().is_err(), "nothing reached the bus");
}

#[tokio::test]
async fn a_second_reconnect_of_the_same_actor_is_refused_while_one_is_in_flight() {
    let (state, _writer, _dir) = configured();
    let count = answer(&state, Duration::from_millis(200), false);
    let (first, second) = tokio::join!(
        reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT),
        reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT),
    );
    assert_eq!(code(&first), StatusCode::OK);
    assert_eq!(code(&second), StatusCode::CONFLICT);
    assert_eq!(count.load(Ordering::SeqCst), 1, "one reconnect ran");
    // Once it is done, another may start.
    let again = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
    assert_eq!(code(&again), StatusCode::OK);
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

/// A reconnect still running when the request has waited its time is
/// `202`, and the actor stays in flight until the outcome arrives: a
/// second press meanwhile cannot queue a second restart.
#[tokio::test]
async fn a_reconnect_still_running_is_accepted_and_keeps_its_guard() {
    let (state, _writer, _dir) = configured();
    let count = answer(&state, Duration::from_millis(400), false);
    let first = reconnect(&state, "mock_monitor.0", Duration::from_millis(50)).await;
    assert_eq!(code(&first), StatusCode::ACCEPTED);
    assert!(first.expect("accepted").1.restarted.is_empty());
    let second = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
    assert_eq!(code(&second), StatusCode::CONFLICT);
    assert!(
        error_of(second)
            .as_str()
            .is_some_and(|e| e.contains("already in progress"))
    );
    assert_eq!(count.load(Ordering::SeqCst), 1, "one command on the bus");

    // The outcome arrives: the guard goes with it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !state.reconnecting.lock().unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline, "guard released");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let third = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
    assert_eq!(code(&third), StatusCode::OK);
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_reconnect_that_starts_nothing_is_a_conflict_with_the_reason() {
    let (state, _writer, _dir) = configured();
    let _count = answer(&state, Duration::ZERO, true);
    let result = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
    assert_eq!(code(&result), StatusCode::CONFLICT);
    assert_eq!(error_of(result), "'mock_monitor.0' was not reconnected");
}

/// A reconnect that starts its actor again by itself later, once the
/// old connection has closed or another command is done restarting it,
/// is accepted.
#[tokio::test]
async fn a_reconnect_that_completes_by_itself_is_accepted() {
    let (state, _writer, _dir) = configured();
    let _count = answer(&state, Duration::ZERO, true);
    state.root.begin_settling("mock_monitor.0");
    let result = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
    assert_eq!(code(&result), StatusCode::ACCEPTED);
    state.root.end_settling("mock_monitor.0");

    crate::actors::start_actor(
        "mock_monitor.0".into(),
        Box::new(crate::actors::mock::MockActor::new(
            flighthook::MockMonitorSection::default(),
        )),
        &state.root,
        &state.bus_tx,
    );
    state.root.close_in_background("mock_monitor.0");
    let result = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
    assert_eq!(code(&result), StatusCode::ACCEPTED);
    assert!(result.expect("accepted").1.restarted.is_empty());
    assert!(state.reconnecting.lock().unwrap().is_empty());
}

#[test]
fn the_config_timeout_outlasts_the_longest_stop_wait() {
    use crate::actors::{MAX_STOP_WAIT, STOP_WAIT};
    let exputt = crate::actors::exputt::ExPuttActor::new(flighthook::ExPuttSection::default());
    assert_eq!(exputt.stop_wait(), MAX_STOP_WAIT);
    let square = crate::actors::square::SquareActor::new(flighthook::SquareSection::default());
    for wait in [STOP_WAIT, square.stop_wait()] {
        assert!(wait <= MAX_STOP_WAIT, "{wait:?}");
    }
    assert!(CONFIG_TIMEOUT >= MAX_STOP_WAIT + START_ROOM);
    assert!(RECONNECT_LINGER >= CONFIG_TIMEOUT);
}

/// An actor that holds its device for `holds` after a stop, waited for
/// `wait`.
struct Lingers {
    holds: Duration,
    wait: Duration,
}

impl crate::actors::Actor for Lingers {
    fn start(
        &self,
        _state: Arc<SystemState>,
        _sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let holds = self.holds;
        std::thread::spawn(move || {
            while !receiver.is_shutdown() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(holds);
        })
    }

    fn stop_wait(&self) -> Duration {
        self.wait
    }
}

/// A save sent while a reconnect waits out its actor's whole stop wait
/// is answered at once, not behind the reconnect; the reconnect, which
/// finds the old connection still closing, is accepted: it completes by
/// itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_during_a_long_reconnect_is_answered_at_once() {
    let (state, writer, _dir) = configured();
    let shutdown = Arc::new(AtomicBool::new(false));
    crate::actors::system::start_for_test(
        writer,
        &state.root,
        &state.bus_tx,
        Arc::clone(&shutdown),
    );
    let wait = Duration::from_millis(800);
    crate::actors::start_actor(
        "mock_monitor.0".into(),
        Box::new(Lingers {
            holds: Duration::from_secs(2),
            wait,
        }),
        &state.root,
        &state.bus_tx,
    );
    let at = std::time::Instant::now();
    let reconnecting = {
        let state = Arc::clone(&state);
        tokio::spawn(async move { reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let saved = save_settings(
        &state,
        Some("mock_monitor.0"),
        state.root.system.snapshot(),
        CONFIG_TIMEOUT,
    )
    .await;
    let took = at.elapsed();
    assert!(saved.is_ok(), "{:?}", saved.err());
    assert!(took < wait, "not behind the reconnect: {took:?}");
    let result = reconnecting.await.expect("reconnect");
    assert_eq!(code(&result), StatusCode::ACCEPTED);
    assert!(at.elapsed() >= wait, "after the stop wait");
    shutdown.store(true, Ordering::SeqCst);
    state.root.stop_all_and_wait(Duration::from_secs(3));
}

/// POST `path` with no body: status code and body.
fn post(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).expect("request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    let code = response
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .expect("status");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (code, body)
}

#[test]
fn the_route_reconnects_a_running_monitor() {
    let (state, writer, _dir) = configured();
    let root = Arc::clone(&state.root);
    let bus = state.bus_tx.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    crate::actors::system::start_for_test(writer, &root, &bus, Arc::clone(&shutdown));
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port");
    let web = super::super::WebActor::new(addr);
    let web_sender = BusSender::new("webserver.0".into(), bus.clone(), Arc::default());
    let web_thread = web.start(
        Arc::clone(&root),
        web_sender.clone(),
        web_sender.subscribe(),
    );
    crate::actors::start_actor(
        "mock_monitor.0".into(),
        Box::new(crate::actors::mock::MockActor::new(
            flighthook::MockMonitorSection::default(),
        )),
        &root,
        &bus,
    );
    let before = root.actor_thread("mock_monitor.0");
    // The server is up once it answers.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(addr).is_err() {
        assert!(std::time::Instant::now() < deadline, "web server up");
        std::thread::sleep(Duration::from_millis(20));
    }

    let (status, body) = post(addr, "/api/actors/mock_monitor.0/reconnect");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""restarted":["mock_monitor.0"]"#), "{body}");
    assert_ne!(root.actor_thread("mock_monitor.0"), before);
    assert_eq!(post(addr, "/api/actors/camera.0/reconnect").0, 400);
    assert_eq!(post(addr, "/api/actors/mock_monitor.3/reconnect").0, 404);
    assert_eq!(post(addr, "/api/actors/mock_monitor.0/reconnect/x").0, 404);

    web.stop();
    let _ = web_thread.join();
    root.stop_and_wait("mock_monitor.0");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}
