//! Runtime: the pieces that need a real machine — containment, spawning, health, teardown.
//!
//! **P2 / W2 workstream.** The contract layer (`manager`) defines the seams this module fills:
//! [`crate::manager::startup::ContainmentProbe`], [`crate::manager::SessionSpawner`] and
//! [`crate::manager::ViewOps`]. Slices land here in order:
//!
//! 1. `containment` — cgroup v2 delegation detection + the keeper prep. Class rules and raw
//!    evidence: `p2/cgroup-matrix/` and `fixtures/README.md` §Containment classes.
//! 2. `spawner` — the L1 chain (mutter `--headless` parent → nested Hyprland → on-demand
//!    outputs) as a daemon-managed child: readiness, teardown, and the health pass.
//! 3. `service` — the real dispatcher behind `daiorama serve` ([`service::RuntimeService`]):
//!    the lifecycle verbs over the registry, speaking the pinned wire shapes.
//!
//! The CLI's `serve`/`daemon` branches drive [`service::RuntimeService`] (W3, live): this
//! module is the production half behind `daiorama serve`.
#![allow(dead_code)] // wires in as the slices land; unit tests exercise everything today

pub mod capture;
pub mod containment;
pub mod rate;
pub mod retention;
pub mod service;
pub mod spawner;
pub mod views;
