//! Library surface for `commonsc-devkit`.
//!
//! The CLI binary and the marketplace HTTP service share the same gate code,
//! manifest helpers, signing, and bundle-building. Exposing them here keeps
//! the brief's §3.6 invariant alive: validate locally and validate server-side
//! must be the same code — only the fixtures differ.

pub mod canonical;
pub mod init;
pub mod manifest;
pub mod publish;
pub mod register;
pub mod signing;
pub mod validate;
