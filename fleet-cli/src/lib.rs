//! `ce-fleet` — a Rust CLI + daemon for owning a ce-net device mesh: list nearby nodes, pair
//! devices by mutual consent (an Accept/Deny prompt on the target, no codes or token copy-paste),
//! and grant access. The authority issued is a standard `ce-cap` capability (the same token
//! `ce grant` prints); this tool only makes minting and delivering it a two-click flow.
//!
//! It is an app over `ce-rs` + `ce-cap` (no node changes), installed via `ce app`.

pub mod daemon;
pub mod pair;
pub mod proto;
pub mod util;
pub mod wallet;
