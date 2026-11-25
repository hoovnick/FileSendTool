//! Supervises the `cloudflared` child process that gives us a public HTTPS origin.
//!
//! A quick tunnel's hostname lives and dies with its process, so when the child
//! restarts every link issued under the old hostname is dead. That is surfaced
//! rather than hidden.

use std::{
    ffi::OsString,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const SUFFIX: &str = ".trycloudflare.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Waiting for cloudflared to hand us a hostname.
    Starting,
    /// Live, with the public origin (no trailing slash).
    Up(String),
    /// The child died and is being brought back; links issued before are dead.
    Restarting,
    /// Could not run cloudflared at all.
    Failed(String),
}

impl State {
    pub fn base(&self) -> Option<&str> {
        match self {
            Self::Up(b) => Some(b.as_str()),
            _ => None,
        }
    }
}

pub struct Tunnel {
    state: Arc<RwLock<State>>,
    child: Arc<Mutex<Option<Child>>>,
    shutdown: Arc<AtomicBool>,
    /// Bumped whenever a new hostname is issued. The UI uses a change here to
    /// warn that previously copied links no longer resolve.
    pub epoch: Arc<RwLock<u64>>,
}

impl Tunnel {
    pub fn start(port: u16) -> Arc<Self> {
        let t = Arc::new(Self {
            state: Arc::new(RwLock::new(State::Starting)),
            child: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            epoch: Arc::new(RwLock::new(0)),
        });

        let me = t.clone();
        std::thread::Builder::new()
            .name("fst-tunnel".into())
            .spawn(move || me.supervise(port))
            .expect("spawn tunnel supervisor");

        t
    }

    pub fn state(&self) -> State {
        self.state.read().unwrap().clone()
    }

    pub fn base(&self) -> Option<String> {
        self.state.read().unwrap().base().map(str::to_owned)
    }

    pub fn epoch(&self) -> u64 {
        *self.epoch.read().unwrap()
    }

    /// Stop supervising and kill the child. Safe to call more than once.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn supervise(&self, port: u16) {
        // cloudflared auto-loads ~/.cloudflared/config.yml. If one is there for an
        // unrelated named tunnel, its `ingress` rules apply to our quick tunnel too,
        // and a catch-all `service: http_status:404` makes every link return an
        // empty 404 from Cloudflare's edge while everything here looks healthy.
        // Point cloudflared at our own empty config so only `--url` decides routing.
        let isolated = match write_isolated_config() {
            Ok(p) => p,
            Err(e) => {
                *self.state.write().unwrap() = State::Failed(format!(
                    "could not write a private cloudflared config ({e})"
                ));
                return;
            }
        };

        let program = cloudflared_program();

        let mut first = true;
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            if !first {
                *self.state.write().unwrap() = State::Restarting;
            }
            first = false;

            let spawned = Command::new(&program)
                .arg("--config")
                .arg(&isolated)
                .args([
                    "tunnel",
                    "--no-autoupdate",
                    "--url",
                    &format!("http://127.0.0.1:{port}"),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .no_console()
                .spawn();

            let mut child = match spawned {
                Ok(c) => c,
                Err(e) => {
                    *self.state.write().unwrap() = State::Failed(format!(
                        "could not run cloudflared ({e}). Expected it next to FST.exe, \
                         or installed on PATH."
                    ));
                    return;
                }
            };

            // Tie cloudflared's life to ours, so a crash or a force-kill of this
            // process cannot leave a public ingress running unattended.
            adopt_into_job(&child);

            let stderr = child.stderr.take();
            *self.child.lock().unwrap() = Some(child);

            // cloudflared announces the hostname on stderr; read until the pipe
            // closes, which also tells us the child is finished.
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if self.shutdown.load(Ordering::SeqCst) {
                        return;
                    }
                    if self.state().base().is_none() {
                        if let Some(url) = find_hostname(&line) {
                            *self.epoch.write().unwrap() += 1;
                            *self.state.write().unwrap() = State::Up(url);
                        }
                    }
                }
            }

            // Pipe closed: reap it, then back round to restart.
            if let Some(mut c) = self.child.lock().unwrap().take() {
                let _ = c.wait();
            }
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            *self.state.write().unwrap() = State::Restarting;
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(windows)]
const CLOUDFLARED_EXE: &str = "cloudflared.exe";
#[cfg(not(windows))]
const CLOUDFLARED_EXE: &str = "cloudflared";

/// Decide which cloudflared to run.
///
/// A copy shipped next to FST wins, so the app works on a machine that has never
/// installed cloudflared and so a bundled known-good version is not quietly
/// replaced by whatever happens to be on PATH. Falls back to the bare name,
/// which lets the OS search PATH as before.
fn cloudflared_program() -> OsString {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join(CLOUDFLARED_EXE),
                dir.join("vendor").join(CLOUDFLARED_EXE),
            ] {
                if candidate.is_file() {
                    return candidate.into_os_string();
                }
            }
        }
    }
    OsString::from(CLOUDFLARED_EXE)
}

/// Put the child in a job object that kills its members when the job handle
/// closes — which Windows does for us when this process dies, however it dies.
///
/// The normal exits (tray Exit, the window loop returning) already kill the
/// child explicitly. This covers the crash and force-kill paths, where the
/// thing left behind would otherwise be a public ingress.
#[cfg(windows)]
fn adopt_into_job(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // Held open for the life of the process on purpose: closing it is the
    // signal that kills the children.
    static JOB: OnceLock<usize> = OnceLock::new();

    let job = *JOB.get_or_init(|| {
        // SAFETY: a fresh unnamed job object with default security.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            job as usize
        }
    });

    if job == 0 {
        return;
    }
    // SAFETY: `job` is a live job handle and the child handle is owned by `child`.
    unsafe {
        AssignProcessToJobObject(job as *mut _, child.as_raw_handle() as *mut _);
    }
}

#[cfg(not(windows))]
fn adopt_into_job(_child: &Child) {}

/// Write (once per run is enough, but idempotent) an empty cloudflared config of
/// our own, and return its path. Deliberately does not touch the user's
/// `~/.cloudflared/config.yml`, which may be driving a real named tunnel.
fn write_isolated_config() -> std::io::Result<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("FST"))
        .unwrap_or_else(std::env::temp_dir);

    let dir = match std::fs::create_dir_all(&dir) {
        Ok(()) => dir,
        Err(_) => std::env::temp_dir(),
    };

    let path = dir.join("cloudflared-isolated.yml");
    std::fs::write(
        &path,
        b"# Written by FST.\n\
          # Intentionally empty: it keeps cloudflared from picking up\n\
          # ~/.cloudflared/config.yml, whose ingress rules would otherwise\n\
          # take over FST's quick tunnel. Safe to delete.\n",
    )?;
    Ok(path)
}

/// Pull `https://<something>.trycloudflare.com` out of a log line. cloudflared
/// prints it inside an ASCII box, so the URL is surrounded by spaces and pipes.
fn find_hostname(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest.find(SUFFIX)? + SUFFIX.len();
    let url = &rest[..end];
    if url.len() > 200 || url.chars().any(char::is_whitespace) {
        return None;
    }
    Some(url.to_string())
}

/// Keep cloudflared from flashing up a console window of its own.
trait NoConsole {
    fn no_console(&mut self) -> &mut Self;
}

impl NoConsole for Command {
    #[cfg(windows)]
    fn no_console(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_console(&mut self) -> &mut Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::find_hostname;

    #[test]
    fn pulls_url_out_of_a_boxed_log_line() {
        let line = "2026-11-24T18:02:11Z INF |  https://brave-amber-tiger-mint.trycloudflare.com    |";
        assert_eq!(
            find_hostname(line).as_deref(),
            Some("https://brave-amber-tiger-mint.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert_eq!(find_hostname("INF Requesting new quick tunnel"), None);
        assert_eq!(find_hostname("see https://developers.cloudflare.com/"), None);
    }
}
