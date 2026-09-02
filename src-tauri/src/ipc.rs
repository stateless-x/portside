//! Wire types for docs/IPC.md — the FROZEN contract with the UI. Every field name,
//! camelCase rename, and string literal here must match that document exactly: the UI
//! in src/ is built against the document, not against this file, so a mismatch here is
//! a broken contract rather than a rename the other side follows.
//!
//! Deliberately separate from `domain::model`: the domain types encode CONTEXT.md's
//! vocabulary for internal reasoning, not JSON wire format. Serializing them directly
//! would let a `#[serde(rename)]` typo silently become "the spec", and a later domain
//! refactor would silently break the frozen contract instead of failing to compile
//! here. The `From` conversions below are the one place that translation happens.

use serde::Serialize;

use crate::domain::model::{Kind, ProjectAttribution};

/// A Port Binding as the UI sees it (docs/IPC.md `Port`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Port {
    pub number: u16,
    pub family: PortFamily,
    pub reachability: PortReachability,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PortFamily {
    V4,
    V6,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortReachability {
    Localhost,
    AllInterfaces,
}

impl From<&crate::platform::PortBinding> for Port {
    fn from(b: &crate::platform::PortBinding) -> Self {
        Port {
            number: b.port,
            family: match b.family {
                crate::platform::AddressFamily::V4 => PortFamily::V4,
                crate::platform::AddressFamily::V6 => PortFamily::V6,
            },
            reachability: match b.reachability {
                crate::platform::Reachability::LocalhostOnly => PortReachability::Localhost,
                crate::platform::Reachability::AllInterfaces => PortReachability::AllInterfaces,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthWire {
    Responding,
    NotResponding,
    Unknown,
}

/// docs/IPC.md v1.4 `ResourceUsage` — what one Server was using at the latest scan.
///
/// Every displayed Server kind carries one, so the UI can render a placeholder on
/// every row rather than having to know which shapes can and cannot have figures.
///
/// `cpuPercent` crosses the wire as a NUMBER with at most one decimal place, converted
/// from the internal integer tenths at exactly this boundary — the domain and scanner
/// stay integer (they derive `Eq`), and only the wire, which is JSON and has no
/// integer/float distinction to preserve, sees a fractional value. `null` on either
/// field means the figure was not available, which is deliberately different from `0`.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUsageWire {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub pressure: PressureWire,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PressureWire {
    Normal,
    Cpu,
    Memory,
    Both,
}

impl ResourceUsageWire {
    /// Build the wire figure from the internal integer sample plus the sustained
    /// verdict. The tenths-to-percent division is the single place a float enters this
    /// codebase, and it happens on the way out.
    pub fn new(sample: &crate::platform::ResourceSample, pressure: crate::scanner::Pressure) -> Self {
        ResourceUsageWire {
            cpu_percent: sample.cpu_tenths_percent.map(|tenths| f64::from(tenths) / 10.0),
            memory_bytes: sample.memory_bytes,
            pressure: match pressure {
                crate::scanner::Pressure::Normal => PressureWire::Normal,
                crate::scanner::Pressure::Cpu => PressureWire::Cpu,
                crate::scanner::Pressure::Memory => PressureWire::Memory,
                crate::scanner::Pressure::Both => PressureWire::Both,
            },
        }
    }
}

/// docs/IPC.md `Server` — a DevServer row.
///
/// `PartialEq` but not `Eq` since v1.4: `ResourceUsageWire.cpuPercent` is a float on
/// the wire (see that type), and float equality is partial. Nothing compares these
/// structs for total equality — the scanner's change detection works on the domain
/// types, not the wire ones.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerWire {
    /// Stable across scans: `"{pid}:{first_port}"` (PLAN.md: "Server.id must be
    /// STABLE across scans (pid + first port)"). Kept as a string, not a struct, so
    /// the UI can use it as an opaque row key without needing to parse it — and it
    /// must never be parsed back apart on the Rust side either; see scanner.rs for
    /// why (a stale/adversarial id must resolve through the live snapshot, not be
    /// trusted as a literal pid).
    pub id: String,
    pub pid: u32,
    pub package: Option<String>,
    /// Absolute path of the Project root this Server was started from, for the UI's
    /// "go to source" action (docs/IPC.md amendment v1.1). `None` when the Project is
    /// unknown. Derived from the same F2 walk that produces `package`, so it is never
    /// a second, independently-guessed answer to "where does this live".
    pub project_path: Option<String>,
    pub title: Option<String>,
    pub command: String,
    pub ports: Vec<Port>,
    pub uptime_seconds: u64,
    pub health: HealthWire,
    pub unattended: bool,
    pub keep_running: bool,
    /// docs/IPC.md v1.4. Observational only — this figure never changes what stopping
    /// this Server does, never triggers cleanup, and never affects `keepRunning`.
    pub usage: ResourceUsageWire,
}

/// docs/IPC.md v1.4 `ResourceSamples` — the payload of `resources:changed`.
///
/// A flat, id-keyed list rather than the Snapshot's three sections: the UI patches
/// values into rows it has already drawn, addressing them by id, so the sectioning
/// that matters for LAYOUT is irrelevant here. Sending a whole Snapshot instead would
/// invite the UI to rebuild the list, which is precisely what this event exists to
/// avoid.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSamples {
    pub samples: Vec<ResourceSampleEntry>,
    /// ISO 8601, same clock and format as `Snapshot.scannedAt`.
    pub scanned_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSampleEntry {
    pub id: String,
    pub usage: ResourceUsageWire,
}

/// docs/IPC.md `ProjectGroup`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProjectGroup {
    pub project: String,
    pub servers: Vec<ServerWire>,
}

/// docs/IPC.md `WatchOnlyServer`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WatchOnlyServer {
    pub id: String,
    pub label: String,
    pub reason: WatchOnlyReason,
    pub ports: Vec<Port>,
    pub uptime_seconds: u64,
    /// docs/IPC.md v1.4. Present on a Watch Only row for the same reason it is
    /// present everywhere else: the user wants to SEE what these are using — that is
    /// the entire purpose of a row shown but never offered a stop. It changes nothing
    /// about the row's controls, of which there are still none.
    pub usage: ResourceUsageWire,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchOnlyReason {
    YourOwnTool,
    PartOfMacos,
}

/// docs/IPC.md `OtherServer`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OtherServer {
    pub id: String,
    pub label: String,
    pub kind: OtherKind,
    pub guessed_project: Option<String>,
    pub ports: Vec<Port>,
    /// docs/IPC.md v1.4.
    pub usage: ResourceUsageWire,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OtherKind {
    PartOfApp,
    BackgroundService,
}

/// docs/IPC.md `Snapshot` — the payload of `servers:changed` and the return value of
/// `refresh_now`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub projects: Vec<ProjectGroup>,
    pub watch_only: Vec<WatchOnlyServer>,
    pub others: Vec<OtherServer>,
    /// ISO 8601, e.g. "2026-09-02T08:42:21Z".
    pub scanned_at: String,
    /// docs/IPC.md v1.2. True when the most recent scan attempt failed, in which case
    /// every other field is the last snapshot that DID succeed, not current fact.
    /// Exists because a failed scan and a quiet machine are otherwise identical on
    /// screen, and presenting one as the other is exactly what N3 forbids.
    pub scan_failed: bool,
}

/// docs/IPC.md `StopOutcome`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StopOutcome {
    pub id: String,
    pub result: StopResult,
    /// User-facing. Names What This Stops in plain words — never a process name or
    /// raw port number (CONTEXT.md "What This Stops", REQUIREMENTS.md F8).
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopResult {
    Stopped,
    StillRunning,
    Refused,
}

/// The Kind-derived section a Server's Kind belongs in on the wire. Exists so
/// scanner.rs can route a classified Server to the right IPC bucket without
/// duplicating `Kind`'s match arms at every call site; not part of the frozen IPC
/// document itself, purely an internal routing helper.
pub fn wire_section_for(kind: Kind) -> WireSection {
    match kind {
        Kind::DevServer => WireSection::Project,
        Kind::YourOwnTool => WireSection::WatchOnly(WatchOnlyReason::YourOwnTool),
        Kind::PartOfMacOS => WireSection::WatchOnly(WatchOnlyReason::PartOfMacos),
        Kind::PartOfApp => WireSection::Other(OtherKind::PartOfApp),
        Kind::BackgroundService => WireSection::Other(OtherKind::BackgroundService),
    }
}

pub enum WireSection {
    Project,
    WatchOnly(WatchOnlyReason),
    Other(OtherKind),
}

/// Render a `ProjectAttribution` as the "Guessed Project" name shown to the user, per
/// CONTEXT.md: shown as uncertain, never as fact. Used for `OtherServer.guessedProject`
/// — only `BackgroundService` ever carries a `Guessed` attribution (see
/// domain::classify), so this returns `Some` only in that case and `None` otherwise,
/// which is exactly `OtherServer`'s documented "never as fact" contract.
pub fn guessed_project_name(attribution: &ProjectAttribution) -> Option<String> {
    match attribution {
        ProjectAttribution::Guessed(project, _) => Some(project.name.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen contract is JSON string literals, not Rust identifiers. This is the
    /// only check that actually verifies serde's renames produce docs/IPC.md's exact
    /// strings rather than what `snake_case`/`lowercase` would derive from the Rust
    /// variant name by convention (e.g. `PartOfMacos` would naively derive
    /// `"part_of_macos"` correctly here, but `V4`/`V6` under `snake_case` would NOT
    /// produce `"v4"`/`"v6"` — hence `PortFamily` uses `lowercase`, not
    /// `snake_case`, and this test is what catches getting that wrong).
    #[test]
    fn wire_enums_serialize_to_the_literals_ipc_md_specifies() {
        assert_eq!(serde_json::to_string(&PortFamily::V4).unwrap(), "\"v4\"");
        assert_eq!(serde_json::to_string(&PortFamily::V6).unwrap(), "\"v6\"");
        assert_eq!(serde_json::to_string(&PortReachability::Localhost).unwrap(), "\"localhost\"");
        assert_eq!(serde_json::to_string(&PortReachability::AllInterfaces).unwrap(), "\"all_interfaces\"");
        assert_eq!(serde_json::to_string(&HealthWire::Responding).unwrap(), "\"responding\"");
        assert_eq!(serde_json::to_string(&HealthWire::NotResponding).unwrap(), "\"not_responding\"");
        assert_eq!(serde_json::to_string(&HealthWire::Unknown).unwrap(), "\"unknown\"");
        assert_eq!(serde_json::to_string(&WatchOnlyReason::YourOwnTool).unwrap(), "\"your_own_tool\"");
        assert_eq!(serde_json::to_string(&WatchOnlyReason::PartOfMacos).unwrap(), "\"part_of_macos\"");
        assert_eq!(serde_json::to_string(&OtherKind::PartOfApp).unwrap(), "\"part_of_app\"");
        assert_eq!(serde_json::to_string(&OtherKind::BackgroundService).unwrap(), "\"background_service\"");
        assert_eq!(serde_json::to_string(&StopResult::Stopped).unwrap(), "\"stopped\"");
        assert_eq!(serde_json::to_string(&StopResult::StillRunning).unwrap(), "\"still_running\"");
        assert_eq!(serde_json::to_string(&StopResult::Refused).unwrap(), "\"refused\"");
        // docs/IPC.md v1.4.
        assert_eq!(serde_json::to_string(&PressureWire::Normal).unwrap(), "\"normal\"");
        assert_eq!(serde_json::to_string(&PressureWire::Cpu).unwrap(), "\"cpu\"");
        assert_eq!(serde_json::to_string(&PressureWire::Memory).unwrap(), "\"memory\"");
        assert_eq!(serde_json::to_string(&PressureWire::Both).unwrap(), "\"both\"");
    }

    /// docs/IPC.md v1.4's field names, and the tenths-to-percent conversion at the one
    /// boundary where it happens. A tenths value leaking to the wire unconverted would
    /// read as 825% CPU on screen, which is why this asserts the number and not just
    /// the key.
    #[test]
    fn resource_usage_wire_field_names_and_percent_conversion() {
        let usage = ResourceUsageWire::new(
            &crate::platform::ResourceSample { cpu_tenths_percent: Some(825), memory_bytes: Some(1_073_741_824) },
            crate::scanner::Pressure::Both,
        );
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"cpuPercent\":82.5"), "{json}");
        assert!(json.contains("\"memoryBytes\":1073741824"), "{json}");
        assert!(json.contains("\"pressure\":\"both\""), "{json}");
        assert!(!json.contains("cpu_percent"), "{json}");
    }

    #[test]
    fn resource_samples_field_names_match_ipc_md() {
        let samples = ResourceSamples {
            samples: vec![ResourceSampleEntry {
                id: "1:3000".into(),
                usage: ResourceUsageWire::new(&Default::default(), crate::scanner::Pressure::Normal),
            }],
            scanned_at: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&samples).unwrap();
        assert!(json.contains("\"scannedAt\":"), "{json}");
        assert!(json.contains("\"samples\":"), "{json}");
        assert!(!json.contains("scanned_at"), "{json}");
    }

    #[test]
    fn server_wire_field_names_are_camel_case() {
        let server = ServerWire {
            id: "123:4000".into(),
            pid: 123,
            package: None,
            project_path: None,
            title: None,
            command: "node".into(),
            ports: vec![],
            uptime_seconds: 42,
            health: HealthWire::Responding,
            unattended: false,
            keep_running: false,
            usage: ResourceUsageWire::new(&Default::default(), crate::scanner::Pressure::Normal),
        };
        let json = serde_json::to_string(&server).unwrap();
        assert!(json.contains("\"uptimeSeconds\":42"), "{json}");
        assert!(json.contains("\"keepRunning\":false"), "{json}");
        assert!(!json.contains("uptime_seconds"), "{json}");
    }

    #[test]
    fn snapshot_field_names_match_ipc_md() {
        let snapshot = Snapshot { projects: vec![], watch_only: vec![], others: vec![], scanned_at: "2026-01-01T00:00:00Z".into(), scan_failed: false };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"watchOnly\":"), "{json}");
        assert!(json.contains("\"scannedAt\":"), "{json}");
        // docs/IPC.md v1.2 — camelCase on the wire, like every other field here.
        assert!(json.contains("\"scanFailed\":false"), "{json}");
        assert!(!json.contains("scan_failed"), "{json}");
    }
}
