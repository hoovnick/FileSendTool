//! The share table: what is being served, under which token, and for how long.

use rand::{RngExt, distr::Alphanumeric};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};

/// Token length. 32 alphanumerics is ~190 bits of entropy from the CSPRNG-backed
/// thread rng, so a link is not findable by guessing.
pub const TOKEN_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Live,
    Expired,
    Exhausted,
    Revoked,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Expired => "expired",
            Self::Exhausted => "limit reached",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Share {
    pub token: String,
    /// Absolute, resolved at creation. The only path the server ever opens.
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub created: SystemTime,
    pub expires: Option<SystemTime>,
    pub max_downloads: Option<u32>,
    pub downloads: u32,
    pub revoked: bool,
}

impl Share {
    pub fn status(&self) -> Status {
        if self.revoked {
            return Status::Revoked;
        }
        if let Some(e) = self.expires {
            if SystemTime::now() >= e {
                return Status::Expired;
            }
        }
        if let Some(m) = self.max_downloads {
            if self.downloads >= m {
                return Status::Exhausted;
            }
        }
        Status::Live
    }

    pub fn is_live(&self) -> bool {
        self.status() == Status::Live
    }

    pub fn time_left(&self) -> Option<Duration> {
        self.expires
            .map(|e| e.duration_since(SystemTime::now()).unwrap_or_default())
    }
}

pub type Store = Arc<RwLock<HashMap<String, Share>>>;

pub fn new_store() -> Store {
    Arc::new(RwLock::new(HashMap::new()))
}

pub fn new_token() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(TOKEN_LEN)
        .map(char::from)
        .collect()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// "2d 3h", "5h 12m", "47m", "30s" — coarse on purpose, it is a reassurance not a timer.
pub fn human_left(d: Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        return "expired".into();
    }
    let (days, hours, mins) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        format!("{s}s")
    }
}
