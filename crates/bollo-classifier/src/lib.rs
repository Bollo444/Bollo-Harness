//! The TypeSafe/Jev advisory classifier adapter (BH-021).
//!
//! This crate is deliberately small and outside `bollo-providers`: Jev is not
//! an agent model and cannot implement the `Provider` port (no text, no tool
//! calls). It implements the `RiskClassifier` port from `bollo-policy` over
//! the shared `Transport` boundary, so all tests run against a deterministic
//! fake and the real HTTPS path stays behind the `live-http` feature.
//!
//! The adapter cannot authorize anything: it maps either a valid typed answer
//! or a documented failure to a [`bollo_policy::classifier::ClassifierVerdict`],
//! and the escalation rule can only turn `allow` into `ask`.

pub mod typesafe;

pub use typesafe::{ClassifierConfigError, TypeSafeClassifier, TypeSafeSettings};
