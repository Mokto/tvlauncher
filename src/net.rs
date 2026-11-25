//! HTTP and HTTPS requests. Plain HTTP goes through minreq, which has no TLS: this binary carries no TLS
//! library, so HTTPS goes through the TV's own `curl` (with the certificate store of the TV).
use crate::sdl::Event;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// The address that answered, after redirects.
    pub url: String,
}

/// Starts a picture thread: send it the pictures a page shows, as (address, path under `root`) pairs; only the
/// latest list is worked on. Each picture saved comes back as its path, followed by `wake`. A picture the server
/// refuses is saved empty so it is not asked for again; a server that does not answer is skipped until the
/// next list.
pub fn pictures(root: String, wake: u32) -> (Sender<Vec<(String, String)>>, Receiver<String>) {
    let (req_tx, req_rx) = channel::<Vec<(String, String)>>();
    let (res_tx, res_rx) = channel();
    std::thread::spawn(move || {
        let (mut todo, mut dead) = (Vec::<(String, String)>::new(), Vec::<String>::new());
        loop {
            let next = if todo.is_empty() {
                match req_rx.recv() {
                    Ok(l) => Some(l),
                    Err(_) => return,
                }
            } else {
                req_rx.try_recv().ok()
            };
            if let Some(list) = next {
                todo = list;
                todo.reverse();
                dead.clear();
                continue;
            }
            let Some((url, path)) = todo.pop() else { continue };
            let full = format!("{root}{path}");
            let host = url.split('/').nth(2).unwrap_or_default().to_string();
            if std::path::Path::new(&full).exists() || dead.contains(&host) {
                continue;
            }
            match send(&url, None, &[], 10) {
                Ok(r) if r.status < 400 && !r.body.is_empty() => {
                    if let Some(dir) = std::path::Path::new(&full).parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if std::fs::write(&full, &r.body).is_ok() {
                        if res_tx.send(path).is_err() {
                            return;
                        }
                        unsafe { crate::sdl::SDL_PushEvent(&mut Event::new(wake)) };
                    }
                }
                Ok(_) => drop(std::fs::write(&full, b"")),
                Err(_) => dead.push(host),
            }
        }
    });
    (req_tx, res_rx)
}

/// Live streams are fetched as a browser: some channels refuse other agents (and some a bare `Mozilla/5.0`).
pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// A curl config value: quoted, with the escapes curl reads.
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
    )
}

/// The config curl reads from stdin, so tokens never show in the process list. The status code is written
/// after the body, on a line of its own.
fn curl_config(url: &str, ua: &str, headers: &[(&str, String)], body: Option<&str>, timeout: u64) -> String {
    let mut c = format!(
        "url = {}\nsilent\nshow-error\nlocation\nconnect-timeout = {}\nmax-time = {timeout}\nuser-agent = {}\nwrite-out = \"\\n%{{url_effective}}\\n%{{http_code}}\"\n",
        quote(url),
        timeout.min(10),
        quote(ua)
    );
    for (k, v) in headers {
        c.push_str(&format!("header = {}\n", quote(&format!("{k}: {v}"))));
    }
    if let Some(b) = body {
        c.push_str(&format!("data-binary = {}\n", quote(b)));
    }
    c
}

fn curl_bin() -> &'static str {
    ["/usr/bin/curl", "/bin/curl", "/usr/local/bin/curl"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or("curl")
}

/// The answer whatever its status (the OAuth endpoints answer 4xx with a JSON reason), or what curl said
/// when there was no answer at all.
pub fn curl(
    url: &str,
    ua: &str,
    headers: &[(&str, String)],
    body: Option<&str>,
    timeout: u64,
) -> Result<Reply, String> {
    let mut child = Command::new(curl_bin())
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(curl_config(url, ua, headers, body, timeout).as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(why.lines().next().unwrap_or("curl failed").to_string());
    }
    let n = out.stdout.len();
    let status = n
        .checked_sub(3)
        .and_then(|i| std::str::from_utf8(&out.stdout[i..]).ok())
        .and_then(|s| s.parse().ok())
        .ok_or("curl gave no status")?;
    let rest = &out.stdout[..n - 4];
    let cut = rest.iter().rposition(|&b| b == b'\n').ok_or("curl gave no address")?;
    Ok(Reply {
        status,
        url: String::from_utf8_lossy(&rest[cut + 1..]).into_owned(),
        body: rest[..cut].to_vec(),
    })
}

/// A `curl` writing the body of `url` to its stdout for as long as the server sends it (a live stream).
pub fn curl_stream(url: &str) -> std::io::Result<std::process::Child> {
    let mut child = Command::new(curl_bin())
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let c = format!(
            "url = {}\nsilent\nshow-error\nfail\nlocation\nconnect-timeout = 10\nuser-agent = {}\n",
            quote(url),
            quote(BROWSER_UA)
        );
        stdin.write_all(c.as_bytes())?;
    }
    Ok(child)
}

/// A `curl` writing the bytes of `url` from `from` on to its stdout (a ranged GET, for a film file that is read
/// at the place the player asks for).
pub fn curl_from(url: &str, from: i64) -> std::io::Result<std::process::Child> {
    let mut child = Command::new(curl_bin())
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let c = format!(
            "url = {}\nsilent\nfail\nlocation\nconnect-timeout = 10\nuser-agent = {}\nrange = \"{from}-\"\n",
            quote(url),
            quote(BROWSER_UA)
        );
        stdin.write_all(c.as_bytes())?;
    }
    Ok(child)
}

/// The size of the file at `url` in bytes, from the `Content-Range` of a one-byte ranged GET. Err when the server
/// does not do ranges: such a file cannot be played from the middle.
pub fn file_size(url: &str) -> Result<i64, String> {
    let mut child = Command::new(curl_bin())
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let c = format!(
            "url = {}\nsilent\nlocation\nconnect-timeout = 10\nmax-time = 20\nuser-agent = {}\nrange = \"0-0\"\noutput = \"/dev/null\"\ndump-header = \"-\"\n",
            quote(url),
            quote(BROWSER_UA)
        );
        let _ = stdin.write_all(c.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    parse_size(&String::from_utf8_lossy(&out.stdout))
}

/// The total after the slash of the last `Content-Range: bytes 0-0/<total>` header.
fn parse_size(headers: &str) -> Result<i64, String> {
    headers
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case("content-range").then_some(v)
        })
        .next_back()
        .and_then(|v| v.rsplit('/').next())
        .and_then(|n| n.trim().parse::<i64>().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| "the server does not serve byte ranges".to_string())
}

/// A GET, or a POST of a JSON body, with `headers`, that gives up after `timeout` seconds.
pub fn send(url: &str, post: Option<&str>, headers: &[(&str, String)], timeout: u64) -> Result<Reply, String> {
    if url.starts_with("https://") {
        let mut h = headers.to_vec();
        if post.is_some() {
            h.push(("Content-Type", "application/json".into()));
        }
        return curl(url, "tvlauncher", &h, post, timeout);
    }
    let req = match post {
        Some(b) => minreq::post(url)
            .with_header("Content-Type", "application/json")
            .with_body(b),
        None => minreq::get(url),
    };
    let r = headers
        .iter()
        .fold(req, |r, (k, v)| r.with_header(*k, v.clone()))
        .with_timeout(timeout)
        .send()
        .map_err(|e| e.to_string())?;
    Ok(Reply {
        status: r.status_code as u16,
        url: r.url.clone(),
        body: r.into_bytes(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn size_comes_from_the_last_content_range() {
        let h = "HTTP/1.1 302 Found\r\nLocation: /x\r\n\r\nHTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/9876543210\r\n\r\n";
        assert_eq!(super::parse_size(h), Ok(9_876_543_210));
        assert!(super::parse_size("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n").is_err());
    }

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn config_quotes_what_it_must() {
        let c = curl_config("https://x/y", "ua", &[("A", "b\"c".into())], Some("{\"k\":\"v\"}"), 7);
        assert!(c.contains("header = \"A: b\\\"c\"\n"));
        assert!(c.contains("data-binary = \"{\\\"k\\\":\\\"v\\\"}\"\n"));
        assert!(c.contains("max-time = 7\n"));
    }

    /// Runs the real `curl` against a local server: the status comes after the body, which may hold newlines.
    #[test]
    fn curl_gives_status_and_body_and_posts() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 9\r\nConnection: close\r\n\r\nhi\nthere\n")
                .unwrap();
            req
        });
        let r = curl(
            &format!("http://127.0.0.1:{port}/p"),
            "ua",
            &[("X-T", "1".into())],
            Some("{}"),
            5,
        )
        .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (201, &b"hi\nthere\n"[..]));
        assert_eq!(r.url, format!("http://127.0.0.1:{port}/p"));
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /p "), "{req}");
        assert!(req.contains("X-T: 1"));
    }
}
