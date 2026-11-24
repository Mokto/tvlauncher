//! HTTP and HTTPS requests. Plain HTTP goes through minreq, which has no TLS: this binary carries no TLS
//! library, so HTTPS goes through the TV's own `curl` (with the certificate store of the TV).
use std::io::Write;
use std::process::{Command, Stdio};

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

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
        "url = {}\nsilent\nshow-error\nlocation\nconnect-timeout = {}\nmax-time = {timeout}\nuser-agent = {}\nwrite-out = \"\\n%{{http_code}}\"\n",
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
    Ok(Reply {
        status,
        body: out.stdout[..n.saturating_sub(4)].to_vec(),
    })
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
        body: r.into_bytes(),
    })
}

#[cfg(test)]
mod tests {
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
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /p "), "{req}");
        assert!(req.contains("X-T: 1"));
    }
}
