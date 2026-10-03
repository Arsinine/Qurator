//! SEMANTIC_MODEL.md Tier A invariants (QURATOR-1, M0 lane A).
//!
//! Source-tree sweeps: A1 (frontend has no network/SQL surface), A2
//! (`Connection::open*` only in `db.rs`), A4 (write-capable file APIs only in
//! the — M0-empty — allowlist), A5 (no identity/sync dependencies). The sweeps
//! only walk `app/src` and `app/src-tauri/src`, never `tests/`, so the needle
//! literals in this file are not self-hits.

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The SvelteKit frontend: `app/src-tauri/../src` == `app/src`.
fn frontend_dir() -> PathBuf {
    manifest_dir().join("../src")
}

fn backend_src_dir() -> PathBuf {
    manifest_dir().join("src")
}

/// Recursively collect files under `dir` whose extension is in `exts`, never
/// descending into generated/scaffold directories. Sorted for stable output.
fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !matches!(name, "node_modules" | ".svelte-kit" | "build") {
                walk(&path, exts, out);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

fn collect(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(dir, exts, &mut files);
    files.sort();
    files
}

fn read_source(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Blank the body of every item carrying a `#[cfg(test)]` attribute while
/// preserving line count, so sweeps never see test-only code yet reported
/// line numbers still point into the original file. A brace-depth tracker:
/// after the attributed item's first line, every line is blanked until brace
/// depth returns to zero.
fn strip_test_modules(src: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut armed = false; // saw `#[cfg(test)]`, waiting for the item
    let mut in_item = false; // inside the attributed item's braces
    let mut depth = 0usize;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if in_item {
            depth += line.matches('{').count();
            depth -= line.matches('}').count().min(depth);
            out.push("");
            if depth == 0 {
                in_item = false;
                armed = false;
            }
            continue;
        }
        if trimmed.starts_with("#[cfg(test)]") {
            armed = true;
            out.push(line);
            continue;
        }
        if armed {
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("#[") {
                out.push(line); // further attributes or comments before the item
                continue;
            }
            // The item's first line: blank it and start tracking its braces.
            depth += line.matches('{').count();
            depth -= line.matches('}').count().min(depth);
            armed = false;
            in_item = depth > 0;
            out.push("");
            continue;
        }
        out.push(line);
    }
    let mut stripped = out.join("\n");
    if src.ends_with('\n') {
        stripped.push('\n');
    }
    stripped
}

/// Sweep `app/src-tauri/src/**/*.rs` for `needles`, skipping `allowlist`ed
/// file names and with `#[cfg(test)]` bodies stripped. Hits are reported as
/// `path:line: needle`.
fn sweep_backend(needles: &[&str], allowlist: &[&str]) -> Vec<String> {
    let files = collect(&backend_src_dir(), &["rs"]);
    assert!(
        !files.is_empty(),
        "backend sweep found no .rs files under {}",
        backend_src_dir().display()
    );
    let mut hits = Vec::new();
    for file in files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if allowlist.contains(&name) {
            continue;
        }
        for (idx, line) in strip_test_modules(&read_source(&file)).lines().enumerate() {
            for needle in needles {
                if line.contains(needle) {
                    hits.push(format!("{}:{}: {}", file.display(), idx + 1, needle));
                }
            }
        }
    }
    hits
}

/// A1: the frontend reaches the core only via Tauri invoke/events — no
/// network clients and no SQL anywhere in `app/src`.
#[test]
fn sem_a1_frontend_has_no_network_or_sql_surface() {
    const NEEDLES: &[(&str, bool)] = &[
        ("fetch(", false),
        ("XMLHttpRequest", false),
        ("WebSocket", false),
        ("axios", false),
        ("sqlite", true), // case-insensitive
    ];
    let files = collect(&frontend_dir(), &["svelte", "ts", "js", "css"]);
    assert!(
        !files.is_empty(),
        "A1 sweep found no frontend files under {}",
        frontend_dir().display()
    );
    let mut hits = Vec::new();
    for file in &files {
        for (idx, line) in read_source(file).lines().enumerate() {
            for &(needle, case_insensitive) in NEEDLES {
                let found = if case_insensitive {
                    line.to_lowercase().contains(&needle.to_lowercase())
                } else {
                    line.contains(needle)
                };
                if found {
                    hits.push(format!("{}:{}: {}", file.display(), idx + 1, needle));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "A1 violated: no network/SQL surface allowed in app/src:\n{}",
        hits.join("\n")
    );
}

/// A2: `Connection::open*` (covering `open_in_memory`, `open_with_flags`)
/// appears only in `db.rs` production code.
#[test]
fn sem_a2_connection_open_only_in_db_rs() {
    let hits = sweep_backend(&["Connection::open"], &["db.rs"]);
    assert!(
        hits.is_empty(),
        "A2 violated: Connection::open* is only allowed in db.rs:\n{}",
        hits.join("\n")
    );
}

/// A4: write-capable file APIs appear only in allowlisted modules; the
/// allowlist is empty in M0. (`fs::remove` as a prefix covers
/// `remove_file`/`remove_dir`/`remove_dir_all`; `fs::create_dir_all` is not
/// an A4 API and is not matched.)
#[test]
fn sem_a4_no_write_capable_file_apis_outside_allowlist() {
    let hits = sweep_backend(
        &[
            "File::create",
            "fs::write",
            "fs::rename",
            "fs::remove",
            "OpenOptions",
        ],
        &[],
    );
    assert!(
        hits.is_empty(),
        "A4 violated: write-capable file APIs outside the (empty) M0 allowlist:\n{}",
        hits.join("\n")
    );
}

/// A5: 1.0 carries no identity or sync surface — no `iroh*`, `secp256k1*`,
/// `nostr*`, `bip39*`, or `*schnorr*` key in any dependency table of
/// `app/src-tauri/Cargo.toml`. Hand-parsed; std only, no `toml` crate.
#[test]
fn sem_a5_no_sync_or_identity_deps_in_1_0() {
    const BANNED_PREFIXES: &[&str] = &["iroh", "secp256k1", "nostr", "bip39"];
    const BANNED_SUBSTRINGS: &[&str] = &["schnorr"];
    let cargo_toml = manifest_dir().join("Cargo.toml");
    let mut in_dep_table = false;
    let mut hits = Vec::new();
    for (idx, raw) in read_source(&cargo_toml).lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let table = line.trim_matches(|c| c == '[' || c == ']');
            in_dep_table = table == "dependencies"
                || table == "dev-dependencies"
                || table == "build-dependencies"
                || (table.starts_with("target.") && table.ends_with(".dependencies"));
            continue;
        }
        if !in_dep_table {
            continue;
        }
        let Some((key, _value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim_matches('\'');
        let banned = BANNED_PREFIXES.iter().any(|p| key.starts_with(p))
            || BANNED_SUBSTRINGS.iter().any(|s| key.contains(s));
        if banned {
            hits.push(format!("{}:{}: {}", cargo_toml.display(), idx + 1, key));
        }
    }
    assert!(
        hits.is_empty(),
        "A5 violated: identity/sync dependencies are banned in 1.0:\n{}",
        hits.join("\n")
    );
}

/// The stripper is load-bearing for A2/A4: a `#[cfg(test)]` module body must
/// vanish from the sweep while production lines survive and line count holds.
#[test]
fn strip_test_modules_blanks_cfg_test_bodies() {
    let src = "\
use rusqlite::Connection;

pub fn open(path: &str) -> Connection {
    Connection::open(path).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_test_db() {
        let conn = Connection::open(\"test.db\");
        assert!(conn.is_ok());
    }
}
";
    let stripped = strip_test_modules(src);
    assert_eq!(
        stripped.lines().count(),
        src.lines().count(),
        "line count must be preserved"
    );
    assert!(
        stripped.contains("pub fn open(path: &str) -> Connection {"),
        "production lines must survive: {stripped}"
    );
    assert!(
        stripped.contains("Connection::open(path)"),
        "production code must survive: {stripped}"
    );
    assert!(
        !stripped.contains("Connection::open(\"test.db\")"),
        "test-module body must be blanked: {stripped}"
    );
}
