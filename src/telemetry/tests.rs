use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const NOW: u64 = 1_790_000_000;

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gitagent-telemetry-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn queued(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join(QUEUE_FILE))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn finished(outcome: Outcome) -> Event {
    Event::flow_finished(
        "commit_and_pr",
        outcome,
        &Forge::GitHub,
        ProviderKind::Ollama,
    )
}

#[test]
fn nothing_is_recorded_unless_every_condition_holds() {
    let url = Some("https://x");
    // gate(endpoint, consent, informed, env_blocked)
    assert!(gate(url, Some(true), true, false));
    // No endpoint: a development build, whatever the person said.
    assert!(!gate(None, Some(true), true, false));
    // Said no.
    assert!(!gate(url, Some(false), true, false));
    // An environment opt-out beats a yes.
    assert!(!gate(url, Some(true), true, true));
}

#[test]
fn on_by_default_never_means_on_before_you_were_told() {
    let url = Some("https://x");
    // Unanswered and not yet shown the notice: nothing, even though the
    // default is yes.
    assert!(!gate(url, None, false, false));
    // Shown the notice and did nothing about it: the default applies.
    assert_eq!(gate(url, None, true, false), DEFAULT_CONSENT);
    // An explicit answer is never overridden by the default or by having
    // been informed.
    assert!(!gate(url, Some(false), true, false));
    assert!(gate(url, Some(true), false, false));
    // And the environment still wins over the default.
    assert!(!gate(url, None, true, true));
}

#[test]
fn being_informed_is_remembered_and_does_not_touch_the_answer() {
    let dir = scratch();
    assert!(!load_state(&dir).informed);
    mark_informed_in(&dir);
    assert!(load_state(&dir).informed);
    assert_eq!(load_state(&dir).consent, None, "informing is not answering");

    set_consent_in(&dir, false);
    mark_informed_in(&dir);
    assert_eq!(
        load_state(&dir).consent,
        Some(false),
        "and never flips an answer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opt_out_variables_follow_their_conventions() {
    assert!(flag_set(Some("1")));
    assert!(flag_set(Some("true")));
    assert!(!flag_set(Some("0")));
    assert!(!flag_set(Some("false")));
    assert!(!flag_set(Some("")));
    assert!(!flag_set(None));
}

#[test]
fn a_run_is_only_done_if_nothing_in_it_went_wrong() {
    use NodeStatus::*;
    assert_eq!(outcome_of(&[Done, Done, Skipped]), Outcome::Done);
    // Committed, then the push failed: that did not work.
    assert_eq!(outcome_of(&[Done, Failed, Blocked]), Outcome::Failed);
    assert_eq!(outcome_of(&[Done, Rejected, Blocked]), Outcome::Rejected);
    assert_eq!(outcome_of(&[Failed, Rejected]), Outcome::Failed);
    assert_eq!(outcome_of(&[Skipped, Bypassed]), Outcome::Nothing);
    assert_eq!(outcome_of(&[]), Outcome::Nothing);
}

#[test]
fn an_empty_endpoint_is_no_endpoint() {
    // An unset repository variable reaches option_env! as "", not as absent.
    assert_eq!(Some("").filter(|u| !u.is_empty()), None);
}

#[test]
fn off_writes_nothing_at_all() {
    let dir = scratch();
    record_in(&dir, false, &Event::AppStarted, NOW);
    assert!(!dir.exists(), "even the state file would be a trace");
}

#[test]
fn identity_is_random_stable_and_local() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);
    let first = load_state(&dir).install_id;
    assert_eq!(first.len(), 32);
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    record_in(&dir, true, &Event::AppStarted, NOW + 5);
    assert_eq!(load_state(&dir).install_id, first, "one id per install");
    assert_ne!(random_hex(), random_hex());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn installed_is_reported_once_ahead_of_the_first_event() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);
    record_in(&dir, true, &Event::AppStarted, NOW + 60);
    record_in(&dir, true, &finished(Outcome::Done), NOW + 120);
    let names: Vec<String> = queued(&dir)
        .iter()
        .map(|e| e["n"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names[0], "app_installed", "first, before what triggered it");
    assert_eq!(names.iter().filter(|n| *n == "app_installed").count(), 1);

    // Off means no identity, so no install is reported either.
    let off = scratch();
    record_in(&off, false, &Event::AppStarted, NOW);
    assert!(queued(&off).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn activation_fires_once_and_only_for_a_run_that_worked() {
    let dir = scratch();
    record_in(&dir, true, &finished(Outcome::Failed), NOW);
    record_in(&dir, true, &finished(Outcome::Nothing), NOW);
    assert!(queued(&dir)
        .iter()
        .all(|e| e["n"] != "first_flow_completed"));

    record_in(&dir, true, &finished(Outcome::Done), NOW + 3 * 86_400);
    record_in(&dir, true, &finished(Outcome::Done), NOW + 4 * 86_400);
    let firsts: Vec<_> = queued(&dir)
        .into_iter()
        .filter(|e| e["n"] == "first_flow_completed")
        .collect();
    assert_eq!(firsts.len(), 1);
    assert_eq!(firsts[0]["p"]["days"], "3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_event_cannot_carry_anything_but_its_vocabulary() {
    let (name, p) = Event::flow_finished(
        "my-secret-client-flow",
        Outcome::Done,
        &Forge::Unsupported("git.internal.bigcorp.example".into()),
        ProviderKind::Remote,
    )
    .wire();
    assert_eq!(name, "flow_finished");
    assert_eq!(p["flow"], "custom");
    assert_eq!(p["forge"], "other");
    assert_eq!(p["provider"], "remote");
    let everything = format!("{p:?}");
    assert!(!everything.contains("bigcorp") && !everything.contains("secret"));

    // Shipped flows keep their names.
    assert_eq!(flow_tag("commit_and_pr"), "commit_and_pr");
    assert_eq!(flow_tag("review_and_merge"), "review_and_merge");
}

#[test]
fn a_version_string_is_checked_before_it_is_kept() {
    assert_eq!(sanitise_version("0.1.72"), "0.1.72");
    assert_eq!(sanitise_version("0.2.0-beta1"), "0.2.0-beta1");
    assert_eq!(sanitise_version("1.0 <script>"), "unknown");
    assert_eq!(sanitise_version(""), "unknown");
    assert_eq!(sanitise_version(&"9".repeat(40)), "unknown");
}

#[test]
fn a_full_queue_drops_new_events_rather_than_growing() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);
    let filler = "x".repeat(MAX_QUEUE_BYTES as usize);
    std::fs::write(dir.join(QUEUE_FILE), filler).unwrap();
    let before = std::fs::metadata(dir.join(QUEUE_FILE)).unwrap().len();
    record_in(&dir, true, &Event::AppStarted, NOW + 1);
    assert_eq!(
        std::fs::metadata(dir.join(QUEUE_FILE)).unwrap().len(),
        before
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saying_no_deletes_the_backlog_and_keeps_the_answer() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);
    assert!(dir.join(QUEUE_FILE).exists());
    set_consent_in(&dir, false);
    assert!(!dir.join(QUEUE_FILE).exists());
    assert_eq!(load_state(&dir).consent, Some(false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tiny HTTP server: answers the nth request with `statuses[n]` (204 once
/// the list runs out) and records each request target, in order.
async fn serve(
    statuses: Vec<&'static str>,
) -> (
    String,
    std::sync::Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/ping", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let handle = tokio::spawn(async move {
        let mut n = 0;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..read]);
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let first = head.lines().next().unwrap_or_default();
            assert!(first.starts_with("GET "), "telemetry is a GET: {first}");
            log.lock().unwrap().push(first.to_string());
            let status = statuses.get(n).copied().unwrap_or("204 No Content");
            n += 1;
            let reply =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (url, seen, handle)
}

/// Decodes the batch out of a recorded request line.
fn decode(request_line: &str) -> serde_json::Value {
    let target = request_line.split(' ').nth(1).unwrap();
    let encoded = target.split("b=").nth(1).expect("batch in the query");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Exactly `events` queued lines: the first record of an install also
/// queues its one `app_installed`, so one fewer `app_started` is needed.
fn backlog(dir: &Path, events: usize) {
    for i in 0..events - 1 {
        record_in(dir, true, &Event::AppStarted, NOW + i as u64);
    }
    assert_eq!(queued(dir).len(), events);
}

#[tokio::test]
async fn a_successful_send_delivers_the_envelope_and_clears_the_queue() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);
    record_in(&dir, true, &finished(Outcome::Done), NOW + 1);
    assert_eq!(
        queued(&dir).len(),
        4,
        "app_installed, app_started, flow_finished, first_flow_completed"
    );

    let (url, seen, server) = serve(vec!["204 No Content"]).await;
    assert!(flush_to(&dir, &url, NOW + 2).await);
    server.abort();

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /ping?b="));
    let body = decode(&requests[0]);
    let mut keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["app", "arch", "events", "install_id", "os", "sent_at", "v"]
    );
    assert_eq!(body["app"], "gitagent");
    assert_eq!(body["v"], 1);
    assert_eq!(body["events"].as_array().unwrap().len(), 4);
    assert_eq!(body["install_id"].as_str().unwrap().len(), 32);
    assert!(!dir.join(QUEUE_FILE).exists(), "sent events are forgotten");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_failed_send_keeps_the_events_for_next_time() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);

    let (url, _seen, server) = serve(vec!["500 Internal Server Error"]).await;
    assert!(!flush_to(&dir, &url, NOW + 1).await);
    server.abort();
    assert_eq!(queued(&dir).len(), 2, "app_installed, app_started");

    // Nothing listening at all.
    assert!(!flush_to(&dir, "http://127.0.0.1:1/x", NOW + 1).await);
    assert_eq!(queued(&dir).len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_backlog_goes_out_in_chunks_that_each_fit_in_a_url() {
    let dir = scratch();
    backlog(&dir, 300);

    let (url, seen, server) = serve(vec![]).await;
    flush_all_to(&dir, &url, NOW + 300).await;
    server.abort();

    let requests = seen.lock().unwrap().clone();
    assert!(requests.len() > 1, "a backlog needs several requests");
    assert!(requests.len() <= MAX_CHUNKS_PER_FLUSH);
    let mut sent = 0;
    for line in &requests {
        // nginx's default request-line limit is 8 KB.
        assert!(line.len() < 8000, "request line is {} bytes", line.len());
        let events = decode(line)["events"].as_array().unwrap().len();
        assert!((1..=MAX_BATCH).contains(&events));
        sent += events;
    }
    assert_eq!(sent + queued(&dir).len(), 300, "nothing lost or doubled");

    // The remainder goes out on a later flush.
    let (url, _seen, server) = serve(vec![]).await;
    for _ in 0..3 {
        flush_all_to(&dir, &url, NOW + 300).await;
    }
    server.abort();
    assert!(!dir.join(QUEUE_FILE).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_failing_chunk_stops_the_flush_and_keeps_the_rest() {
    let dir = scratch();
    backlog(&dir, 300);

    let (url, seen, server) = serve(vec!["204 No Content", "500 Internal Server Error"]).await;
    flush_all_to(&dir, &url, NOW + 300).await;
    server.abort();

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "stops at the first failure");
    let first = decode(&requests[0])["events"].as_array().unwrap().len();
    let second = decode(&requests[1])["events"].as_array().unwrap().len();
    let left = queued(&dir);
    assert_eq!(
        left.len(),
        300 - first,
        "only the acknowledged chunk is gone"
    );
    assert!(left.len() >= second, "the refused chunk is still queued");
    // The queue starts app_installed@NOW, app_started@NOW, @NOW+1, … so
    // line k (k ≥ 1) is stamped NOW + k - 1.
    assert_eq!(
        left[0]["t"],
        NOW + first as u64 - 1,
        "oldest unsent comes first"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn old_and_unreadable_lines_are_dropped_not_sent() {
    let dir = scratch();
    record_in(&dir, true, &Event::AppStarted, NOW);
    let mut text = std::fs::read_to_string(dir.join(QUEUE_FILE)).unwrap();
    text.push_str("this is not json\n");
    std::fs::write(dir.join(QUEUE_FILE), text).unwrap();

    // Two weeks later the one good event is stale too.
    let later = NOW + 2 * MAX_AGE_SECS;
    assert!(!flush_to(&dir, "http://127.0.0.1:1/x", later).await);
    assert!(!dir.join(QUEUE_FILE).exists());
    let _ = std::fs::remove_dir_all(&dir);
}
