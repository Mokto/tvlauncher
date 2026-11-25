//! Jellyfin films and episodes for the launcher's own player (`player::film`): what to play, the address of the
//! server's remuxed HLS stream (the server only re-packs H.264/H.265 into MPEG-TS segments, which the player
//! already reads), the progress reports, and the playhead clock.
use crate::jellyfin::Config;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TICKS: i64 = 10_000_000;
/// The exit status of the player process that tells Home to open the title in the Jellyfin app instead.
pub const ELSEWHERE: u8 = 3;
const DEVICE: &str = "tvlauncher-c1";

/// An audio or a subtitle track, as the server names it (for example "French - AC3 5.1 - Default").
#[derive(Clone)]
pub struct Track {
    pub index: i64,
    pub name: String,
}

/// What is played: a film or one episode.
pub struct Item {
    pub audio: Vec<Track>,
    /// Text subtitles only: the server can hand those over as SRT; picture subtitles (PGS, VobSub) it would have
    /// to burn into the video.
    pub subs: Vec<Track>,
    pub id: String,
    pub source: String,
    pub name: String,
    pub sub: String,
    /// Length and the place to resume from, in 100 ns ticks.
    pub total: i64,
    pub resume: i64,
    pub session: String,
}

pub enum Why {
    /// A collection, a library: only the Jellyfin app shows those.
    Elsewhere,
    Failed(String),
}

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
    item_from(&o).ok_or_else(|| Why::Failed("The server sent no video for this title.".into()))
}

pub fn item_from(o: &Value) -> Option<Item> {
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tracks = |kind: &str, text_only: bool| -> Vec<Track> {
        o["MediaSources"][0]["MediaStreams"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["Type"] == kind && (!text_only || s["IsTextSubtitleStream"] == true))
            .filter_map(|s| {
                Some(Track {
                    index: s["Index"].as_i64()?,
                    name: s["DisplayTitle"].as_str().or(s["Language"].as_str())?.to_string(),
                })
            })
            .collect()
    };
    Some(Item {
        audio: tracks("Audio", false),
        subs: tracks("Subtitle", true),
        source: o["MediaSources"][0]["Id"].as_str().unwrap_or(&id).to_string(),
        name: o["Name"].as_str().unwrap_or("Video").to_string(),
        sub,
        // Resume only between 30 s in and 95 %: what is nearly finished starts again.
        resume: if at > 30 * TICKS && (total == 0 || at < total / 100 * 95) {
            at
        } else {
            0
        },
        total,
        session: format!("{nanos:x}"),
        id,
    })
}

/// The playlist of the server's remux of the film, from `start` ticks in. The server copies H.264 and H.265 and
/// converts audio the TV cannot decode (DTS, TrueHD) to AC-3.
pub fn master_url(cfg: &Config, it: &Item, start: i64, audio: Option<i64>) -> String {
    let audio = audio.map_or(String::new(), |i| format!("&AudioStreamIndex={i}"));
    format!(
        "{}/Videos/{}/master.m3u8?MediaSourceId={}&PlaySessionId={}&DeviceId={DEVICE}&VideoCodec=h264,hevc\
         &AudioCodec=aac,ac3,eac3&TranscodingMaxAudioChannels=6&SegmentContainer=ts&MinSegments=1\
         &BreakOnNonKeyFrames=true&AllowVideoStreamCopy=true&AllowAudioStreamCopy=true&StartTimeTicks={start}\
         &api_key={}{audio}",
        cfg.server, it.id, it.source, it.session, cfg.token
    )
}

/// A subtitle track as SRT, from the server.
pub fn subtitles(cfg: &Config, id: &str, source: &str, index: i64) -> Result<Vec<crate::subs::Cue>, String> {
    let url = format!("{}/Videos/{id}/{source}/Subtitles/{index}/0/Stream.srt", cfg.server);
    let auth = format!(
        "MediaBrowser Token=\"{}\", Client=\"tvlauncher\", Device=\"LG C1\", DeviceId=\"{DEVICE}\", Version=\"0.1\"",
        cfg.token
    );
    let r = crate::net::send(&url, None, &[("Authorization", auth)], 20)?;
    if r.status != 200 {
        return Err(format!("HTTP {}", r.status));
    }
    Ok(crate::subs::parse_srt(&String::from_utf8_lossy(&r.body)))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Event {
    Start,
    Progress,
    Stop,
}

/// Tells the server where the viewer is (the resume point, the watched mark and the dashboard's "now playing").
/// The last one is waited for so the process does not exit first; the others go out on a thread.
pub fn report(cfg: &Config, it: &Item, event: Event, ticks: i64, paused: bool) {
    let (path, wait) = match event {
        Event::Start => ("/Sessions/Playing", false),
        Event::Progress => ("/Sessions/Playing/Progress", false),
        Event::Stop => ("/Sessions/Playing/Stopped", true),
    };
    let body = body(it, ticks, paused).to_string();
    let (url, token) = (format!("{}{path}", cfg.server), cfg.token.clone());
    let send = move || {
        let auth = format!("MediaBrowser Token=\"{token}\", Client=\"tvlauncher\", Device=\"LG C1\", DeviceId=\"{DEVICE}\", Version=\"0.1\"");
        if let Err(e) = crate::net::send(&url, Some(&body), &[("Authorization", auth)], 6) {
            crate::sdl::log(&format!("film: report: {e}"));
        }
    };
    if wait {
        send();
    } else {
        std::thread::spawn(send);
    }
}

fn body(it: &Item, ticks: i64, paused: bool) -> Value {
    json!({
        "ItemId": it.id,
        "MediaSourceId": it.source,
        "PlaySessionId": it.session,
        "PositionTicks": ticks.max(0),
        "IsPaused": paused,
        "CanSeek": true,
        "PlayMethod": "Transcode",
    })
}

/// The playhead: how far the pipeline has been let run, which stops while paused.
#[derive(Default)]
pub struct Clock {
    before: Duration,
    since: Option<Instant>,
}

impl Clock {
    pub fn run(&mut self) {
        self.since.get_or_insert_with(Instant::now);
    }

    pub fn hold(&mut self) {
        if let Some(t) = self.since.take() {
            self.before += t.elapsed();
        }
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    pub fn now(&self) -> Duration {
        self.before + self.since.map_or(Duration::ZERO, |t| t.elapsed())
    }
}

/// `1:05:09` or `5:09`.
pub fn clock(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
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
        let it = item_from(&o(1200 * TICKS)).unwrap();
        assert_eq!(
            (it.resume, it.source.as_str(), it.sub.as_str()),
            (1200 * TICKS, "src9", "1999")
        );
        assert_eq!(item_from(&o(10 * TICKS)).unwrap().resume, 0, "too early to resume");
        assert_eq!(
            item_from(&o(5900 * TICKS)).unwrap().resume,
            0,
            "nearly finished starts again"
        );
    }

    #[test]
    fn tracks_keep_text_subtitles_only() {
        let it = item_from(&json!({ "Id": "m1", "MediaSources": [{ "Id": "s", "MediaStreams": [
            { "Type": "Video", "Index": 0 },
            { "Type": "Audio", "Index": 1, "DisplayTitle": "French - AC3 5.1" },
            { "Type": "Audio", "Index": 2, "Language": "eng" },
            { "Type": "Subtitle", "Index": 3, "DisplayTitle": "French - SRT", "IsTextSubtitleStream": true },
            { "Type": "Subtitle", "Index": 4, "DisplayTitle": "English - PGS", "IsTextSubtitleStream": false },
        ] }] }))
        .unwrap();
        let names = |t: &[Track]| t.iter().map(|t| (t.index, t.name.clone())).collect::<Vec<_>>();
        assert_eq!(names(&it.audio), [(1, "French - AC3 5.1".into()), (2, "eng".into())]);
        assert_eq!(names(&it.subs), [(3, "French - SRT".into())]);
    }

    #[test]
    fn episodes_say_season_and_number() {
        let it = item_from(
            &json!({ "Id": "e1", "Name": "Pilot", "Type": "Episode", "SeriesName": "The Show",
                                    "ParentIndexNumber": 2, "IndexNumber": 5 }),
        )
        .unwrap();
        assert_eq!(it.sub, "The Show   S2 E5");
        assert_eq!(it.source, "e1", "without media sources the item is its own source");
    }

    #[test]
    fn master_url_asks_for_a_remux_from_the_start_point() {
        let it = item_from(&json!({ "Id": "m1", "MediaSources": [{ "Id": "src9" }] })).unwrap();
        let u = master_url(&cfg(), &it, 42 * TICKS, Some(3));
        assert!(u.starts_with("https://jf.example/Videos/m1/master.m3u8?MediaSourceId=src9&PlaySessionId="));
        for part in [
            "VideoCodec=h264,hevc",
            "SegmentContainer=ts",
            "AllowVideoStreamCopy=true",
            "StartTimeTicks=420000000",
            "api_key=tok",
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
        let it = item_from(&json!({ "Id": "m1" })).unwrap();
        let b = body(&it, -5, true);
        assert_eq!(
            (
                b["PositionTicks"].as_i64(),
                b["IsPaused"].as_bool(),
                b["ItemId"].as_str()
            ),
            (Some(0), Some(true), Some("m1"))
        );
    }

    #[test]
    fn the_clock_stops_while_held() {
        let mut c = Clock::default();
        assert_eq!(c.now(), Duration::ZERO);
        c.run();
        std::thread::sleep(Duration::from_millis(30));
        c.hold();
        let held = c.now();
        assert!(held >= Duration::from_millis(30) && !c.running());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(c.now(), held);
        c.run();
        assert!(c.running());
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

    #[test]
    fn times_read_as_clocks() {
        assert_eq!(
            (clock(309).as_str(), clock(3909).as_str(), clock(-4).as_str()),
            ("5:09", "1:05:09", "0:00")
        );
    }
}
