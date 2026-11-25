//! The pictures shown above the bar while seeking: a thread fetches and decodes what the server made (Jellyfin's
//! trickplay sheets, Plex's BIF index) and hands over one small surface for the time asked, so the player never waits
//! for a download.
use crate::film::extras::{self, Trick};
use crate::net;
use crate::sdl::*;
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Thumbs {
    tx: Option<Sender<i64>>,
    rx: Receiver<(i64, Surf)>,
    asked: Option<i64>,
    tex: Option<Tex>,
}

impl Thumbs {
    /// None of it happens for a title without thumbnails: `at` then always gives nothing.
    pub fn new(trick: Option<Trick>) -> Thumbs {
        let (to_worker, from_main) = channel();
        let (to_main, rx) = channel();
        let tx = trick.map(|t| {
            std::thread::spawn(move || worker(t, from_main, to_main));
            to_worker
        });
        Thumbs {
            tx,
            rx,
            asked: None,
            tex: None,
        }
    }

    /// The picture for `ms` into the film, or the one before it while the next is on its way.
    pub fn at(&mut self, g: &crate::gfx::Gfx, ms: i64) -> Option<&Tex> {
        let tx = self.tx.as_ref()?;
        if self.asked != Some(ms) {
            self.asked = Some(ms);
            let _ = tx.send(ms);
        }
        while let Ok((_, s)) = self.rx.try_recv() {
            if let Some(t) = Tex::new(g.ren, &s) {
                self.tex = Some(t);
            }
        }
        self.tex.as_ref()
    }
}

/// Which sheet, and which column and row on it, holds thumbnail `idx`.
pub fn cell(idx: i64, tile: (i64, i64)) -> (i64, i64, i64) {
    let per = (tile.0 * tile.1).max(1);
    let i = idx % per;
    (idx / per, i % tile.0.max(1), i / tile.0.max(1))
}

/// A new ARGB surface holding `r` of `src` (SDL converts the format).
fn crop(src: &Surf, r: Rect) -> Option<Surf> {
    let (w, h) = src.size();
    let r = Rect {
        x: r.x.clamp(0, w.max(1) - 1),
        y: r.y.clamp(0, h.max(1) - 1),
        w: r.w.min(w - r.x.clamp(0, w)).max(1),
        h: r.h.min(h - r.y.clamp(0, h)).max(1),
    };
    let dst = Surf(unsafe { SDL_CreateRGBSurfaceWithFormat(0, r.w, r.h, 32, PIXELFORMAT_ARGB8888) });
    if dst.0.is_null() {
        return None;
    }
    let ok = unsafe { SDL_UpperBlitScaled(src.0, &r, dst.0, std::ptr::null_mut()) };
    (ok == 0).then_some(dst)
}

/// A JPEG from memory as a surface (SDL_image reads files).
fn decode(jpeg: &[u8]) -> Option<Surf> {
    let path = std::env::temp_dir().join(format!("tvlauncher-thumb-{}.jpg", std::process::id()));
    std::fs::write(&path, jpeg).ok()?;
    let s = Surf::load(&path.to_string_lossy());
    let _ = std::fs::remove_file(&path);
    s
}

fn worker(trick: Trick, rx: Receiver<i64>, tx: Sender<(i64, Surf)>) {
    let mut src: Box<dyn Source> = match trick {
        Trick::Sheets {
            cfg,
            id,
            source,
            width,
            tile,
            size,
            count,
            interval_ms,
        } => Box::new(Sheets {
            fetch: Box::new(move |n| {
                cfg.get_within(
                    &format!("/Videos/{id}/Trickplay/{width}/{n}.jpg?MediaSourceId={source}"),
                    15,
                )
            }),
            tile,
            size,
            count,
            interval_ms,
            sheet: None,
        }),
        Trick::Bif { conn, path } => Box::new(Bif {
            url: conn.url(&path),
            index: None,
        }),
    };
    let mut failed = 0;
    while let Ok(mut ms) = rx.recv() {
        while let Ok(m) = rx.try_recv() {
            ms = m;
        }
        match src.get(ms) {
            Ok(s) => {
                failed = 0;
                crate::sdl::log(&format!("film: thumbnail for {ms} ms"));
                if tx.send((ms, s)).is_err() {
                    return;
                }
            }
            Err(e) => {
                failed += 1;
                crate::sdl::log(&format!("film: thumbnail: {e}"));
                if failed >= 3 {
                    return;
                }
            }
        }
    }
}

trait Source {
    fn get(&mut self, ms: i64) -> Result<Surf, String>;
}

struct Sheets {
    fetch: Box<dyn Fn(i64) -> Result<Vec<u8>, String> + Send>,
    tile: (i64, i64),
    size: (i64, i64),
    count: i64,
    interval_ms: i64,
    /// The last sheet decoded: a sheet covers minutes, so seeking around reuses it.
    sheet: Option<(i64, Surf)>,
}

impl Source for Sheets {
    fn get(&mut self, ms: i64) -> Result<Surf, String> {
        let idx = (ms / self.interval_ms.max(1)).clamp(0, (self.count - 1).max(0));
        let (n, col, row) = cell(idx, self.tile);
        if self.sheet.as_ref().is_none_or(|s| s.0 != n) {
            let bytes = (self.fetch)(n)?;
            self.sheet = Some((n, decode(&bytes).ok_or("a sheet would not decode")?));
        }
        let sheet = &self.sheet.as_ref().ok_or("no sheet")?.1;
        let r = Rect {
            x: (col * self.size.0) as i32,
            y: (row * self.size.1) as i32,
            w: self.size.0 as i32,
            h: self.size.1 as i32,
        };
        crop(sheet, r).ok_or_else(|| "a thumbnail would not crop".to_string())
    }
}

struct Bif {
    url: String,
    index: Option<(Vec<(u32, u32)>, u32)>,
}

/// Bytes `from..=to` of `url`; a server that ignores the range and sends the whole file is read only when small.
fn range(url: &str, from: usize, to: usize) -> Result<Vec<u8>, String> {
    let r = net::send(url, None, &[("Range", format!("bytes={from}-{to}"))], 20)?;
    match r.status {
        206 => Ok(r.body),
        200 if r.body.len() <= 8 << 20 => Ok(r.body.get(from..=to.min(r.body.len() - 1)).unwrap_or_default().to_vec()),
        200 => Err("the server sent the whole index".into()),
        s => Err(format!("HTTP {s}")),
    }
}

impl Source for Bif {
    fn get(&mut self, ms: i64) -> Result<Surf, String> {
        if self.index.is_none() {
            let head = range(&self.url, 0, extras::BIF_HEAD - 1)?;
            let (n, unit) = extras::bif_header(&head).ok_or("not a BIF index")?;
            let table = range(&self.url, extras::BIF_HEAD, extras::BIF_HEAD + (n + 1) * 8 - 1)?;
            self.index = Some((extras::bif_index(&table, n).ok_or("a broken BIF index")?, unit));
        }
        let (index, unit) = self.index.as_ref().ok_or("no index")?;
        let i = extras::bif_image_at(index, *unit, ms);
        let (from, to) = (index[i].1 as usize, index[i + 1].1 as usize);
        if to <= from {
            return Err("an empty image".into());
        }
        let jpeg = range(&self.url, from, to - 1)?;
        let s = decode(&jpeg).ok_or("an image would not decode")?;
        let (w, h) = s.size();
        crop(&s, Rect { x: 0, y: 0, w, h }).ok_or_else(|| "an image would not convert".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A BIF of `n` copies of a 32 x 18 JPEG, one per 2 s, served with byte ranges.
    fn bif_server(n: usize) -> String {
        let jpg = include_bytes!("test-thumb.jpg");
        let mut body = crate::film::extras::BIF_MAGIC_FOR_TESTS.to_vec();
        body.extend(0u32.to_le_bytes());
        body.extend((n as u32).to_le_bytes());
        body.extend(2000u32.to_le_bytes());
        body.resize(crate::film::extras::BIF_HEAD, 0);
        let first = crate::film::extras::BIF_HEAD + (n + 1) * 8;
        for i in 0..=n {
            body.extend((if i < n { i as u32 } else { u32::MAX }).to_le_bytes());
            body.extend(((first + i * jpg.len()) as u32).to_le_bytes());
        }
        for _ in 0..n {
            body.extend(jpg);
        }
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/bif", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let mut c = c.unwrap();
                let mut buf = [0u8; 2048];
                let k = c.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..k]).to_lowercase();
                let r = req
                    .split("range: bytes=")
                    .nth(1)
                    .and_then(|r| r.split("\r\n").next())
                    .unwrap_or("0-0");
                let (a, b) = r.split_once('-').unwrap();
                let (a, b) = (
                    a.parse::<usize>().unwrap(),
                    b.parse::<usize>().unwrap().min(body.len() - 1),
                );
                let _ = write!(
                    c,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    b - a + 1
                );
                let _ = c.write_all(&body[a..=b]);
            }
        });
        url
    }

    #[test]
    fn a_bif_gives_the_picture_for_a_time() {
        let mut bif = Bif {
            url: bif_server(3),
            index: None,
        };
        for ms in [0, 2_500, 99_000] {
            let s = bif.get(ms).unwrap();
            assert_eq!(s.size(), (32, 18), "at {ms}");
        }
        assert_eq!(bif.index.as_ref().map(|i| i.0.len()), Some(4));
    }

    #[test]
    fn a_thumbnail_is_found_on_its_sheet() {
        assert_eq!(super::cell(0, (10, 10)), (0, 0, 0));
        assert_eq!(super::cell(23, (10, 10)), (0, 3, 2));
        assert_eq!(super::cell(100, (10, 10)), (1, 0, 0));
        assert_eq!(super::cell(257, (10, 10)), (2, 7, 5));
    }
}
