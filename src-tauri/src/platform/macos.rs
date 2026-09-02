//! macOS implementation of `ProcessSource` (P2: gathering is platform-specific,
//! everything downstream is not).
//!
//! Three external commands do the gathering:
//! - `lsof -nP -iTCP -sTCP:LISTEN -F pcnt` lists every listening socket, grouped by
//!   pid, with clean field-per-line output (no truncated command names, no escaped
//!   spaces — see tests/fixtures/lsof_listen_fields_raw.txt for a captured sample).
//!   The `t` field (file TYPE — "IPv4"/"IPv6") is what actually distinguishes
//!   address family; the address string alone does not (`*:5432` gives no family
//!   clue by itself, only the bracket syntax `[::1]` does, and only when the host
//!   isn't `*`).
//! - `ps -o pid=,ppid=,user=,etime=,comm=` fills in the process metadata `lsof`
//!   doesn't carry: parent pid, owning user, elapsed run time, and the full
//!   executable path (macOS `ps comm` prints the path, not just a short name).
//! - `lsof -a -p <pids> -d cwd -F pn` looks up every enumerated pid's working
//!   directory in one batched call, rather than one `lsof` spawn per pid.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::{AddressFamily, PortBinding, ProcessSource, Reachability, RawListener};

pub struct MacosProcessSource;

impl ProcessSource for MacosProcessSource {
    fn enumerate(&self) -> Result<Vec<RawListener>, String> {
        let lsof_output = run_lsof_listeners()?;
        let sockets = parse_lsof_fields(&lsof_output);
        if sockets.is_empty() {
            return Ok(Vec::new());
        }

        let pids: Vec<u32> = sockets.iter().map(|s| s.pid).collect();
        let ps_rows = run_ps_metadata(&pids)?;
        let ps_by_pid = parse_ps_output(&ps_rows);
        // One batched cwd lookup for every pid, not one `lsof` spawn per pid — N1
        // budgets enumeration in tens of milliseconds, and spawning a process per
        // listener would multiply that by the listener count for no benefit.
        let cwd_output = run_lsof_cwd_batch(&pids)?;
        let cwd_by_pid = parse_cwd_batch(&cwd_output);

        let mut listeners = Vec::with_capacity(sockets.len());
        for socket in sockets {
            // A process can vanish between `lsof` and `ps` (it exited). Skip it
            // rather than fabricate metadata for a process that no longer exists.
            let Some(meta) = ps_by_pid.get(&socket.pid) else {
                continue;
            };

            listeners.push(RawListener {
                pid: socket.pid,
                ppid: meta.ppid,
                command: socket.command,
                exe_path: PathBuf::from(&meta.comm),
                cwd: cwd_by_pid.get(&socket.pid).cloned(),
                ports: socket.ports,
                start_time: meta.start_time,
                user: meta.user.clone(),
            });
        }
        Ok(listeners)
    }

    fn owning_app(&self, exe: &Path) -> Option<String> {
        // Walk every ancestor and remember the OUTERMOST ".app" bundle seen, since a
        // helper process can live inside its own nested bundle
        // (Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/...)
        // and the user recognises the outer app, not the helper.
        let mut outermost: Option<String> = None;
        for ancestor in exe.ancestors() {
            if let Some(name) = ancestor.file_name().and_then(|n| n.to_str()) {
                if let Some(app_name) = name.strip_suffix(".app") {
                    outermost = Some(app_name.to_string());
                }
            }
        }
        outermost
    }

    fn request_stop(&self, pid: u32) -> Result<(), String> {
        signal_process_group(pid, libc::SIGTERM)
    }

    fn force_stop(&self, pid: u32) -> Result<(), String> {
        signal_process_group(pid, libc::SIGKILL)
    }
}

/// What `kill` should actually be aimed at for a given pid and its process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalTarget {
    /// The whole process group — `kill(-pgid, ...)`. Only when the target LEADS it.
    Group(libc::pid_t),
    /// This process alone — `kill(pid, ...)`.
    JustThisProcess(libc::pid_t),
}
/// Decide what a stop request for `pid` may signal, given the group `getpgid` reported.
///
/// F8/CONTEXT.md "Stopping" wants the signal aimed at "the Server and everything it
/// started", which `kill(-pgid)` achieves — but ONLY when the target is the group
/// leader, i.e. when the group IS the Server's own tree. That condition was previously
/// never checked, and it does not hold for the case this tool exists to handle:
/// agent-started dev servers are frequently ordinary MEMBERS of a group led by some
/// unrelated process (a coding agent's helper). Signaling `-pgid` there reaches that
/// whole foreign tree — verified live during the audit, where one stop would have
/// signaled 17 unrelated processes.
///
/// So the group is signaled only when `pgid == pid`. Otherwise the signal is narrowed
/// to the bare pid.
///
/// The cost of that narrowing is real and deliberate: a non-leader Server's own
/// children are no longer signaled, so a child that survives can keep the port held —
/// which F8 already handles honestly, because a stop is verified by re-checking the
/// port and reported as `still_running` rather than assumed. Over-signaling an
/// unrelated process tree has no such recovery, so the narrow target is the correct
/// trade.
///
/// Pure and separated from the `kill` call so exactly this decision is unit-testable.
fn signal_target(pid: u32, pgid: libc::pid_t) -> Result<SignalTarget, String> {
    let pid = pid as libc::pid_t;
    if pgid <= 1 {
        // -1 is a lookup failure; 0 or 1 would mean "no group" or "kernel/init",
        // never a real dev-server group. Refuse rather than signal the wrong thing.
        return Err(format!("refusing to signal pid {pid}: invalid process group {pgid}"));
    }
    if pgid == pid {
        Ok(SignalTarget::Group(pgid))
    } else {
        Ok(SignalTarget::JustThisProcess(pid))
    }
}

/// Signal a Server, aimed as widely as is provably safe — see `signal_target` for the
/// leader check that decides between the whole process group and the bare pid.
///
/// Called by `request_stop` (SIGTERM) and `force_stop` (SIGKILL), which are the only
/// two places in this codebase that signal anything at all.
fn signal_process_group(pid: u32, signal: libc::c_int) -> Result<(), String> {
    // SAFETY: getpgid/kill are plain libc calls; pid is a plain integer, no pointers
    // involved.
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    let target = signal_target(pid, pgid)?;

    // A negative pid targets the whole process group (see `man 2 kill`); a positive
    // one targets that process alone.
    let (arg, description) = match target {
        SignalTarget::Group(pgid) => (-pgid, format!("process group {pgid}")),
        SignalTarget::JustThisProcess(pid) => (pid, format!("process {pid} (not a group leader)")),
    };

    let result = unsafe { libc::kill(arg, signal) };
    if result != 0 {
        return Err(format!(
            "failed to signal {description} for pid {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// One process's raw listening sockets, before `ps` metadata is joined in.
struct RawSockets {
    pid: u32,
    command: String,
    ports: Vec<PortBinding>,
}

fn run_lsof_listeners() -> Result<String, String> {
    let output = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcnt"])
        .output()
        .map_err(|e| format!("failed to run lsof: {e}"))?;

    // lsof exits non-zero both when there are simply no matching listeners (the
    // common, non-error case) and on a real failure. The two are distinguished by
    // stderr: "no listeners" produces empty stdout and empty stderr, a real failure
    // (e.g. lsof missing, bad arguments) writes to stderr. Only the latter should be
    // surfaced as an error — silently swallowing it would hide a real failure behind
    // an empty "no servers running" result.
    if !output.status.success() && !output.stderr.is_empty() {
        return Err(format!(
            "lsof exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_lsof_cwd_batch(pids: &[u32]) -> Result<String, String> {
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let output = Command::new("lsof")
        .args(["-a", "-p", &pid_list, "-d", "cwd", "-F", "pn"])
        .output()
        .map_err(|e| format!("failed to run lsof (cwd): {e}"))?;

    if !output.status.success() && !output.stderr.is_empty() {
        return Err(format!(
            "lsof (cwd) exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `lsof -a -p <pids> -d cwd -F pn` output: a `p<pid>` line followed by an
/// `n<path>` line (an `f<fd>` line in between is skipped, same shape as the socket
/// parser). A pid that lsof could not report a cwd for (permission denied, or it
/// exited) simply has no entry here — callers get `None` for it, which is the
/// expected, handled case per `RawListener.cwd: Option<PathBuf>`.
fn parse_cwd_batch(raw: &str) -> BTreeMap<u32, PathBuf> {
    let mut result = BTreeMap::new();
    let mut current_pid: Option<u32> = None;

    for line in raw.lines() {
        let Some((tag, value)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => current_pid = value.parse::<u32>().ok(),
            "n" => {
                if let (Some(pid), false) = (current_pid, value.is_empty()) {
                    result.insert(pid, PathBuf::from(value));
                }
            }
            _ => {}
        }
    }
    result
}

/// Parse `lsof -F pcnt` output. Format is line-oriented: a `p<pid>` line starts a
/// process, `c<command>` names it, and each following `f<fd>`/`t<type>`/`n<address:port>`
/// triple is one listening socket for that process. `t` (file TYPE: "IPv4" or
/// "IPv6") must arrive before the `n` line it describes for a binding to be built —
/// lsof always emits fields in that order for a TCP socket, so a `t` is buffered
/// until its matching `n` shows up.
fn parse_lsof_fields(raw: &str) -> Vec<RawSockets> {
    let mut result = Vec::new();
    let mut current_pid: Option<u32> = None;
    let mut current_command = String::new();
    let mut current_ports: Vec<PortBinding> = Vec::new();
    let mut pending_family: Option<AddressFamily> = None;

    let flush = |result: &mut Vec<RawSockets>, pid: Option<u32>, command: &str, ports: Vec<PortBinding>| {
        if let Some(pid) = pid {
            if !ports.is_empty() {
                result.push(RawSockets {
                    pid,
                    command: command.to_string(),
                    ports,
                });
            }
        }
    };

    for line in raw.lines() {
        let Some((tag, value)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => {
                // Starting a new process: flush whatever we accumulated for the
                // previous one.
                flush(&mut result, current_pid, &current_command, std::mem::take(&mut current_ports));
                current_pid = value.parse::<u32>().ok();
                current_command.clear();
                pending_family = None;
            }
            "c" => current_command = value.to_string(),
            "t" => {
                pending_family = match value {
                    "IPv4" => Some(AddressFamily::V4),
                    "IPv6" => Some(AddressFamily::V6),
                    // Any other file type ("IPv4"/"IPv6" are the only ones the
                    // -iTCP filter should produce) is not a socket this tool
                    // tracks; drop the association so a stray `n` line right
                    // after it isn't misattributed a family.
                    _ => None,
                };
            }
            "n" => {
                // A binding with no family (the `t` line was missing, malformed, or
                // not IPv4/IPv6) is skipped rather than guessed at — this is
                // parsing untrusted external command output, and a silently wrong
                // family is worse than a dropped row.
                if let Some(family) = pending_family.take() {
                    if let Some(binding) = parse_port_binding(value, family) {
                        current_ports.push(binding);
                    }
                }
            }
            _ => {}
        }
    }
    flush(&mut result, current_pid, &current_command, current_ports);

    result
}

/// Parse one `name` field from `lsof -F n`, e.g. `127.0.0.1:4399`, `[::1]:4399`, or
/// `*:5432`, paired with the address family already read from the preceding `t`
/// field. Must split from the RIGHT: an IPv6 address contains colons of its own, so
/// splitting on the first colon would cut `[::1]` apart.
fn parse_port_binding(address: &str, family: AddressFamily) -> Option<PortBinding> {
    let (host, port_str) = address.rsplit_once(':')?;
    let port = port_str.parse::<u16>().ok()?;
    let reachability = if host == "*" {
        Reachability::AllInterfaces
    } else if host == "127.0.0.1" || host == "[::1]" || host == "localhost" {
        Reachability::LocalhostOnly
    } else {
        // Any other bound address (e.g. a specific LAN IP) is reachable from outside
        // this machine, same as `*`.
        Reachability::AllInterfaces
    };
    Some(PortBinding { port, family, reachability })
}

struct ProcessMeta {
    ppid: u32,
    user: String,
    start_time: SystemTime,
    comm: String,
}

fn run_ps_metadata(pids: &[u32]) -> Result<String, String> {
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let output = Command::new("ps")
        .args(["-o", "pid=,ppid=,user=,etime=,comm=", "-p", &pid_list])
        .output()
        .map_err(|e| format!("failed to run ps: {e}"))?;

    // Same status/stderr discipline as the two lsof paths above, and for a sharper
    // reason: `enumerate()` JOINS on this output, so an unchecked failure returns an
    // empty map, every socket is dropped at the join, and the app reports "nothing
    // running" — a fabricated fact, which N3 forbids outright.
    //
    // `ps -p <list>` also exits non-zero in a legitimate, non-error case: when NONE of
    // the listed pids still exist (every listener exited between the lsof call and
    // this one). That case writes nothing to stderr, so it is distinguished the same
    // way lsof's is — an empty result there is genuinely correct, not a hidden failure.
    if !output.status.success() && !output.stderr.is_empty() {
        return Err(format!(
            "ps exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `ps -o pid=,ppid=,user=,etime=,comm=` output. The first four columns are
/// separated by RUNS of spaces (column alignment, not single delimiters), so they
/// must be split with `split_once(char::is_whitespace)` plus `trim_start` on the
/// remainder, not `splitn` on individual whitespace chars — `splitn` would treat
/// each space in a run as its own empty field and every row would fail to parse.
/// `comm` is whatever remains: the full executable path, which itself may contain
/// spaces (e.g. "OrbStack Helper"), so it is taken as-is rather than split further.
fn parse_ps_output(raw: &str) -> BTreeMap<u32, ProcessMeta> {
    let mut result = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some(parsed) = parse_ps_line(trimmed) else { continue };
        result.insert(parsed.0, parsed.1);
    }
    result
}

fn parse_ps_line(trimmed: &str) -> Option<(u32, ProcessMeta)> {
    let mut rest = trimmed;

    let (pid_str, tail) = rest.split_once(char::is_whitespace)?;
    rest = tail.trim_start();
    let (ppid_str, tail) = rest.split_once(char::is_whitespace)?;
    rest = tail.trim_start();
    let (user, tail) = rest.split_once(char::is_whitespace)?;
    rest = tail.trim_start();
    let (etime_str, tail) = rest.split_once(char::is_whitespace)?;
    let comm = tail.trim_start();

    let pid = pid_str.parse::<u32>().ok()?;
    let ppid = ppid_str.parse::<u32>().ok()?;
    let elapsed = parse_etime(etime_str)?;
    if comm.is_empty() {
        return None;
    }

    let start_time = SystemTime::now()
        .checked_sub(elapsed)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    Some((
        pid,
        ProcessMeta {
            ppid,
            user: user.to_string(),
            start_time,
            comm: comm.to_string(),
        },
    ))
}

/// Parse macOS `ps etime`, which comes in one of three shapes depending on how long
/// the process has run: `mm:ss`, `hh:mm:ss`, or `dd-hh:mm:ss`. Returns `None` rather
/// than panicking on anything else, since this is parsing external command output.
fn parse_etime(raw: &str) -> Option<Duration> {
    let (days, rest) = match raw.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, raw),
    };

    let fields: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [h, m, s] => (h.parse::<u64>().ok()?, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        [m, s] => (0, m.parse::<u64>().ok()?, s.parse::<u64>().ok()?),
        _ => return None,
    };

    let total_seconds = ((days * 24 + hours) * 60 + minutes) * 60 + seconds;
    Some(Duration::from_secs(total_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- signal_target: the leader check that decides how wide a stop may reach ----

    /// The Server leads its own group, so the group IS its own tree — signal all of
    /// it, which is what F8/CONTEXT.md "aimed at the Server and everything it started"
    /// asks for.
    #[test]
    fn signal_target_signals_the_group_when_the_pid_leads_it() {
        assert_eq!(signal_target(4242, 4242), Ok(SignalTarget::Group(4242)));
    }

    /// The audited wrong-target case: an agent-started dev server that is merely a
    /// MEMBER of a group led by an unrelated process. Signaling `-pgid` here would
    /// reach that entire foreign tree, so the signal must narrow to the bare pid.
    #[test]
    fn signal_target_narrows_to_the_bare_pid_when_the_pid_does_not_lead_the_group() {
        assert_eq!(signal_target(4242, 991), Ok(SignalTarget::JustThisProcess(4242)));
    }

    #[test]
    fn signal_target_refuses_invalid_process_groups() {
        // -1 is getpgid's lookup failure; 0 and 1 are never a real dev-server group.
        assert!(signal_target(4242, -1).is_err());
        assert!(signal_target(4242, 0).is_err());
        assert!(signal_target(4242, 1).is_err());
    }

    #[test]
    fn parses_ipv4_and_ipv6_localhost_as_localhost_only() {
        assert_eq!(
            parse_port_binding("127.0.0.1:4399", AddressFamily::V4),
            Some(PortBinding {
                port: 4399,
                family: AddressFamily::V4,
                reachability: Reachability::LocalhostOnly
            })
        );
        assert_eq!(
            parse_port_binding("[::1]:4399", AddressFamily::V6),
            Some(PortBinding {
                port: 4399,
                family: AddressFamily::V6,
                reachability: Reachability::LocalhostOnly
            })
        );
    }

    #[test]
    fn parses_star_as_all_interfaces() {
        assert_eq!(
            parse_port_binding("*:5432", AddressFamily::V4),
            Some(PortBinding {
                port: 5432,
                family: AddressFamily::V4,
                reachability: Reachability::AllInterfaces
            })
        );
    }

    #[test]
    fn rejects_malformed_address() {
        assert_eq!(parse_port_binding("not-an-address", AddressFamily::V4), None);
        assert_eq!(parse_port_binding("127.0.0.1:not-a-port", AddressFamily::V4), None);
        assert_eq!(parse_port_binding("", AddressFamily::V4), None);
    }

    #[test]
    fn same_port_two_families_are_two_distinct_bindings() {
        // Real data (tests/fixtures/lsof_listen_fields_raw.txt): openclaw (pid
        // 54082) binds 18789 on both v4 and v6. Confirms the fix for the bug the
        // coordinator flagged: without `family` on PortBinding, these two sockets
        // were indistinguishable duplicates.
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/lsof_listen_fields_raw.txt"
        ))
        .expect("fixture must exist");
        let sockets = parse_lsof_fields(&raw);

        let openclaw = sockets.iter().find(|s| s.pid == 54082).expect("pid 54082 must be present");
        assert_eq!(openclaw.ports.len(), 2);
        assert!(openclaw.ports.iter().any(|p| p.port == 18789 && p.family == AddressFamily::V4));
        assert!(openclaw.ports.iter().any(|p| p.port == 18789 && p.family == AddressFamily::V6));
    }

    #[test]
    fn parses_real_lsof_fixture_grouping_ports_by_pid() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/lsof_listen_fields_raw.txt"
        ))
        .expect("fixture must exist");

        let sockets = parse_lsof_fields(&raw);

        // OrbStack (pid 2979) held 7 sockets in the captured fixture: 32222 (v4+v6),
        // 60252 (v4 only), and 5432/5433 each on both v4 and v6.
        let orbstack = sockets
            .iter()
            .find(|s| s.pid == 2979)
            .expect("OrbStack pid must be present");
        assert_eq!(orbstack.ports.len(), 7);
        assert!(orbstack.ports.iter().filter(|p| p.port == 5432).count() == 2);
        assert!(orbstack.ports.iter().filter(|p| p.port == 5433).count() == 2);

        // The two 4399 listeners are different pids, not merged.
        let port_4399_pids: Vec<u32> = sockets
            .iter()
            .filter(|s| s.ports.iter().any(|p| p.port == 4399))
            .map(|s| s.pid)
            .collect();
        assert_eq!(port_4399_pids.len(), 2);
    }

    #[test]
    fn owning_app_resolves_to_outermost_bundle() {
        let source = MacosProcessSource;
        let exe = Path::new(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)",
        );
        assert_eq!(source.owning_app(exe), Some("Visual Studio Code".to_string()));
    }

    #[test]
    fn owning_app_none_when_not_in_a_bundle() {
        let source = MacosProcessSource;
        let exe = Path::new("/usr/libexec/rapportd");
        assert_eq!(source.owning_app(exe), None);
    }

    #[test]
    fn owning_app_single_bundle_resolves_to_itself() {
        let source = MacosProcessSource;
        let exe = Path::new("/Applications/GitKraken.app/Contents/MacOS/GitKraken");
        assert_eq!(source.owning_app(exe), Some("GitKraken".to_string()));
    }

    #[test]
    fn parses_etime_variants() {
        assert_eq!(parse_etime("19:03:26"), Some(Duration::from_secs(19 * 3600 + 3 * 60 + 26)));
        assert_eq!(parse_etime("02-18:31:33"), Some(Duration::from_secs((2 * 24 + 18) * 3600 + 31 * 60 + 33)));
        assert_eq!(parse_etime("00:05"), Some(Duration::from_secs(5)));
        assert_eq!(parse_etime("garbage"), None);
    }

    #[test]
    fn parses_real_ps_fixture() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ps_snapshot_raw.txt"
        ))
        .expect("fixture must exist");

        let meta = parse_ps_output(&raw);

        // This is the exact bug an earlier version of this parser had: it split on
        // every whitespace char individually, which made every field after pid
        // empty on column-aligned `ps` output, so `meta` came back empty and
        // `enumerate()` silently dropped every listener. Asserting real fields here
        // is what would have caught it.
        let orbstack = meta.get(&2979).expect("pid 2979 must parse");
        assert_eq!(orbstack.ppid, 1);
        assert_eq!(orbstack.user, "purin");
        assert!(orbstack.comm.ends_with("OrbStack Helper"));

        let openclaw = meta.get(&54082).expect("pid 54082 must parse");
        assert_eq!(openclaw.comm, "/opt/homebrew/bin/node");

        // Comment lines in the fixture must not produce a bogus entry.
        assert_eq!(meta.len(), 14);
    }

    #[test]
    fn parses_real_cwd_batch_fixture() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/lsof_cwd_batch_fields_raw.txt"
        ))
        .expect("fixture must exist");

        let cwds = parse_cwd_batch(&raw);

        assert_eq!(
            cwds.get(&98027),
            Some(&PathBuf::from("/Users/purin/dev/vala-platform/apps/web"))
        );
        assert_eq!(cwds.get(&54082), Some(&PathBuf::from("/Users/purin/.openclaw")));
        // pid 627's cwd was reported as bare "/" in the fixture (a system daemon
        // sandboxed to root) — still a valid path, must not be treated as missing.
        assert_eq!(cwds.get(&627), Some(&PathBuf::from("/")));
    }

    /// Exercises the real `enumerate()` end to end — `lsof`, `ps`, and the batched
    /// cwd lookup, actually spawned and joined together, not each parser tested in
    /// isolation against a captured fixture. Machine-state-dependent (the exact
    /// listeners on the box running this differ every time), so it is `#[ignore]`d
    /// by default and run on demand with `cargo test -- --ignored`. Its purpose is
    /// to catch bugs in the JOIN between the three commands — the class of bug a
    /// per-function test against a fixture cannot see (an earlier version of
    /// `parse_ps_output` passed every isolated test while `enumerate()` silently
    /// returned an empty `Vec` in practice, because the join step depended on it).
    #[test]
    #[ignore]
    fn enumerate_returns_real_data_on_this_machine() {
        let source = MacosProcessSource;
        let listeners = source.enumerate().expect("enumerate must not error");

        assert!(!listeners.is_empty(), "this machine has listeners; enumerate() must find at least one");
        assert!(
            listeners.iter().any(|l| l.cwd.is_some()),
            "at least one real listener must have a resolvable cwd"
        );
        assert!(
            listeners.iter().all(|l| !l.exe_path.as_os_str().is_empty()),
            "every listener must have a non-empty exe_path"
        );
        assert!(
            listeners.iter().all(|l| !l.ports.is_empty()),
            "every listener must be collapsed with at least one port binding"
        );

        for l in &listeners {
            eprintln!(
                "pid={} ppid={} exe={} cwd={:?} ports={:?}",
                l.pid,
                l.ppid,
                l.exe_path.display(),
                l.cwd,
                l.ports
            );
        }
    }
}

#[cfg(test)]
mod identity_live_tests {
    use super::*;
    use crate::platform::ProcessSource;

    /// Real-machine check that A3's identity gate does not refuse a stop for a process
    /// that has not changed. `start_time` is recomputed from `ps` etime on every
    /// enumeration, so two enumerations seconds apart give different SystemTimes for
    /// the same unchanged process — if the gate compared exactly, every ordinary stop
    /// would be refused. Machine-state-dependent, hence #[ignore].
    #[test]
    #[ignore]
    fn identity_gate_accepts_an_unchanged_process_across_two_real_enumerations() {
        use crate::scanner::{refuse_if_identity_changed, ScannedServer, Health};
        use crate::domain::model::{Kind, ProjectAttribution};

        let source = MacosProcessSource;
        let first = source.enumerate().expect("enumerate must succeed");
        let sample = first.first().expect("this machine has at least one listener").clone();

        std::thread::sleep(Duration::from_millis(1500));
        let second = source.enumerate().expect("second enumerate must succeed");

        let target = ScannedServer {
            id: format!("{}:{}", sample.pid, sample.ports[0].port),
            pid: sample.pid,
            command: sample.command.clone(),
            ports: sample.ports.clone(),
            start_time: sample.start_time,
            unattended: false,
            kind: Kind::DevServer,
            attribution: ProjectAttribution::None,
            belongs_to: None,
            health: Health::Unknown,
            title: None,
        };

        let verdict = refuse_if_identity_changed(&second, &target);
        assert!(
            verdict.is_none(),
            "an unchanged real process must pass the identity gate, got: {verdict:?} (etime jitter would break every stop)"
        );
        eprintln!("identity gate accepted unchanged pid {} across a 1.5s gap", sample.pid);
    }
}
