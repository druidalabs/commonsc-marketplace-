//! Tier-1 sandbox host.
//!
//! The host is the only component with access to the user's data and the network.
//! Plugin code runs inside a Deno sidecar that loads Pyodide; the bridge between
//! them carries validated, schema-typed records — not raw file bytes. The privacy
//! invariant ("absence of capability, not trust") is enforced at the OS level by
//! the Deno permission flags the host passes when spawning the sidecar.

pub mod sidecar;
