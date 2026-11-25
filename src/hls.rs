//! HLS playlists, for live channels whose segments are MPEG-TS.

#[derive(Debug, PartialEq)]
pub enum Playlist {
    /// The variant streams: bandwidth and address.
    Master(Vec<(u64, String)>),
    /// `seq` is the media sequence number of the first segment.
    Media {
        seq: u64,
        target_s: u64,
        segs: Vec<String>,
        /// Each segment's length in seconds, as `#EXTINF` gives it.
        durs: Vec<f64>,
        end: bool,
    },
}

pub fn is_hls(url: &str) -> bool {
    url.split(['?', '#']).next().unwrap_or("").ends_with(".m3u8")
}

/// `r` as an absolute address, relative to the playlist at `base`.
pub fn join(base: &str, r: &str) -> String {
    if r.contains("://") {
        return r.into();
    }
    let scheme = base.find("://").map_or(0, |i| i + 3);
    if let Some(rest) = r.strip_prefix("//") {
        return format!("{}{rest}", &base[..scheme]);
    }
    let path = base.split(['?', '#']).next().unwrap_or(base);
    if r.starts_with('/') {
        let host_end = path[scheme..].find('/').map_or(path.len(), |i| scheme + i);
        return format!("{}{r}", &path[..host_end]);
    }
    let dir = path.rfind('/').filter(|&i| i >= scheme).map_or(path.len(), |i| i + 1);
    format!("{}{r}", &path[..dir])
}

fn attr<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split(',').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim_matches('"'))
    })
}

/// The index of the segment that holds second `s` of the stream (the last one when `s` is past the end).
pub fn index_at(durs: &[f64], s: f64) -> usize {
    let mut t = 0.0;
    for (i, d) in durs.iter().enumerate() {
        t += d;
        if t > s {
            return i;
        }
    }
    durs.len().saturating_sub(1)
}

pub fn parse(base: &str, text: &str) -> Result<Playlist, String> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("The channel's address is not an HLS playlist.".into());
    }
    let (mut variants, mut segs, mut durs) = (Vec::new(), Vec::new(), Vec::new());
    let mut dur = 0.0;
    let (mut seq, mut target_s, mut end, mut inf) = (0, 2, false, None::<u64>);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(a) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            inf = Some(attr(a, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0));
        } else if let Some(n) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = n.parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_s = n.parse().unwrap_or(2);
        } else if let Some(n) = line.strip_prefix("#EXTINF:") {
            dur = n.split(',').next().and_then(|d| d.trim().parse().ok()).unwrap_or(0.0);
        } else if line == "#EXT-X-ENDLIST" {
            end = true;
        } else if let Some(a) = line.strip_prefix("#EXT-X-KEY:") {
            if attr(a, "METHOD") != Some("NONE") {
                return Err("The channel is encrypted, which the player does not read.".into());
            }
        } else if line.starts_with("#EXT-X-MAP:") {
            return Err("The channel sends MP4 segments; the player reads MPEG-TS only.".into());
        } else if !line.starts_with('#') {
            match inf.take() {
                Some(bw) => variants.push((bw, join(base, line))),
                None => {
                    segs.push(join(base, line));
                    durs.push(std::mem::take(&mut dur));
                }
            }
        }
    }
    Ok(if variants.is_empty() {
        Playlist::Media {
            seq,
            target_s,
            segs,
            durs,
            end,
        }
    } else {
        Playlist::Master(variants)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_and_parses() {
        let b = "https://h.example/a/b/live.m3u8?x=1";
        assert_eq!(join(b, "s1.ts"), "https://h.example/a/b/s1.ts");
        assert_eq!(join(b, "/c/s1.ts"), "https://h.example/c/s1.ts");
        assert_eq!(join(b, "//o.example/s.ts"), "https://o.example/s.ts");
        assert_eq!(join(b, "http://o.example/s.ts"), "http://o.example/s.ts");
        assert!(is_hls(b) && !is_hls("http://h/live/1.ts"));

        let master = "#EXTM3U\r\n#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=800,CODECS=\"avc1.64001f,mp4a.40.2\"\r\nlo.m3u8\r\n#EXT-X-STREAM-INF:BANDWIDTH=5000,RESOLUTION=1280x720\r\nhi.m3u8?v=1\r\n";
        assert_eq!(
            parse(b, master),
            Ok(Playlist::Master(vec![
                (800, "https://h.example/a/b/lo.m3u8".into()),
                (5000, "https://h.example/a/b/hi.m3u8?v=1".into())
            ]))
        );
        let media =
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:41\n#EXTINF:6,\nx41.ts\n#EXTINF:6,\nx42.ts\n";
        assert_eq!(
            parse(b, media),
            Ok(Playlist::Media {
                seq: 41,
                target_s: 6,
                segs: vec![
                    "https://h.example/a/b/x41.ts".into(),
                    "https://h.example/a/b/x42.ts".into()
                ],
                durs: vec![6.0, 6.0],
                end: false
            })
        );
        let d = [7.0, 8.0, 4.0];
        assert_eq!(
            (
                index_at(&d, 0.0),
                index_at(&d, 6.9),
                index_at(&d, 7.0),
                index_at(&d, 15.0)
            ),
            (0, 0, 1, 2)
        );
        assert_eq!(index_at(&d, 99.0), 2);
        assert_eq!(index_at(&[], 5.0), 0);
        assert!(parse(b, "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\nx.ts\n").is_err());
        assert!(parse(b, "<!DOCTYPE html>").is_err());
    }
}
