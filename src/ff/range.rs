//! A file read over HTTP(S) by the TV's `curl`, for FFmpeg's custom I/O: a ranged GET at the place read, kept open
//! while the reads follow one another, and started again at the new place on a seek.
use crate::net;
use std::io::Read;
use std::process::{Child, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

/// A gap this small is read through and dropped rather than costing a new request.
const SKIP_MAX: i64 = 256 * 1024;

/// What another thread needs to cut a read short: Back, a seek, leaving.
#[derive(Default)]
pub struct Stop {
    pub flag: AtomicBool,
    pid: AtomicI32,
}

impl Stop {
    /// Cuts the request in progress without ending the stream: the reader fails once and the next read starts again.
    pub fn cut(&self) {
        let pid = self.pid.load(Ordering::SeqCst);
        if pid > 0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    /// Ends the read in progress (the `curl` is killed, so a blocked read returns) and every later one.
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        let pid = self.pid.load(Ordering::SeqCst);
        if pid > 0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

pub struct Range {
    url: String,
    pub size: i64,
    pos: i64,
    cur: Option<(Child, ChildStdout, i64)>,
    stop: Arc<Stop>,
    pub fetched: u64,
}

impl Range {
    pub fn open(url: &str, stop: Arc<Stop>) -> Result<Range, String> {
        let size = net::file_size(url)?;
        Ok(Range {
            url: url.to_string(),
            size,
            pos: 0,
            cur: None,
            stop,
            fetched: 0,
        })
    }

    pub fn position(&self) -> i64 {
        self.pos
    }

    fn close(&mut self) {
        if let Some((mut c, out, _)) = self.cur.take() {
            drop(out);
            let _ = c.kill();
            let _ = c.wait();
        }
        self.stop.pid.store(0, Ordering::SeqCst);
    }

    fn start(&mut self) -> std::io::Result<()> {
        self.close();
        let mut c = net::curl_from(&self.url, self.pos)?;
        let out = c.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
        self.stop.pid.store(c.id() as i32, Ordering::SeqCst);
        // Stop may have come between the check and the store above.
        if self.stop.flag.load(Ordering::SeqCst) {
            unsafe { libc::kill(c.id() as i32, libc::SIGKILL) };
        }
        self.cur = Some((c, out, self.pos));
        Ok(())
    }

    /// Reads at the current place. Ok(0) is the end of the file, Err the end of the stream (stopped or a network
    /// failure).
    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.stop.flag.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("stopped"));
        }
        if self.pos >= self.size {
            return Ok(0);
        }
        if self.cur.is_none() {
            self.start()?;
        }
        let (_, out, at) = self.cur.as_mut().unwrap();
        let n = out.read(buf)?;
        if n == 0 {
            // curl ended before the file did: a dropped connection, or a stop.
            self.close();
            return Err(std::io::Error::other(if self.stop.flag.load(Ordering::SeqCst) {
                "stopped"
            } else {
                "the connection ended early"
            }));
        }
        *at += n as i64;
        self.pos += n as i64;
        self.fetched += n as u64;
        Ok(n)
    }

    /// Moves the place read. A small step forward is read through; anything else starts a new request when read.
    pub fn seek(&mut self, to: i64) {
        let to = to.clamp(0, self.size);
        if to == self.pos {
            return;
        }
        let ahead = to - self.pos;
        if self.cur.is_some() && (1..=SKIP_MAX).contains(&ahead) {
            let mut left = ahead as usize;
            let mut sink = [0u8; 16 * 1024];
            while left > 0 {
                match self.read(&mut sink[..left.min(16 * 1024)]) {
                    Ok(n) if n > 0 => left -= n,
                    _ => break,
                }
            }
            if self.pos == to {
                return;
            }
        }
        self.close();
        self.pos = to;
    }
}

impl Drop for Range {
    fn drop(&mut self) {
        self.close();
    }
}
