//! How the launcher lives next to the app manager (SAM) and the watchdog in `takeover.sh`.
use crate::sdl::{log, Event, SDL_PushEvent};
use std::ffi::{c_int, CStr};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

const CRASH_FILE: &CStr = c"/tmp/tvlauncher-crashes";

pub static RELAUNCH: AtomicBool = AtomicBool::new(false);
static REGISTERED: AtomicBool = AtomicBool::new(false);
/// The `luna-send` that holds the registration, 0 when none (or when [`unregister`] ended it).
static HOLDER: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_crash(sig: c_int) {
    // Only async-signal-safe calls: open, write, close, signal, raise. One line per crash, the time in seconds;
    // the watchdog gives LG's Home back after three within a minute.
    unsafe {
        let mut digits = [b'\n'; 24];
        let (mut n, mut i) = (libc::time(std::ptr::null_mut()) as u64, digits.len() - 1);
        while n > 0 || i == digits.len() - 1 {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        let fd = libc::open(
            CRASH_FILE.as_ptr(),
            libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT,
            0o644,
        );
        if fd >= 0 {
            libc::write(fd, digits[i..].as_ptr().cast(), digits.len() - i);
            libc::close(fd);
        }
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Counts a crash (a signal, a panic that aborts, or a failed start) for the watchdog.
pub fn install_crash_counter() {
    for sig in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT] {
        unsafe { libc::signal(sig, on_crash as *const () as usize) };
    }
}

pub fn note_failed_start() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(CRASH_FILE.to_str().expect("ascii"))
        .and_then(|mut f| f.write_all(format!("{now}\n").as_bytes()));
}

/// Registers as Home with the app manager, which then sends a "relaunch" on the Home key instead of
/// restarting the process, and tells it to exit on "close". When the app manager itself goes away the
/// process exits too: it restarts at standby, and a launcher left over from before is no longer known as Home.
///
/// Opt-in (`<DIR>/register`): not yet seen to bring the window to the front on a relaunch.
///
/// False when it was registered already.
pub fn register(wake: u32) -> bool {
    if REGISTERED.swap(true, Ordering::SeqCst) {
        return false;
    }
    std::thread::spawn(move || {
        let child = Command::new("/usr/bin/luna-send")
            .args([
                "-i",
                "-a",
                "com.webos.app.home",
                "luna://com.webos.applicationManager/registerApp",
                "{}",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return log("register: luna-send did not start");
        };
        HOLDER.store(child.id() as i32, Ordering::SeqCst);
        let mut registered: Option<Instant> = None;
        for line in BufReader::new(child.stdout.take().expect("piped"))
            .lines()
            .map_while(Result::ok)
        {
            log(&format!("register: {}", line.chars().take(200).collect::<String>()));
            if line.contains("\"returnValue\"") && line.contains("false") {
                return;
            }
            if line.contains("\"registered\"") {
                registered = Some(Instant::now());
            } else if line.contains("\"relaunch\"") {
                // The app manager answers the registration itself with a relaunch (seen on the TV): not the Home key.
                if registered.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    continue;
                }
                RELAUNCH.store(true, Ordering::SeqCst);
                unsafe { SDL_PushEvent(&mut Event::new(wake)) };
            } else if line.contains("\"close\"") {
                std::process::exit(0);
            }
        }
        let _ = child.wait();
        if registered.is_some() && HOLDER.load(Ordering::SeqCst) != 0 {
            log("register: the app manager went away");
            std::process::exit(0);
        }
    });
    true
}

/// Ends the registration. While registered, the Home key is ignored with the launcher in front (seen on the TV);
/// unregistered, the app manager restarts the launcher, which lands on Home.
pub fn unregister() {
    let pid = HOLDER.swap(0, Ordering::SeqCst);
    if pid > 0 {
        unsafe { libc::kill(pid, libc::SIGTERM) };
        log("register: ended");
    }
    REGISTERED.store(false, Ordering::SeqCst);
}

/// Watches the remotes for the Home button. The app manager ignores it while Home is in front, so the launcher hears it
/// here and starts over as on a relaunch. The devices are only read, never grabbed: the TV still gets every key.
/// Remotes that pair later are picked up within 30 s.
#[cfg(target_os = "linux")]
pub fn watch_home(wake: u32) {
    use std::collections::HashSet;
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    /// The Magic Remote's Home button, as the kernel reports it (read on the C1).
    const KEY_HOME: u16 = 773;
    std::thread::spawn(move || {
        let open: Arc<Mutex<HashSet<String>>> = Arc::default();
        loop {
            let nodes = std::fs::read_dir("/dev/input").into_iter().flatten().flatten();
            for path in nodes.map(|e| e.path().display().to_string()) {
                if !path.contains("/event") || !open.lock().unwrap().insert(path.clone()) {
                    continue;
                }
                let open = open.clone();
                std::thread::spawn(move || {
                    if let Ok(mut f) = std::fs::File::open(&path) {
                        let mut ev = [0u8; std::mem::size_of::<libc::input_event>()];
                        while f.read_exact(&mut ev).is_ok() {
                            let e: libc::input_event = unsafe { std::ptr::read_unaligned(ev.as_ptr().cast()) };
                            if e.type_ == 1 && e.code == KEY_HOME && e.value == 1 {
                                log("key Home");
                                RELAUNCH.store(true, Ordering::SeqCst);
                                unsafe { SDL_PushEvent(&mut Event::new(wake)) };
                            }
                        }
                    }
                    open.lock().unwrap().remove(&path);
                });
            }
            std::thread::sleep(Duration::from_secs(30));
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn watch_home(_wake: u32) {}
