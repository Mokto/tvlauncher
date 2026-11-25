//! Jellyfin: what to play, the address of the file and of the server's remuxed HLS stream (the fallback, where the
//! server re-packs H.264/H.265 into MPEG-TS segments), the subtitles and the progress reports.
use super::*;
use crate::jellyfin::Config;
use serde_json::{json, Value};

const DEVICE: &str = "tvlauncher-c1";

fn json_of(cfg: &Config, path: &str) -> Result<Value, Why> {
    let body = cfg.get_within(path, 15).map_err(Why::Failed)?;
    serde_json::from_slice(&body).map_err(|e| Why::Failed(e.to_string()))
}

/// The item to play for `id`: itself for a film or an episode, the next episode to watch for a series or season.
pub fn resolve(cfg: &Config, id: &str) -> Result<Item, Why> {
    let user = &cfg.user_id;
    let o = json_of(cfg, &format!("/Users/{user}/Items/{id}"))?;
    let target = match o["Type"].as_str().unwrap_or_default() {
        "Movie" | "Episode" | "Video" | "MusicVideo" => id.to_string(),
        "Series" | "Season" => {
            let series = o["SeriesId"].as_str().unwrap_or(id);
            let first = |v: &Value| v["Items"][0]["Id"].as_str().map(String::from);
            let next = json_of(cfg, &format!("/Shows/NextUp?userId={user}&seriesId={series}&limit=1"))
                .ok()
                .as_ref()
                .and_then(first);
            let season = if o["Type"] == "Season" {
                format!("&seasonId={id}")
            } else {
                String::new()
            };
            match next {
                Some(n) if o["Type"] == "Series" => n,
                _ => first(&json_of(
                    cfg,
                    &format!("/Shows/{series}/Episodes?userId={user}&limit=1{season}"),
                )?)
                .ok_or_else(|| Why::Failed("There is no episode to play.".into()))?,
            }
        }
        _ => return Err(Why::Elsewhere),
    };
    let o = if target == id {
        o
    } else {
        json_of(cfg, &format!("/Users/{user}/Items/{target}"))?
    };
    let mut it = item_from(cfg, &o).ok_or_else(|| Why::Failed("The server sent no video for this title.".into()))?;
    // Jellyfin 10.10 and later; an older server answers 404 and the film has nothing to skip.
    it.segments = cfg
        .get_within(&format!("/MediaSegments/{}", it.id), 4)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .map(|v| extras::jellyfin_segments(&v))
        .unwrap_or_default();
    Ok(it)
}

pub fn item_from(cfg: &Config, o: &Value) -> Option<Item> {
    let id = o["Id"].as_str()?.to_string();
    let total = o["RunTimeTicks"].as_i64().unwrap_or(0);
    let at = o["UserData"]["PlaybackPositionTicks"].as_i64().unwrap_or(0);
    let sub = if o["Type"] == "Episode" {
        let series = o["SeriesName"].as_str().unwrap_or_default();
        match (o["ParentIndexNumber"].as_i64(), o["IndexNumber"].as_i64()) {
            (Some(s), Some(e)) => format!("{series}   S{s} E{e}"),
            _ => series.to_string(),
        }
    } else {
        o["ProductionYear"].as_i64().map(|y| y.to_string()).unwrap_or_default()
    };
    let streams = &o["MediaSources"][0]["MediaStreams"];
    let tracks = |kind: &str, text_only: bool| -> Vec<Track> {
        streams
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["Type"] == kind && (!text_only || s["IsTextSubtitleStream"] == true))
            .filter_map(|s| {
                let mut t = Track::new(
                    s["Index"].as_i64()?,
                    s["DisplayTitle"].as_str().or(s["Language"].as_str())?,
                );
                t.external = s["IsExternal"] == true;
                t.lang = s["Language"].as_str().unwrap_or_default().into();
                t.forced = s["IsForced"] == true;
                t.sdh = s["IsHearingImpaired"] == true;
                t.title = s["Title"].as_str().unwrap_or_default().into();
                t.format = super::sub_format(s["Codec"].as_str().unwrap_or_default());
                Some(t)
            })
            .collect()
    };
    let source = o["MediaSources"][0]["Id"].as_str().unwrap_or(&id).to_string();
    let default = |k: &str| o["MediaSources"][0][k].as_i64().filter(|i| *i >= 0);
    Some(Item {
        backend: Backend::Jellyfin(cfg.clone()),
        audio: tracks("Audio", false),
        subs: tracks("Subtitle", true),
        default_audio: default("DefaultAudioStreamIndex"),
        default_sub: default("DefaultSubtitleStreamIndex"),
        chapters: extras::jellyfin_chapters(o),
        segments: vec![],
        trick: extras::jellyfin_trick(cfg, o, &id, &source),
        direct: format!(
            "{}/Videos/{id}/stream?static=true&MediaSourceId={source}&DeviceId={DEVICE}&api_key={}",
            cfg.server, cfg.token
        ),
        source,
        name: o["Name"].as_str().unwrap_or("Video").to_string(),
        sub,
        resume: resume_point(at, total),
        total,
        session: session_id(),
        series: o["SeriesId"]
            .as_str()
            .filter(|_| o["Type"] == "Episode")
            .map(String::from),
        id,
    })
}

/// The next episode to watch of `series`, if it is not `current`.
pub fn next(cfg: &Config, series: &str, current: &str) -> Option<Item> {
    let user = &cfg.user_id;
    let path = format!("/Shows/{series}/Episodes?userId={user}&startItemId={current}&limit=2");
    let v = json_of(cfg, &path).ok()?;
    let id = v["Items"][1]["Id"].as_str().filter(|id| *id != current)?;
    resolve(cfg, id).ok()
}

/// The playlist of the server's remux of the film. It takes no start time (Jellyfin 12: "StartTimeTicks is not
/// allowed"), so the reader skips to the segment. The server copies H.264 and H.265 and
/// converts audio the TV cannot decode (DTS, TrueHD) to AC-3.
pub fn master_url(cfg: &Config, it: &Item, audio: Option<i64>) -> String {
    let audio = audio.map_or(String::new(), |i| format!("&AudioStreamIndex={i}"));
    // Jellyfin sizes a conversion from VideoBitrate: without it, it converts to 416x234. 120 Mbit/s is the most its
    // own web client asks for.
    let bps = u64::from(match MAX_KBPS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => 120_000,
        k => k,
    }) * 1000;
    let max = format!(
        "&MaxStreamingBitrate={bps}&VideoBitrate={}&AudioBitrate=384000",
        bps.saturating_sub(384_000)
    );
    // `ApiKey`, not `api_key`: Jellyfin 12 answers 401 to the latter on the playlists (the file stream still takes it),
    // and passes the former on to the segment addresses in them.
    format!(
        "{}/Videos/{}/master.m3u8?MediaSourceId={}&PlaySessionId={}&DeviceId={DEVICE}&VideoCodec=h264,hevc\
         &AudioCodec=aac,ac3,eac3&TranscodingMaxAudioChannels=6&SegmentContainer=ts&MinSegments=1\
         &BreakOnNonKeyFrames=true&AllowVideoStreamCopy=true&AllowAudioStreamCopy=true\
         &h264-profile=high,main,baseline,constrainedbaseline&h264-videobitdepth=8\
         &ApiKey={}{audio}{max}",
        cfg.server, it.id, it.source, it.session, cfg.token
    )
}

/// A subtitle track as SRT, from the server.
pub fn subtitles(cfg: &Config, id: &str, source: &str, index: i64) -> Result<Vec<crate::subs::Cue>, String> {
    let url = format!("{}/Videos/{id}/{source}/Subtitles/{index}/0/Stream.srt", cfg.server);
    let r = crate::net::send(&url, None, &[("Authorization", auth(cfg))], 20)?;
    if r.status != 200 {
        return Err(format!("HTTP {}", r.status));
    }
    Ok(crate::subs::parse_srt(&String::from_utf8_lossy(&r.body)))
}

fn auth(cfg: &Config) -> String {
    format!(
        "MediaBrowser Token=\"{}\", Client=\"tvlauncher\", Device=\"LG C1\", DeviceId=\"{DEVICE}\", Version=\"0.1\"",
        cfg.token
    )
}

pub fn report(cfg: &Config, it: &Summary, event: Event, ticks: i64, paused: bool, direct: bool) -> Result<(), String> {
    let (path, secs) = match event {
        Event::Start => ("/Sessions/Playing", 6),
        Event::Progress => ("/Sessions/Playing/Progress", 6),
        Event::Stop => ("/Sessions/Playing/Stopped", 3),
    };
    let body = body(it, ticks, paused, direct).to_string();
    crate::net::send(
        &format!("{}{path}", cfg.server),
        Some(&body),
        &[("Authorization", auth(cfg))],
        secs,
    )
    .map(drop)
}

fn body(it: &Summary, ticks: i64, paused: bool, direct: bool) -> Value {
    json!({
        "ItemId": it.id,
        "MediaSourceId": it.source,
        "PlaySessionId": it.session,
        "PositionTicks": ticks.max(0),
        "IsPaused": paused,
        "CanSeek": true,
        "PlayMethod": if direct { "DirectPlay" } else { "Transcode" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            server: "https://jf.example".into(),
            token: "tok".into(),
            server_id: "s".into(),
            user_id: "u".into(),
            user: "me".into(),
        }
    }

    #[test]
    fn items_resume_only_in_the_middle() {
        let o = |at: i64| {
            json!({ "Id": "m1", "Name": "Film", "Type": "Movie", "ProductionYear": 1999, "RunTimeTicks": 6000 * TICKS,
                    "UserData": { "PlaybackPositionTicks": at }, "MediaSources": [{ "Id": "src9" }] })
        };
        let it = item_from(&cfg(), &o(1200 * TICKS)).unwrap();
        assert_eq!(
            (it.resume, it.source.as_str(), it.sub.as_str()),
            (1200 * TICKS, "src9", "1999")
        );
        assert_eq!(
            item_from(&cfg(), &o(10 * TICKS)).unwrap().resume,
            0,
            "too early to resume"
        );
        assert_eq!(
            item_from(&cfg(), &o(5900 * TICKS)).unwrap().resume,
            0,
            "nearly finished starts again"
        );
    }

    #[test]
    fn tracks_keep_text_subtitles_only() {
        let it = item_from(
            &cfg(),
            &json!({ "Id": "m1", "MediaSources": [{ "Id": "s", "MediaStreams": [
            { "Type": "Video", "Index": 0 },
            { "Type": "Audio", "Index": 1, "DisplayTitle": "French - AC3 5.1" },
            { "Type": "Audio", "Index": 2, "Language": "eng" },
            { "Type": "Subtitle", "Index": 3, "DisplayTitle": "French - SRT", "IsTextSubtitleStream": true },
            { "Type": "Subtitle", "Index": 4, "DisplayTitle": "English - PGS", "IsTextSubtitleStream": false },
        ] }] }),
        )
        .unwrap();
        let names = |t: &[Track]| t.iter().map(|t| (t.index, t.name.clone())).collect::<Vec<_>>();
        assert_eq!(names(&it.audio), [(1, "French - AC3 5.1".into()), (2, "eng".into())]);
        assert_eq!(names(&it.subs), [(3, "French - SRT".into())]);
    }

    #[test]
    fn episodes_say_season_and_number() {
        let it = item_from(
            &cfg(),
            &json!({ "Id": "e1", "Name": "Pilot", "Type": "Episode", "SeriesName": "The Show",
                                    "ParentIndexNumber": 2, "IndexNumber": 5 }),
        )
        .unwrap();
        assert_eq!(it.sub, "The Show   S2 E5");
        assert_eq!(it.source, "e1", "without media sources the item is its own source");
    }

    #[test]
    fn master_url_asks_for_a_remux() {
        let it = item_from(&cfg(), &json!({ "Id": "m1", "MediaSources": [{ "Id": "src9" }] })).unwrap();
        let u = master_url(&cfg(), &it, Some(3));
        assert!(u.starts_with("https://jf.example/Videos/m1/master.m3u8?MediaSourceId=src9&PlaySessionId="));
        for part in [
            "VideoCodec=h264,hevc",
            "SegmentContainer=ts",
            "AllowVideoStreamCopy=true",
            "&h264-videobitdepth=8&",
            "&MaxStreamingBitrate=120000000&VideoBitrate=119616000&",
            "ApiKey=tok",
            "&AudioStreamIndex=3",
        ] {
            assert!(u.contains(part), "{part}");
        }
        assert!(
            !u.contains(' ') && !u.contains('\n'),
            "the line continuations leave no gaps"
        );
    }

    #[test]
    fn reports_carry_the_position_and_never_a_negative_one() {
        let it = item_from(&cfg(), &json!({ "Id": "m1" })).unwrap();
        let b = body(&it.summary(), -5, true, false);
        assert_eq!(
            (
                b["PositionTicks"].as_i64(),
                b["IsPaused"].as_bool(),
                b["ItemId"].as_str()
            ),
            (Some(0), Some(true), Some("m1"))
        );
    }

    /// A server that answers each request with the body of the first route its path contains, and 404 otherwise.
    fn serve(routes: &'static [(&'static str, &'static str)]) -> Config {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut buf = [0; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
                let (code, body) = routes
                    .iter()
                    .find(|r| path.contains(r.0))
                    .map_or(("404 Not Found", ""), |r| ("200 OK", r.1));
                let _ = write!(
                    conn,
                    "HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Config { server, ..cfg() }
    }

    #[test]
    fn a_series_plays_its_next_episode_else_its_first() {
        let with_next = serve(&[
            ("/NextUp", r#"{"Items":[{"Id":"e7"}]}"#),
            (
                "/Items/e7",
                r#"{"Id":"e7","Name":"Seven","Type":"Episode","SeriesName":"Show","ParentIndexNumber":1,"IndexNumber":7}"#,
            ),
            ("/Items/s1", r#"{"Id":"s1","Type":"Series"}"#),
        ]);
        let it = resolve(&with_next, "s1").ok().unwrap();
        assert_eq!((it.id.as_str(), it.sub.as_str()), ("e7", "Show   S1 E7"));

        let finished = serve(&[
            ("/NextUp", r#"{"Items":[]}"#),
            ("/Episodes", r#"{"Items":[{"Id":"e1"}]}"#),
            ("/Items/e1", r#"{"Id":"e1","Name":"One","Type":"Episode"}"#),
            ("/Items/s1", r#"{"Id":"s1","Type":"Series"}"#),
        ]);
        assert_eq!(resolve(&finished, "s1").ok().unwrap().id, "e1");
    }

    #[test]
    fn the_next_episode_is_the_one_after_the_current() {
        let cfg = serve(&[
            ("/Episodes", r#"{"Items":[{"Id":"e7"},{"Id":"e8"}]}"#),
            (
                "/Items/e8",
                r#"{"Id":"e8","Name":"Eight","Type":"Episode","SeriesId":"s1"}"#,
            ),
        ]);
        assert_eq!(next(&cfg, "s1", "e7").map(|i| i.id), Some("e8".into()));
        let last = serve(&[("/Episodes", r#"{"Items":[{"Id":"e9"}]}"#)]);
        assert!(next(&last, "s1", "e9").is_none(), "the last episode has none");
    }

    #[test]
    fn films_play_and_collections_open_elsewhere() {
        let cfg = serve(&[
            (
                "/Items/m1",
                r#"{"Id":"m1","Name":"Film","Type":"Movie","RunTimeTicks":600000000}"#,
            ),
            ("/Items/c1", r#"{"Id":"c1","Type":"BoxSet"}"#),
        ]);
        assert_eq!(resolve(&cfg, "m1").ok().unwrap().total, 60 * TICKS);
        assert!(matches!(resolve(&cfg, "c1"), Err(Why::Elsewhere)));
        assert!(matches!(resolve(&cfg, "gone"), Err(Why::Failed(_))));
    }
}
