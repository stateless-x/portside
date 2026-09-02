//! Pure domain logic (P1): deriving a Project and Package, assigning a Kind. No
//! `#[cfg]`, no OS calls anywhere under this module. Imports from `platform/` are
//! limited to `RawListener` and the plain data types it carries (`PortBinding`,
//! `Reachability`) — never anything that gathers data itself.

pub mod classify;
pub mod model;
pub mod project;
