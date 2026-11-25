//! Test harness: stand up the FST server with shares registered up front, print
//! what is needed to drive it, and stay alive until killed.
//!
//! Not part of the shipped binary — this exists so the server can be exercised
//! end to end without a window.
//!
//! Usage:
//!   cargo run --example serve_once -- [--tunnel] [--expires-secs N]
//!                                     [--max-downloads N] <file> [more files...]

use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use fst::{
    server,
    share::{self, Share},
    tunnel,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut want_tunnel = false;
    let mut expires_secs: Option<u64> = None;
    let mut max_downloads: Option<u32> = None;
    let mut files: Vec<PathBuf> = Vec::new();

    while let Some(a) = args.next() {
        match a.as_str() {
            "--tunnel" => want_tunnel = true,
            "--expires-secs" => {
                expires_secs = args.next().and_then(|v| v.parse().ok());
            }
            "--max-downloads" => {
                max_downloads = args.next().and_then(|v| v.parse().ok());
            }
            other => files.push(PathBuf::from(other)),
        }
    }

    if files.is_empty() {
        eprintln!("need at least one file");
        std::process::exit(2);
    }

    let store = share::new_store();
    let log = server::new_log();
    let port = server::spawn(store.clone(), log.clone()).expect("server");

    println!("PORT={port}");

    for f in &files {
        let meta = std::fs::metadata(f).expect("stat file");
        let path = std::fs::canonicalize(f).expect("canonicalize");
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "download".into());
        let token = share::new_token();
        let now = SystemTime::now();

        store.write().unwrap().insert(
            token.clone(),
            Share {
                token: token.clone(),
                path,
                name: name.clone(),
                size: meta.len(),
                created: now,
                expires: expires_secs.map(|s| now + Duration::from_secs(s)),
                max_downloads,
                downloads: 0,
                revoked: false,
            },
        );
        println!("SHARE\t{name}\t{token}\t{}", meta.len());
    }

    // A deliberately revoked share, so the 404-parity assertions have a target.
    {
        let token = share::new_token();
        let path = std::fs::canonicalize(&files[0]).unwrap();
        let now = SystemTime::now();
        store.write().unwrap().insert(
            token.clone(),
            Share {
                token: token.clone(),
                path,
                name: "revoked.bin".into(),
                size: 0,
                created: now,
                expires: None,
                max_downloads: None,
                downloads: 0,
                revoked: true,
            },
        );
        println!("REVOKED\t{token}");
    }

    if want_tunnel {
        let t = tunnel::Tunnel::start(port);
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        loop {
            match t.state() {
                tunnel::State::Up(base) => {
                    println!("PUBLIC={base}");
                    break;
                }
                tunnel::State::Failed(e) => {
                    println!("TUNNEL_FAILED={e}");
                    break;
                }
                _ if std::time::Instant::now() > deadline => {
                    println!("TUNNEL_TIMEOUT=1");
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        println!("READY=1");
        // Keep the tunnel object alive for the process lifetime.
        std::mem::forget(t);
    } else {
        println!("READY=1");
    }

    use std::io::Write;
    std::io::stdout().flush().ok();

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
