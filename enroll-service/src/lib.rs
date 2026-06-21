//! ce-fleet-enroll — the enrollment + rollup **delegate service**, built as an application on CE.
//!
//! It adds **no node code**. It composes three CE building blocks:
//!   - **ce-cap** delegation — the delegate holds an org-root-anchored capability chain and issues
//!     each enrolling node a *strictly weaker*, audience-bound working cap signed by the delegate.
//!     A node honors the chain because it roots at the shared **org root key** (`ce-root`) every
//!     fleet node lists in `<data_dir>/roots`.
//!   - **ce-rs** (`atlas` / `status` / `history`) — the rollup endpoint aggregates the mesh view
//!     across the delegate's subtree, so the browser console hits a few delegates, never 1500 nodes.
//!   - **one-time, nonce-tracked bootstrap caps** — the Tailscale-authkey ergonomics piece: a
//!     reusable, very-short-TTL, tag-scoped bootstrap secret that the delegate hands out exactly
//!     once per node (nonce burned on first use). This is the only new *app-level* state.
//!
//! ## Trust spine (mirrors h-research:fleet §0 and `replicator`)
//! One offline Ed25519 **org root** key signs the delegate's chain. The delegate's working caps are
//! `[..org-root-chain, delegate->node]` — `ce-cap::authorize` accepts them on any fleet node because
//! every link attenuates and the whole thing roots at `ce-root`. Compromising the deploy channel
//! (a poisoned MSI mirror) grants no fleet authority: the working cap is *issued* by the delegate's
//! key, *rooted* at the org key, and *bound* to the enrolling node's id as audience. None of that is
//! in the installer.
//!
//! Privilege can only ever shrink: the working cap intersects the delegate's abilities, clamps the
//! expiry to never outlive the delegate's own cap, and fixes the audience to the enrolling node.

pub mod attenuate;
pub mod nonce;
pub mod rollup;
pub mod service;
pub mod token;

pub use attenuate::{WorkingCapParams, clamp_not_after, issue_working_cap, working_abilities};
pub use nonce::NonceLedger;
pub use rollup::{NodeView, RollupAggregator, SubtreeRollup};
pub use service::{AppState, DelegateConfig, router};
pub use token::{BootstrapPolicy, EnrollOutcome, EnrollRequest};
