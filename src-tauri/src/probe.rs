//! F4/F5: check Responding (bare TCP connect, no protocol data) and fetch Title
//! (`GET /`, DevServer only, cached). See docs/adr/0001-actively-connect-to-every-port.md
//! for why this is safe to run against every listener, including databases.
//!
//! Not domain/: this module does real I/O (P1 — platform/domain stay pure, but
//! probe.rs and scanner.rs are allowed to touch the network).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::platform::{AddressFamily, PortBinding};

/// Measured on this machine: a live socket accepts in ~6ms, a dead one refuses in
/// ~3.4ms (docs/PLAN.md). 500ms is generous headroom over both, not a tuned budget —
/// it exists to bound the pathological case (a firewalled host that neither accepts
/// nor refuses, just silently drops) rather than the common one.
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(500);

/// Generous enough that a slow-starting dev server's first response still lands
/// inside it, but short enough that one unresponsive title fetch cannot stall a
/// panel-open scan (F5 runs only for DevServer Kind, and only when Responding, so this
/// is rare on the hot path, not the routine one).
const TITLE_TIMEOUT: Duration = Duration::from_secs(2);

/// Refuse to read more than this many bytes of a title response. An HTTP GET to a
/// server that isn't a normal dev server (or one that streams indefinitely, e.g. an
/// SSE/websocket-upgrade endpoint that never closes) must not be allowed to read
/// unboundedly — this is a bound on a hostile or merely unusual peer, not a
/// performance tweak.
const MAX_TITLE_RESPONSE_BYTES: usize = 64 * 1024;

/// F4: whether a bare TCP connect succeeds, carrying no protocol data. This is the
/// ONLY check ever run against a non-DevServer Kind (databases, mail, background
/// services) — see ADR 0001 for why that is safe.
///
/// Binds to the loopback address matching the binding's family (127.0.0.1 for V4,
/// ::1 for V6) rather than resolving a hostname, so this never touches DNS and always
/// reaches the same machine the listener is actually on.
pub fn is_live(binding: &PortBinding) -> bool {
    let ip = match binding.family {
        AddressFamily::V4 => IpAddr::V4(Ipv4Addr::LOCALHOST),
        AddressFamily::V6 => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    let addr = SocketAddr::new(ip, binding.port);
    // Connecting and immediately dropping is the entire check: no bytes are written,
    // no bytes are read. This is what makes it safe against Postgres, mail servers,
    // or anything else that would log a malformed request.
    TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).is_ok()
}

/// Run `is_live` across every binding of every server IN PARALLEL. Sequential would
/// multiply `LIVENESS_TIMEOUT` by listener count in the worst case (any dead/filtered
/// port pays the full timeout) — with ~30 listeners that is 15s sequential vs. one
/// timeout's worth in parallel. `std::thread::scope` needs no async runtime, and none
/// is present in this crate.
///
/// Takes `(key, bindings)` pairs rather than a flat list so the caller (scanner.rs,
/// keyed by Server id) gets results back grouped by server without re-joining them
/// itself. Returns one bool per server: true if ANY binding answered — see
/// `scanner::classify_and_probe`, which computes `Health::Responding` from this same
/// "any binding live" rule, for why "any" is the right aggregation rather than "all".
pub fn liveness_for_servers<K: Clone + Send + Sync>(servers: &[(K, Vec<PortBinding>)]) -> Vec<(K, bool)> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = servers
            .iter()
            .map(|(key, bindings)| {
                scope.spawn(move || {
                    let any_live = bindings.iter().any(is_live);
                    (key.clone(), any_live)
                })
            })
            .collect();

        // A panicked probe thread (should not happen — `is_live` cannot panic on any
        // input it can receive) is dropped from the results via `.ok()` rather than
        // propagating the panic and losing every other server's result too. Its
        // server simply gets no liveness result this cycle, same as if it were
        // momentarily absent from the input.
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

/// F5: fetch the `<title>` of a DevServer's `GET /` response. Callers MUST only call
/// this for `Kind::DevServer` — see CONTEXT.md "Title" and REQUIREMENTS.md F5:
/// "Never send protocol requests to any other Kind." This function itself has no way
/// to enforce that (it only sees a binding, not a Kind), so the caller in scanner.rs
/// is the enforcement point; keep it that way rather than threading a Kind through
/// here just to assert it.
///
/// Returns `None` on any failure (connection refused, malformed response, no `<title>`
/// tag, timeout) — a missing title is shown as the command instead (IPC.md: `title:
/// string | null`), never as an error the user has to do anything about.
pub fn fetch_title(binding: &PortBinding) -> Option<String> {
    let ip = match binding.family {
        AddressFamily::V4 => IpAddr::V4(Ipv4Addr::LOCALHOST),
        AddressFamily::V6 => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    let addr = SocketAddr::new(ip, binding.port);

    let mut stream = TcpStream::connect_timeout(&addr, LIVENESS_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(TITLE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TITLE_TIMEOUT)).ok()?;

    let request = format!("GET / HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n", binding.port);
    stream.write_all(request.as_bytes()).ok()?;

    let body = read_bounded(&mut stream, MAX_TITLE_RESPONSE_BYTES);
    let text = String::from_utf8_lossy(&body);
    extract_title(&text)
}

/// Read up to `max_bytes` from `stream` and stop — never buffer an unbounded
/// response. A short read (peer closed early, or hit the timeout) is not an error
/// here: whatever bytes arrived are still searched for a `<title>`, since a real HTML
/// document's `<head>` is almost always well within the cap.
fn read_bounded(stream: &mut TcpStream, max_bytes: usize) -> Vec<u8> {
    let mut buf = vec![0u8; max_bytes];
    let mut filled = 0;
    while filled < max_bytes {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    buf.truncate(filled);
    buf
}

/// Extract the text of the first `<title>...</title>` tag from an HTTP response,
/// case-insensitively, tolerating attributes on the tag. Deliberately minimal: this
/// is not an HTML parser, just enough to read a `<title>` out of a `GET /` response
/// (F5's literal scope), and untrusted server output must never be able to make this
/// panic.
fn extract_title(response: &str) -> Option<String> {
    let lower = response.to_ascii_lowercase();
    let open_tag_start = lower.find("<title")?;
    let open_tag_end = lower[open_tag_start..].find('>')? + open_tag_start + 1;
    let close_start_rel = lower[open_tag_end..].find("</title>")?;
    let close_start = open_tag_end + close_start_rel;

    let raw_title = response.get(open_tag_end..close_start)?.trim();
    if raw_title.is_empty() {
        return None;
    }
    Some(decode_basic_entities(raw_title))
}

/// Decode the handful of HTML entities a `<title>` realistically contains. Not a full
/// entity table — this is F5's "read a title", not an HTML decoder — but the five
/// standard ones are common enough (e.g. "Vite &amp; React") that skipping them would
/// show the user a garbled title for a very ordinary page.
fn decode_basic_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// F5: title cache keyed by (pid, port) as PLAN.md specifies. Never re-fetched on a
/// routine scan; invalidated by the caller when Responding changes for that server, or
/// on `refresh_now`. A plain `HashMap` behind the scanner's existing mutex — this
/// module owns no locking of its own, scanner.rs decides when entries are read,
/// written, or dropped.
///
/// The value is `Option<String>`, not `String`: an entry MUST be recorded even when
/// `fetch_title` returns `None` (an API-only dev server with no `<title>`, or one that
/// briefly failed to answer the GET). Without a stored "attempted, found nothing"
/// state, a title-less DevServer would look identical to "never tried" on every single
/// tick, and `scanner::title_for` would issue a real `GET /` to it every 3 seconds
/// while the panel is open — silently violating F5 ("do not re-fetch on routine
/// refresh") and N1 ("No HTTP traffic reaches the user's servers while the dashboard
/// merely sits open") for exactly the servers this cache exists to protect.
pub type TitleCache = HashMap<(u32, u16), Option<String>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    #[test]
    fn is_live_true_for_an_actually_listening_port() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let port = listener.local_addr().unwrap().port();
        let binding = PortBinding { port, family: AddressFamily::V4, reachability: crate::platform::Reachability::LocalhostOnly };
        assert!(is_live(&binding));
    }

    #[test]
    fn is_live_false_for_a_closed_port() {
        // Bind, read the assigned port, then drop the listener — nothing is
        // listening on it anymore, but the port number itself is real and unused by
        // this test, minimizing (not eliminating) flakiness from port reuse.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let binding = PortBinding { port, family: AddressFamily::V4, reachability: crate::platform::Reachability::LocalhostOnly };
        assert!(!is_live(&binding));
    }

    #[test]
    fn liveness_for_servers_runs_in_parallel_and_reports_per_server() {
        let live_listener = TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let live_port = live_listener.local_addr().unwrap().port();
        let dead_listener = TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let dead_port = dead_listener.local_addr().unwrap().port();
        drop(dead_listener);

        let servers = vec![
            (
                "alive",
                vec![PortBinding { port: live_port, family: AddressFamily::V4, reachability: crate::platform::Reachability::LocalhostOnly }],
            ),
            (
                "dead",
                vec![PortBinding { port: dead_port, family: AddressFamily::V4, reachability: crate::platform::Reachability::LocalhostOnly }],
            ),
        ];

        let results = liveness_for_servers(&servers);
        let alive = results.iter().find(|(k, _)| *k == "alive").unwrap();
        let dead = results.iter().find(|(k, _)| *k == "dead").unwrap();
        assert!(alive.1);
        assert!(!dead.1);
    }

    #[test]
    fn extract_title_finds_simple_title() {
        let response = "HTTP/1.1 200 OK\r\n\r\n<html><head><title>Vite App</title></head></html>";
        assert_eq!(extract_title(response), Some("Vite App".to_string()));
    }

    #[test]
    fn extract_title_tolerates_attributes_and_case() {
        let response = "<HTML><HEAD><TITLE data-x=\"1\">My Dev Server</TITLE></HEAD></HTML>";
        assert_eq!(extract_title(response), Some("My Dev Server".to_string()));
    }

    #[test]
    fn extract_title_decodes_basic_entities() {
        let response = "<title>Vite &amp; React</title>";
        assert_eq!(extract_title(response), Some("Vite & React".to_string()));
    }

    #[test]
    fn extract_title_none_when_missing() {
        assert_eq!(extract_title("<html><body>no title here</body></html>"), None);
    }

    #[test]
    fn extract_title_none_when_empty() {
        assert_eq!(extract_title("<title></title>"), None);
    }

    #[test]
    fn fetch_title_reads_a_real_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let port = listener.local_addr().unwrap().port();

        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                let body = "<html><head><title>Test Server</title></head><body></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let binding = PortBinding { port, family: AddressFamily::V4, reachability: crate::platform::Reachability::LocalhostOnly };
        let title = fetch_title(&binding);
        handle.join().unwrap();

        assert_eq!(title, Some("Test Server".to_string()));
    }

    #[test]
    fn fetch_title_none_when_nothing_listening() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let binding = PortBinding { port, family: AddressFamily::V4, reachability: crate::platform::Reachability::LocalhostOnly };
        assert_eq!(fetch_title(&binding), None);
    }

    /// REQUIREMENTS.md documents port 4321 as the motivating case for F4: a server
    /// that held the port for three days while refusing every connection. Re-checked
    /// live on this machine while building this test (2026-09-02): that specific dead
    /// instance is gone — port 4321 is currently held by a healthy astro dev server
    /// (bound IPv6-only, `[::1]:4321`, confirmed above by manually completing a TCP
    /// handshake AND reading a real `HTTP/1.1 200 OK` response from it). So the
    /// correct assertion today is the positive control: `is_live` must report `true`
    /// for it — proving the probe correctly targets the IPv6 family a listener
    /// actually bound, which `127.0.0.1` alone would have missed entirely. The
    /// negative case (a bound-but-refusing port) is exercised deterministically by
    /// `is_live_false_for_a_closed_port` above; this test is `#[ignore]`d like
    /// `enumerate_returns_real_data_on_this_machine` because it depends on this
    /// machine's current process state, not a fixture.
    #[test]
    #[ignore]
    fn port_4321_liveness_matches_its_current_real_state() {
        let binding = PortBinding { port: 4321, family: AddressFamily::V6, reachability: crate::platform::Reachability::LocalhostOnly };
        assert!(is_live(&binding), "port 4321 is currently a live, responding astro dev server on this machine");
    }
}
