//! Plex: what to play, the address of the file and of the server's HLS transcode (the fallback), the sidecar
//! subtitles and the timeline reports.
use super::*;
pub use crate::plex::Conn;
use serde_json::Value;

fn meta(conn: &Conn, key: &str) -> Result<Value, Why> {
    let v = conn
        .get_json(
            &format!("/library/metadata/{key}?includeChapters=1&includeMarkers=1"),
            15,
        )
        .map_err(Why::Failed)?;
    Ok(v["MediaContainer"]["Metadata"][0].clone())
}

fn episodes(conn: &Conn, key: &str, what: &str) -> Result<Vec<Value>, Why> {
    let v = conn
        .get_json(
            &format!("/library/metadata/{key}/{what}?X-Plex-Container-Start=0&X-Plex-Container-Size=500"),
            20,
        )
        .map_err(Why::Failed)?;
    Ok(v["MediaContainer"]["Metadata"].as_array().cloned().unwrap_or_default())
}

/// The episode to play of a list in order: the one begun, else the first not watched, else the first.
fn next_episode(list: &[Value]) -> Option<&Value> {
    list.iter()
        .find(|e| e["viewOffset"].as_i64().unwrap_or(0) > 0)
        .or_else(|| list.iter().find(|e| e["viewCount"].as_i64().unwrap_or(0) == 0))
        .or(list.first())
}

pub fn resolve(cfg: &crate::plex::Config, server: &str, key: &str) -> Result<Item, Why> {
    let conn = crate::plex::connect(cfg, server).map_err(Why::Failed)?;
    let o = meta(&conn, key)?;
    let target = match o["type"].as_str().unwrap_or_default() {
        "movie" | "episode" | "clip" => o,
        t @ ("show" | "season") => {
            let list = episodes(&conn, key, if t == "show" { "allLeaves" } else { "children" })?;
            let rk = next_episode(&list)
                .and_then(|e| e["ratingKey"].as_str())
                .ok_or_else(|| Why::Failed("There is no episode to play.".into()))?;
            meta(&conn, rk)?
        }
        _ => return Err(Why::Elsewhere),
    };
    item_from(&conn, &target).ok_or_else(|| Why::Failed("The server sent no video for this title.".into()))
}

pub fn item_from(conn: &Conn, o: &Value) -> Option<Item> {
    let id = text(&o["ratingKey"])?;
    let part = &o["Media"][0]["Part"][0];
    let total = o["duration"].as_i64().unwrap_or(0) * 10_000;
    let at = o["viewOffset"].as_i64().unwrap_or(0) * 10_000;
    let sub = if o["type"] == "episode" {
        let series = o["grandparentTitle"].as_str().unwrap_or_default();
        match (o["parentIndex"].as_i64(), o["index"].as_i64()) {
            (Some(s), Some(e)) => format!("{series}   S{s} E{e}"),
            _ => series.to_string(),
        }
    } else {
        o["year"].as_i64().map(|y| y.to_string()).unwrap_or_default()
    };
    let streams = part["Stream"].as_array().cloned().unwrap_or_default();
    let (mut audio, mut subs) = (vec![], vec![]);
    let (mut default_audio, mut default_sub) = (None, None);
    for s in &streams {
        let Some(index) = s["index"].as_i64() else { continue };
        let name = s["displayTitle"].as_str().or(s["language"].as_str()).unwrap_or("Track");
        let lang = s["languageCode"]
            .as_str()
            .or(s["languageTag"].as_str())
            .unwrap_or_default()
            .to_string();
        let sid = s["id"].as_i64().map(|i| i.to_string()).unwrap_or_default();
        let selected = s["selected"] == true;
        match s["streamType"].as_i64() {
            Some(2) => {
                let mut t = Track::new(index, name);
                t.lang = lang.clone();
                t.key = sid;
                audio.push(t);
                if selected {
                    default_audio = Some(index);
                }
            }
            Some(3) => {
                let text_codec = matches!(
                    s["codec"].as_str().unwrap_or_default(),
                    "srt" | "subrip" | "ass" | "ssa" | "mov_text" | "webvtt" | "text"
                );
                if selected {
                    default_sub = Some(index);
                }
                if !text_codec {
                    continue;
                }
                let mut t = Track::new(index, name);
                t.lang = lang.clone();
                t.forced = s["forced"] == true;
                t.external = s["key"].is_string();
                t.key = s["key"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| format!("/library/streams/{sid}"));
                subs.push(t);
            }
            _ => {}
        }
    }
    Some(Item {
        backend: Backend::Plex(conn.clone()),
        audio,
        subs,
        default_audio,
        default_sub,
        chapters: extras::plex_chapters(o),
        segments: extras::plex_segments(o),
        trick: extras::plex_trick(conn, o, part),
        direct: conn.url(part["key"].as_str()?),
        source: part["id"].as_i64().map(|i| i.to_string()).unwrap_or_default(),
        name: o["title"].as_str().unwrap_or("Video").to_string(),
        sub,
        resume: resume_point(at, total),
        total,
        session: session_id(),
        series: text(&o["grandparentRatingKey"]).filter(|_| o["type"] == "episode"),
        id,
    })
}

/// The first episode of `series` not watched, if it is not `current`. `server` is the connection of `current`.
pub fn next(conn: &Conn, series: &str, current: &str) -> Option<Item> {
    let list = episodes(conn, series, "allLeaves").ok()?;
    let e = list
        .iter()
        .find(|e| e["viewCount"].as_i64().unwrap_or(0) == 0 && e["ratingKey"] != current)?;
    item_from(conn, &meta(conn, e["ratingKey"].as_str()?).ok()?)
}

/// The server's HLS transcode of the film, from `start` ticks in. It copies what the TV plays (the transcoder is
/// asked for direct streaming) and converts the rest.
pub fn hls_url(conn: &Conn, it: &Item, start: i64, audio: Option<i64>) -> String {
    let stream = audio
        .and_then(|i| it.audio.iter().find(|t| t.index == i))
        .map_or(String::new(), |t| format!("&audioStreamID={}", t.key));
    format!(
        "{}/video/:/transcode/universal/start.m3u8?path=/library/metadata/{}&mediaIndex=0&partIndex=0&protocol=hls\
         &offset={}&fastSeek=1&directPlay=0&directStream=1&directStreamAudio=1&videoQuality=100&subtitles=none\
         &audioBoost=100&session={}&X-Plex-Session-Identifier={}&X-Plex-Client-Identifier={}&X-Plex-Product=tvlauncher\
         &X-Plex-Platform=webOS&X-Plex-Token={}{stream}",
        conn.base,
        it.id,
        start / TICKS,
        it.session,
        it.session,
        conn.client_id,
        conn.token
    )
}

/// A sidecar or extracted subtitle track from the server: SRT, or ASS read down to its dialogue.
pub fn subtitles(conn: &Conn, t: &Track) -> Result<Vec<crate::subs::Cue>, String> {
    let body = conn.get_bytes(&t.key, 20)?;
    let text = String::from_utf8_lossy(&body);
    Ok(if text.contains("[Script Info]") || text.contains("[Events]") {
        crate::subs::parse_ass(&text)
    } else {
        crate::subs::parse_srt(&text)
    })
}

pub fn report(conn: &Conn, it: &Summary, event: Event, ticks: i64, paused: bool, done: bool) -> Result<(), String> {
    let state = match (event, paused) {
        (Event::Stop, _) => "stopped",
        (_, true) => "paused",
        _ => "playing",
    };
    let ms = if done && event == Event::Stop {
        it.total
    } else {
        ticks.max(0)
    } / 10_000;
    let url = format!(
        "{}/:/timeline?ratingKey={id}&key=/library/metadata/{id}&identifier=com.plexapp.plugins.library\
         &state={state}&time={ms}&duration={}&X-Plex-Session-Identifier={}",
        conn.base,
        it.total / 10_000,
        it.session,
        id = it.id
    );
    let h = crate::plex::headers(&conn.client_id, Some(&conn.token));
    crate::net::send(&url, None, &h, 6)?;
    if done && event == Event::Stop {
        let url = format!(
            "{}/:/scrobble?key={}&identifier=com.plexapp.plugins.library",
            conn.base, it.id
        );
        crate::net::send(&url, None, &h, 6)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Conn {
        Conn {
            base: "http://plex.example:32400".into(),
            token: "tok".into(),
            client_id: "cid".into(),
        }
    }

    fn movie() -> Value {
        json!({ "ratingKey": "42", "type": "movie", "title": "Film", "year": 1999, "duration": 6_000_000, "viewOffset": 1_200_000,
          "Media": [{ "Part": [{ "id": 7, "key": "/library/parts/7/1700000/file.mkv", "Stream": [
            { "id": 1, "index": 0, "streamType": 1, "codec": "h264" },
            { "id": 2, "index": 1, "streamType": 2, "codec": "ac3", "displayTitle": "French (AC3 5.1)", "selected": true },
            { "id": 3, "index": 2, "streamType": 2, "codec": "dca", "language": "English" },
            { "id": 4, "index": 3, "streamType": 3, "codec": "srt", "displayTitle": "English (SRT External)", "key": "/library/streams/4", "selected": true },
            { "id": 5, "index": 4, "streamType": 3, "codec": "pgs", "displayTitle": "French (PGS)" },
            { "id": 6, "index": 5, "streamType": 3, "codec": "ass", "displayTitle": "French (ASS)" },
          ] }] }] })
    }

    #[test]
    fn a_film_names_its_file_tracks_and_resume_point() {
        let it = item_from(&conn(), &movie()).unwrap();
        assert_eq!(
            (it.id.as_str(), it.source.as_str(), it.sub.as_str()),
            ("42", "7", "1999")
        );
        assert_eq!(
            it.direct,
            "http://plex.example:32400/library/parts/7/1700000/file.mkv?X-Plex-Token=tok"
        );
        assert_eq!((it.total, it.resume), (6000 * TICKS, 1200 * TICKS));
        assert_eq!(it.audio.iter().map(|t| t.index).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            it.subs
                .iter()
                .map(|t| (t.index, t.external, t.key.as_str()))
                .collect::<Vec<_>>(),
            [(3, true, "/library/streams/4"), (5, false, "/library/streams/6")],
            "the picture track is left to the file"
        );
        assert_eq!((it.default_audio, it.default_sub), (Some(1), Some(3)));
    }

    #[test]
    fn the_transcode_asks_for_direct_streaming_from_the_start_point() {
        let it = item_from(&conn(), &movie()).unwrap();
        let u = hls_url(&conn(), &it, 90 * TICKS, Some(2));
        assert!(
            u.starts_with("http://plex.example:32400/video/:/transcode/universal/start.m3u8?path=/library/metadata/42")
        );
        for p in [
            "protocol=hls",
            "offset=90",
            "directStream=1",
            "audioStreamID=3",
            "X-Plex-Token=tok",
            "X-Plex-Client-Identifier=cid",
        ] {
            assert!(u.contains(p), "{p}");
        }
        assert!(!u.contains(' ') && !u.contains('\n'));
    }

    #[test]
    fn a_show_plays_the_episode_begun_then_the_first_unwatched() {
        let list = vec![
            json!({ "ratingKey": "1", "viewCount": 1 }),
            json!({ "ratingKey": "2", "viewCount": 1 }),
            json!({ "ratingKey": "3" }),
            json!({ "ratingKey": "4" }),
        ];
        assert_eq!(next_episode(&list).unwrap()["ratingKey"], "3");
        let mut begun = list.clone();
        begun[1]["viewOffset"] = json!(5000);
        assert_eq!(next_episode(&begun).unwrap()["ratingKey"], "2");
        let all_seen: Vec<Value> = list
            .iter()
            .map(|_| json!({ "ratingKey": "9", "viewCount": 2 }))
            .collect();
        assert_eq!(
            next_episode(&all_seen).unwrap()["ratingKey"],
            "9",
            "everything seen starts again"
        );
        assert!(next_episode(&[]).is_none());
    }
}
