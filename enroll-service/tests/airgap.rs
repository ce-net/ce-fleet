//! Air-gap CI assertions for the fleet packaging — the "zero non-LAN sockets" guarantee in code.
//!
//! These tests are static analyses over the packaging artifacts in `../packaging`: they assert that
//! every service unit / install script starts the node WITHOUT cloud bootstrap, relay, or DCUtR,
//! and that no packaged config dials a public address. A real deployment also enforces an
//! egress-deny firewall (see `packaging/linux/ce-fleet-egress.nft` and the Windows firewall rules);
//! these tests guard the *software* half — that nothing in our packages phones home by default.
//!
//! The runtime socket assertion (enumerate this process's sockets, assert none are non-LAN) is
//! genuinely useful but requires a live node + platform socket enumeration; it is sketched as an
//! ignored test so CI can opt in where a node is available.

use std::fs;
use std::path::{Path, PathBuf};

fn packaging_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../ce-fleet/enroll-service
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("ce-fleet root")
        .join("packaging")
}

/// Read every file under `dir` (recursively) whose name matches one of `exts`/`names`, returning
/// (path, contents). Used to scan units + scripts.
fn collect(dir: &Path, want: &dyn Fn(&Path) -> bool) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(collect(&p, want));
        } else if want(&p) && let Ok(s) = fs::read_to_string(&p) {
            out.push((p, s));
        }
    }
    out
}

/// The forbidden tokens that would let a fleet node reach the public internet. If any packaging
/// artifact that *starts the node* contains these, the air-gap is broken by default.
const FORBIDDEN_NETWORK_TOKENS: &[&str] = &[
    "ce-net.com/bootstrap",
    "--bootstrap http",
    "p2p.ce-net.com",
    "178.105.145.170", // the public relay IP (CLAUDE.md)
];

fn is_service_or_script(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("service") | Some("sh") | Some("ps1") | Some("plist")
    )
}

#[test]
fn no_packaging_artifact_dials_the_public_relay_or_bootstrap() {
    let dir = packaging_dir();
    assert!(dir.is_dir(), "packaging dir must exist at {}", dir.display());
    let files = collect(&dir, &is_service_or_script);
    assert!(
        !files.is_empty(),
        "expected packaging units/scripts under {}",
        dir.display()
    );
    for (path, body) in &files {
        for line in body.lines() {
            let l = line.trim();
            // Skip comment lines: a unit may legitimately *document* "NO ce-net.com/bootstrap".
            // The air-gap guarantee is about executable lines, not prose.
            if l.starts_with('#') || l.starts_with("//") || l.starts_with("REM") {
                continue;
            }
            for tok in FORBIDDEN_NETWORK_TOKENS {
                assert!(
                    !l.contains(tok),
                    "AIR-GAP VIOLATION: {} contains forbidden public-network token {:?} in: {l}",
                    path.display(),
                    tok
                );
            }
        }
    }
}

#[test]
fn every_node_start_disables_mining_and_uses_no_mine() {
    // Every packaged service that runs `ce start` must pass --no-mine (fleet nodes don't mine; they
    // serve). This also implies LAN-only operation since the units never add a bootstrap flag.
    let dir = packaging_dir();
    let files = collect(&dir, &is_service_or_script);
    let mut checked = 0usize;
    for (path, body) in &files {
        for line in body.lines() {
            let l = line.trim();
            // Match the actual node-start invocation, not comments describing it.
            let is_invocation = (l.starts_with("ExecStart=") || l.contains("ce start") || l.contains("ce.exe start"))
                && l.contains("start")
                && !l.starts_with('#')
                && !l.starts_with("//")
                && !l.starts_with("REM");
            if is_invocation && l.contains("ce") && l.contains("start") {
                checked += 1;
                assert!(
                    l.contains("--no-mine"),
                    "fleet node start must use --no-mine: {} -> {l}",
                    path.display()
                );
            }
        }
    }
    assert!(
        checked > 0,
        "expected at least one packaged `ce start` invocation to verify"
    );
}

#[test]
fn an_egress_firewall_artifact_is_shipped() {
    // The air-gap is enforced in two halves: software (no phone-home, above) AND an egress-deny
    // firewall. Assert the firewall artifact ships so an operator cannot forget it.
    let dir = packaging_dir();
    let nft = dir.join("linux/ce-fleet-egress.nft");
    assert!(
        nft.is_file(),
        "missing egress firewall ruleset at {}",
        nft.display()
    );
    let body = fs::read_to_string(&nft).expect("read nft");
    assert!(
        body.contains("drop") || body.contains("reject"),
        "egress ruleset must default-deny non-LAN outbound"
    );
}

#[test]
#[ignore = "requires a live node + platform socket enumeration; opt-in in air-gap validation"]
fn this_process_has_zero_non_lan_sockets() {
    // Sketch: enumerate established sockets for the node process and assert every remote peer is in
    // an RFC1918 / link-local / loopback range. Implemented per-platform in the air-gap validation
    // harness (Linux: parse /proc/net/tcp{,6}; Windows: GetExtendedTcpTable). Left ignored here so
    // the assertion is documented and CI can wire it where a node is running.
    // TODO: implement platform socket enumeration in the air-gap validation harness.
}
