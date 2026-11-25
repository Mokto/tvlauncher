//! What a server knows about a title besides its tracks: chapters, the segments a viewer may skip (intro, recap,
//! credits) and the thumbnails shown while seeking. Both servers are read into the same shapes.
use super::{text, TICKS};
use crate::plex::Conn;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    /// Ticks (100 ns) from the start.
    pub start: i64,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    Intro,
    Recap,
    Credits,
    Preview,
}

impl SegKind {
    pub fn label(self) -> &'static str {
        match self {
            SegKind::Intro => "Skip intro",
            SegKind::Recap => "Skip recap",
            SegKind::Credits => "Skip credits",
            SegKind::Preview => "Skip preview",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: SegKind,
    pub start: i64,
    pub end: i64,
}

/// Where the seeking thumbnails come from.
#[derive(Clone)]
pub enum Trick {
    /// Jellyfin's trickplay: JPEG sheets of `tile.0` x `tile.1` thumbnails of `size`, one per `interval_ms`.
    Sheets {
        cfg: crate::jellyfin::Config,
        id: String,
        source: String,
        width: i64,
        tile: (i64, i64),
        size: (i64, i64),
        count: i64,
        interval_ms: i64,
    },
    /// Plex's BIF index of a part, one JPEG per `interval` (read from the file).
    Bif { conn: Conn, path: String },
}

/// The chapter at `ticks`: the last that has begun.
pub fn chapter_at(chapters: &[Chapter], ticks: i64) -> Option<usize> {
    chapters.iter().rposition(|c| c.start <= ticks)
}

/// The segment on screen at `ticks` that has at least a second left.
pub fn segment_at(segments: &[Segment], ticks: i64) -> Option<&Segment> {
    segments.iter().find(|s| s.start <= ticks && ticks + TICKS < s.end)
}

/// A chapter name, or "Chapter n" for the nameless (Jellyfin names them "Chapter 1" itself, Plex often not at all).
fn chapter_name(name: Option<&str>, n: usize) -> String {
    match name.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => format!("Chapter {n}"),
    }
}

pub fn jellyfin_chapters(o: &Value) -> Vec<Chapter> {
    let list = o["Chapters"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<Chapter> = list
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            Some(Chapter {
                start: c["StartPositionTicks"].as_i64()?,
                name: chapter_name(c["Name"].as_str(), i + 1),
            })
        })
        .collect();
    out.sort_by_key(|c| c.start);
    // One chapter is no chapters.
    if out.len() < 2 {
        out.clear();
    }
    out
}

/// `GET /MediaSegments/<id>`: `{"Items":[{"Type":"Intro","StartTicks":..,"EndTicks":..}]}`.
pub fn jellyfin_segments(v: &Value) -> Vec<Segment> {
    v["Items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let kind = match s["Type"].as_str()? {
                "Intro" => SegKind::Intro,
                "Recap" => SegKind::Recap,
                "Outro" => SegKind::Credits,
                "Preview" => SegKind::Preview,
                _ => return None,
            };
            let (start, end) = (s["StartTicks"].as_i64()?, s["EndTicks"].as_i64()?);
            (end > start).then_some(Segment { kind, start, end })
        })
        .collect()
}

/// The item's `Trickplay` for the media source: the width nearest 320 that the server made.
pub fn jellyfin_trick(cfg: &crate::jellyfin::Config, o: &Value, id: &str, source: &str) -> Option<Trick> {
    let widths = o["Trickplay"][source].as_object()?;
    let (key, t) = widths
        .iter()
        .filter_map(|(k, t)| Some((k.parse::<i64>().ok()?, t)))
        .min_by_key(|(w, _)| (w - 320).abs())?;
    let get = |k: &str| t[k].as_i64().filter(|n| *n > 0);
    Some(Trick::Sheets {
        cfg: cfg.clone(),
        id: id.to_string(),
        source: source.to_string(),
        width: key,
        tile: (get("TileWidth")?, get("TileHeight")?),
        size: (get("Width")?, get("Height")?),
        count: get("ThumbnailCount")?,
        interval_ms: get("Interval")?,
    })
}

pub fn plex_chapters(o: &Value) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = o["Chapter"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, c)| {
            Some(Chapter {
                start: c["startTimeOffset"].as_i64()? * 10_000,
                name: chapter_name(c["tag"].as_str(), i + 1),
            })
        })
        .collect();
    out.sort_by_key(|c| c.start);
    if out.len() < 2 {
        out.clear();
    }
    out
}

/// Plex's `Marker`s: `{"type":"intro","startTimeOffset":..,"endTimeOffset":..}` in milliseconds.
pub fn plex_segments(o: &Value) -> Vec<Segment> {
    o["Marker"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let kind = match m["type"].as_str()? {
                "intro" => SegKind::Intro,
                "credits" => SegKind::Credits,
                _ => return None,
            };
            let (start, end) = (
                m["startTimeOffset"].as_i64()? * 10_000,
                m["endTimeOffset"].as_i64()? * 10_000,
            );
            (end > start).then_some(Segment { kind, start, end })
        })
        .collect()
}

/// The BIF index of a Plex part, when the server made one (`hasThumbnail`).
pub fn plex_trick(conn: &Conn, o: &Value, part: &Value) -> Option<Trick> {
    let has = part["hasThumbnail"] == "1" || part["hasThumbnail"] == true || part["hasThumbnail"] == 1;
    let id = part["id"].as_i64()?;
    let stamp = o["updatedAt"]
        .as_i64()
        .map(|n| n.to_string())
        .or_else(|| text(&o["updatedAt"]))?;
    has.then(|| Trick::Bif {
        conn: conn.clone(),
        path: format!("/library/parts/{id}/indexes/sd/{stamp}"),
    })
}

// ---- BIF ----------------------------------------------------------------------------------------------------------

const BIF_MAGIC: [u8; 8] = [0x89, b'B', b'I', b'F', 0x0d, 0x0a, 0x1a, 0x0a];
pub const BIF_HEAD: usize = 64;
#[cfg(test)]
pub const BIF_MAGIC_FOR_TESTS: [u8; 8] = BIF_MAGIC;

/// The image count and the timestamp unit in milliseconds, from a BIF header.
pub fn bif_header(h: &[u8]) -> Option<(usize, u32)> {
    if h.len() < BIF_HEAD || h[..8] != BIF_MAGIC {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let (n, unit) = (u32_at(12) as usize, u32_at(16));
    (n > 0 && n < 100_000).then_some((n, if unit == 0 { 1000 } else { unit }))
}

/// The index of a BIF: (timestamp, byte offset) for each image and a last entry whose offset is the end of the data.
pub fn bif_index(table: &[u8], n: usize) -> Option<Vec<(u32, u32)>> {
    if table.len() < (n + 1) * 8 {
        return None;
    }
    let e = |i: usize| {
        let b = &table[i * 8..][..8];
        (
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        )
    };
    let v: Vec<_> = (0..=n).map(e).collect();
    // Offsets only grow.
    v.windows(2).all(|w| w[0].1 <= w[1].1).then_some(v)
}

/// The image of the index that shows `ms`: the last whose timestamp (in `unit` ms) has come.
pub fn bif_image_at(index: &[(u32, u32)], unit: u32, ms: i64) -> usize {
    let n = index.len().saturating_sub(1);
    let t = (ms.max(0) / i64::from(unit.max(1))) as u32;
    index[..n].partition_point(|e| e.0 <= t).saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jellyfin_chapters_and_segments() {
        let o = json!({"Chapters":[{"Name":"Cold open","StartPositionTicks":0},{"Name":"","StartPositionTicks":600_000_000i64}]});
        let c = jellyfin_chapters(&o);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].name, "Chapter 2");
        assert_eq!(chapter_at(&c, 599_999_999), Some(0));
        assert_eq!(chapter_at(&c, 600_000_000), Some(1));
        assert!(jellyfin_chapters(&json!({"Chapters":[{"Name":"x","StartPositionTicks":0}]})).is_empty());
        let v = json!({"Items":[
            {"Type":"Intro","StartTicks":100,"EndTicks":900_000_000i64},
            {"Type":"Outro","StartTicks":5,"EndTicks":4},
            {"Type":"Commercial","StartTicks":1,"EndTicks":9},
            {"Type":"Outro","StartTicks":30_000_000_000i64,"EndTicks":32_000_000_000i64}]});
        let s = jellyfin_segments(&v);
        assert_eq!(
            s.iter().map(|s| s.kind).collect::<Vec<_>>(),
            [SegKind::Intro, SegKind::Credits]
        );
        assert!(segment_at(&s, 50_000_000).is_some());
        assert!(segment_at(&s, 895_000_000).is_none(), "under a second left");
        assert!(segment_at(&s, 1_000_000_000).is_none());
    }

    #[test]
    fn plex_chapters_and_markers_are_milliseconds() {
        let o = json!({"Chapter":[{"tag":"One","startTimeOffset":0},{"startTimeOffset":90_000}],
                       "Marker":[{"type":"intro","startTimeOffset":5_000,"endTimeOffset":65_000},{"type":"credits","startTimeOffset":7,"endTimeOffset":9},{"type":"commercial","startTimeOffset":1,"endTimeOffset":2}]});
        let c = plex_chapters(&o);
        assert_eq!((c[1].start, c[1].name.as_str()), (900_000_000, "Chapter 2"));
        let s = plex_segments(&o);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end), (50_000_000, 650_000_000));
    }

    #[test]
    fn trickplay_picks_the_width_nearest_320() {
        let cfg = crate::jellyfin::Config::default();
        let t = |w: i64| json!({"Width":w,"Height":w/2,"TileWidth":10,"TileHeight":10,"ThumbnailCount":250,"Interval":10000});
        let o = json!({"Trickplay":{"src":{"160":t(160),"480":t(480),"320":t(320)}}});
        let Some(Trick::Sheets {
            width,
            tile,
            interval_ms,
            ..
        }) = jellyfin_trick(&cfg, &o, "i", "src")
        else {
            panic!()
        };
        assert_eq!((width, tile, interval_ms), (320, (10, 10), 10_000));
        assert!(jellyfin_trick(&cfg, &o, "i", "other").is_none());
    }

    #[test]
    fn a_bif_is_found_by_time() {
        let mut h = BIF_MAGIC.to_vec();
        h.extend([0, 0, 0, 0, 3, 0, 0, 0, 0xe8, 3, 0, 0]);
        h.resize(BIF_HEAD, 0);
        assert_eq!(bif_header(&h), Some((3, 1000)));
        assert!(bif_header(&h[..60]).is_none());
        let mut table = vec![];
        for (t, o) in [(0u32, 100u32), (2, 200), (4, 300), (u32::MAX, 450)] {
            table.extend(t.to_le_bytes());
            table.extend(o.to_le_bytes());
        }
        let idx = bif_index(&table, 3).unwrap();
        assert_eq!(bif_image_at(&idx, 1000, 0), 0);
        assert_eq!(bif_image_at(&idx, 1000, 3_999), 1);
        assert_eq!(bif_image_at(&idx, 1000, 4_000), 2);
        assert_eq!(bif_image_at(&idx, 1000, 999_000), 2, "the last image holds on");
        assert!(bif_index(&table[..30], 3).is_none());
    }
}
