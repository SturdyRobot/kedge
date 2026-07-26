//! # kedge-forge
//!
//! **Learn what a task actually needed.** Point it at a recorded agent
//! trajectory and it derives the least-privilege capability manifest that would
//! have permitted that run and nothing else.
//!
//! ```text
//! kedge-bench   →  runs a task           →  kedge-ledger stores the trajectory
//!                                              ↓
//! kedge-forge   →  observe(trajectory)   →  a manifest of exactly what it used
//!                                              ↓
//! kedge-skill   →  enforces that manifest on the next run
//! ```
//!
//! ## Why a manifest derived from a run is worth anything
//!
//! Least privilege is normally aspirational, because nobody knows the true
//! minimum authority a task requires. `kedge-skill` can *measure* the gap
//! between what a manifest declares and what a run exercised, but only while
//! the run is happening. This crate does it from the ledger, after the fact,
//! for runs that already happened — which is the difference between a debugging
//! aid and something you can point at a year of history.
//!
//! ## It verifies its own output
//!
//! The derivation is only half the job. An observation can be perfectly correct
//! and still be **unmanifestable**: a trajectory that ran `cargo test && curl …`
//! exercised a real capability that *no* manifest can grant, because
//! `kedge-skill` denies any command carrying a shell metacharacter and always
//! will. Emitting a manifest for that run would produce a file that rejects the
//! very trajectory it was derived from.
//!
//! So [`observe_verified`] replays the trajectory through a real
//! [`kedge_skill::SkillGuard`] built from the manifest it just emitted, and
//! reports [`Verification::Failed`] rather than handing back something that
//! looks authoritative and is not. The round-trip is a property of the API, not
//! a thing the tests happen to check.
//!
//! Both halves share `kedge_skill`'s derivation and its single manifest
//! renderer, so the observer and the enforcer cannot drift apart.

pub mod gate;
pub mod observe;
pub mod reach;
pub mod registry;

pub use gate::{gate, EvalOutcome, GateNote, GateReason, GateVerdict};
pub use observe::{
    observe, observe_verified, verify, ObservedAuthority, Unobservable, Verification,
};
pub use reach::{general_agent_manifest, reach, Reach, MAX_WALK};
pub use registry::{HistoryEntry, Registry, RegistryError, SkillId, SkillRecord};

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("the observed manifest does not parse: {0}")]
    Manifest(#[from] kedge_skill::ManifestError),
    #[error("ledger: {0}")]
    Ledger(#[from] kedge_ledger::LedgerError),
    #[error("walking the workspace: {0}")]
    Io(#[from] std::io::Error),
    #[error("registry: {0}")]
    Registry(#[from] registry::RegistryError),
}
