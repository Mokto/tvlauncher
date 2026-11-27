//! The library tabs: each movie, series or collection library of the signed-in Jellyfin user and Plex account in
//! a tab of its own, as rows of posters (Continue watching, Recently added, Unwatched, genres) and a grid of all
//! titles, under the focused title's backdrop, details and overview. OK opens the title's page (`detail`), or the
//! title in Native Jelly or PlxNative when films do not play in the launcher.
//!
//! The lists are cached (`cache/lib-*.json` for Jellyfin, `cache/plib-*.json` for Plex) and shown at once; a background thread asks for new ones when they
//! are older than `MAX_AGE`, with the same short timeout as Continue watching, so a sleeping server is never
//! waited for. Pictures are fetched only for the posters on screen and the focused backdrop.
use crate::data::{Item, Kind, DIR};
use crate::gfx::*;
use crate::jellyfin::{clip, Config};
use crate::plex::encode;
use crate::sdl::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};

const MAX_AGE: u64 = 5 * 60;
/// The cache prefixes, which keep each source's lists and pictures apart.
pub const JELLYFIN: &str = "lib";
pub const PLEX: &str = "plib";
const POSTER_W: u32 = 300;
const BACKDROP_W: u32 = 1280;

#[derive(Clone)]
pub struct View {
    pub id: String,
    pub name: String,
    /// The Plex server's `machineIdentifier`, which PlxNative needs; empty for Jellyfin.
    pub server: String,
    pub server_name: String,
    /// Where a Plex library's pictures come from (saved with the list, which is why that file is private); None
    /// for Jellyfin, whose address is in its login.
    pub src: Source,
}

/// Where a library's pictures come from.
#[derive(Clone)]
pub enum Source {
    None,
    Jellyfin(String),
    Plex { base: String, token: String },
}

impl Source {
    /// An episode's still, `w` wide.
    pub fn still(&self, id: &str, tag: &str, w: u32) -> Option<String> {
        match self {
            Source::None => None,
            Source::Jellyfin(server) => Some(format!(
                "{server}/Items/{id}/Images/Primary?maxWidth={w}&quality=80&tag={tag}"
            )),
            Source::Plex { base, token } => Some(format!(
                "{base}/photo/:/transcode?width={w}&height={}&minSize=1&upscale=1&url={}&X-Plex-Token={token}",
                w * 9 / 16,
                encode(tag)
            )),
        }
    }

    /// The title's logo, with its transparency: a PNG.
    pub fn logo(&self, id: &str, tag: &str) -> Option<String> {
        match self {
            Source::None => None,
            Source::Jellyfin(server) => Some(format!(
                "{server}/Items/{id}/Images/Logo?maxWidth=660&maxHeight=230&format=Png&tag={tag}"
            )),
            Source::Plex { base, token } => Some(format!(
                "{base}/photo/:/transcode?width=660&height=230&minSize=0&upscale=0&format=png&url={}&X-Plex-Token={token}",
                encode(tag)
            )),
        }
    }

    /// A person's photo or a poster, at least `w` wide. Plex fetches the picture when `tag` is an address of its
    /// own (cast photos are on plex.tv), and scales it.
    pub fn picture(&self, id: &str, tag: &str, w: u32) -> Option<String> {
        match self {
            Source::Plex { base, token } => Some(format!(
                "{base}/photo/:/transcode?width={w}&height={w}&minSize=1&upscale=1&url={}&X-Plex-Token={token}",
                encode(tag)
            )),
            _ => self.still(id, tag, w),
        }
    }

    fn url(&self, id: &str, backdrop: bool, tag: &str) -> Option<String> {
        match self {
            Source::None => None,
            Source::Jellyfin(server) => {
                let (kind, w) = if backdrop {
                    ("Backdrop/0", BACKDROP_W)
                } else {
                    ("Primary", POSTER_W)
                };
                Some(format!(
                    "{server}/Items/{id}/Images/{kind}?maxWidth={w}&quality=80&tag={tag}"
                ))
            }
            Source::Plex { base, token } => {
                let (w, h) = if backdrop {
                    (BACKDROP_W, BACKDROP_W * 9 / 16)
                } else {
                    (POSTER_W, POSTER_W * 3 / 2)
                };
                Some(format!(
                    "{base}/photo/:/transcode?width={w}&height={h}&minSize=1&upscale=1&url={}&X-Plex-Token={token}",
                    encode(tag)
                ))
            }
        }
    }
}

#[derive(Clone)]
pub struct Title {
    pub id: String,
    pub name: String,
    sort: String,
    added: String,
    /// Year, length, rating and genres, ready to show.
    pub meta: String,
    pub overview: String,
    progress: f32,
    played: bool,
    rating: f32,
    genres: Vec<String>,
    poster: Option<String>,
    backdrop: Option<String>,
    /// See [`tmdb_key`]: the poster and backdrop are kept under it, once for both servers.
    pub key: Option<String>,
}

pub enum Got {
    Views(Vec<View>),
    Titles(String, Vec<Title>, Source),
    Refreshed,
}

/// What a library holds, by its `CollectionType`; None for libraries that are not films or series (music,
/// books, photos, live TV).
fn types(collection: Option<&str>) -> Option<&'static str> {
    match collection {
        Some("movies") => Some("Movie"),
        Some("tvshows") => Some("Series"),
        Some("boxsets") => Some("BoxSet"),
        Some("homevideos" | "musicvideos") => Some("Video,MusicVideo"),
        None | Some("mixed") => Some("Movie,Series"),
        _ => None,
    }
}

fn title(o: &Value) -> Option<Title> {
    let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
    let series = o["Type"] == "Series";
    let year = o["ProductionYear"].as_i64().map(|y| y.to_string()).unwrap_or_default();
    let end = o["EndDate"].as_str().and_then(|d| d.get(..4)).unwrap_or_default();
    let years = match (series, o["Status"].as_str()) {
        (true, Some("Continuing")) if !year.is_empty() => format!("{year}–"),
        (true, _) if !end.is_empty() && end != year => format!("{year}–{end}"),
        _ => year,
    };
    let minutes = if series {
        0
    } else {
        o["RunTimeTicks"].as_i64().unwrap_or(0) / 600_000_000
    };
    let rating = o["CommunityRating"].as_f64();
    let genres: Vec<String> = o["Genres"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| Some(g.as_str()?.to_string()))
        .collect();
    let meta = meta(years, minutes, rating, &s("OfficialRating"), &genres);
    let ud = &o["UserData"];
    Some(Title {
        id: o["Id"].as_str()?.into(),
        name: o["Name"].as_str()?.into(),
        sort: o["SortName"].as_str().map_or_else(|| s("Name"), str::to_lowercase),
        added: s("DateCreated"),
        meta,
        overview: s("Overview"),
        progress: if ud["Played"] == true {
            0.0
        } else {
            ud["PlayedPercentage"].as_f64().unwrap_or(0.0) as f32 / 100.0
        },
        played: ud["Played"] == true,
        rating: rating.unwrap_or(0.0) as f32,
        genres,
        poster: o["ImageTags"]["Primary"].as_str().map(String::from),
        backdrop: o["BackdropImageTags"][0].as_str().map(String::from),
        key: tmdb_key(o),
    })
}

/// Year, length, rating, age rating and the first genres, as shown under the title.
fn meta(years: String, minutes: i64, rating: Option<f64>, age: &str, genres: &[String]) -> String {
    let length = match minutes {
        0 => String::new(),
        m if m >= 60 => format!("{} h {:02} min", m / 60, m % 60),
        m => format!("{m} min"),
    };
    let rating = rating.map(|r| format!("{r:.1}/10")).unwrap_or_default();
    [
        years,
        length,
        rating,
        age.to_string(),
        genres.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("   ·   ")
}

/// A film or show of a Plex section (`/library/sections/<key>/all`).
pub(crate) fn plex_title(o: &Value) -> Option<Title> {
    let s = |k: &str| o[k].as_str().filter(|s| !s.is_empty()).map(String::from);
    let show = o["type"] == "show";
    let num = |k: &str| o[k].as_f64().unwrap_or(0.0);
    let id = s("ratingKey").or_else(|| o["ratingKey"].as_u64().map(|n| n.to_string()))?;
    let name = s("title")?;
    let genres: Vec<String> = o["Genre"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| Some(g["tag"].as_str()?.to_string()))
        .collect();
    let rating = o["audienceRating"].as_f64().or_else(|| o["rating"].as_f64());
    let year = o["year"].as_i64().map(|y| y.to_string()).unwrap_or_default();
    let minutes = if show { 0 } else { num("duration") as i64 / 60_000 };
    let played = if show {
        num("leafCount") > 0.0 && num("viewedLeafCount") >= num("leafCount")
    } else {
        num("viewCount") > 0.0
    };
    Some(Title {
        sort: s("titleSort").unwrap_or_else(|| name.clone()).to_lowercase(),
        added: format!("{:012}", num("addedAt") as u64),
        meta: meta(year, minutes, rating, &s("contentRating").unwrap_or_default(), &genres),
        overview: s("summary").unwrap_or_default(),
        progress: if show || num("duration") <= 0.0 {
            0.0
        } else {
            (num("viewOffset") / num("duration")).clamp(0.0, 1.0) as f32
        },
        played,
        rating: rating.unwrap_or(0.0) as f32,
        genres,
        poster: s("thumb"),
        backdrop: s("art"),
        key: tmdb_key(o),
        id,
        name,
    })
}

fn cache(root: &str, pre: &str, name: &str) -> String {
    format!("{root}{DIR}/cache/{pre}-{name}.json")
}

fn read(root: &str, pre: &str, name: &str) -> Value {
    std::fs::read(cache(root, pre, name))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn cached_views(root: &str, pre: &str) -> Vec<View> {
    read(root, pre, "views")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(View {
                id: v["id"].as_str()?.into(),
                name: v["name"].as_str()?.into(),
                server: v["server"].as_str().unwrap_or_default().into(),
                server_name: v["serverName"].as_str().unwrap_or_default().into(),
                src: match (v["base"].as_str(), v["token"].as_str()) {
                    (Some(base), Some(token)) => Source::Plex {
                        base: base.into(),
                        token: token.into(),
                    },
                    _ => Source::None,
                },
            })
        })
        .collect()
}

pub fn cached_titles(root: &str, pre: &str, view: &str) -> Vec<Title> {
    read(root, pre, view)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let s = |k: &str| t[k].as_str().map(String::from);
            Some(Title {
                id: s("id")?,
                name: s("name")?,
                sort: s("sort").unwrap_or_default(),
                added: s("added").unwrap_or_default(),
                meta: s("meta").unwrap_or_default(),
                overview: s("overview").unwrap_or_default(),
                progress: t["progress"].as_f64().unwrap_or(0.0) as f32,
                played: t["played"] == true,
                rating: t["rating"].as_f64().unwrap_or(0.0) as f32,
                genres: t["genres"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|g| Some(g.as_str()?.to_string()))
                    .collect(),
                poster: s("poster"),
                backdrop: s("backdrop"),
                key: s("key"),
            })
        })
        .collect()
}

pub(crate) fn save_views(root: &str, pre: &str, views: &[View]) -> Result<(), String> {
    std::fs::create_dir_all(format!("{root}{DIR}/cache")).map_err(|e| e.to_string())?;
    let v: Vec<Value> = views
        .iter()
        .map(|v| match &v.src {
            Source::Plex { base, token } => {
                json!({ "id": v.id, "name": v.name, "server": v.server, "serverName": v.server_name, "base": base, "token": token })
            }
            _ => json!({ "id": v.id, "name": v.name, "server": v.server }),
        })
        .collect();
    crate::data::write_private(&cache(root, pre, "views"), Value::from(v).to_string().as_bytes())
        .map_err(|e| e.to_string())
}

pub(crate) fn save_titles(root: &str, pre: &str, view: &str, list: &[Title]) -> Result<(), String> {
    crate::sdl::log(&format!("library {pre}-{view}: {} titles", list.len()));
    // A shared name does not change with the picture: one the server changed is deleted, to be fetched again.
    let old = cached_titles(root, pre, view);
    for (t, o) in list
        .iter()
        .filter_map(|t| Some((t, old.iter().find(|o| o.id == t.id)?)))
    {
        for (kind, new, was) in [('p', &t.poster, &o.poster), ('b', &t.backdrop, &o.backdrop)] {
            if let (Some(k), true) = (&t.key, new != was) {
                let _ = std::fs::remove_file(format!("{root}{DIR}/cache/t-{kind}-{k}.img"));
            }
        }
    }
    let v: Vec<Value> = list
        .iter()
        .map(|t| {
            json!({ "id": t.id, "name": t.name, "sort": t.sort, "added": t.added, "meta": t.meta,
                "overview": t.overview, "progress": t.progress, "played": t.played, "rating": t.rating,
                "genres": t.genres, "poster": t.poster, "backdrop": t.backdrop, "key": t.key })
        })
        .collect();
    crate::data::write_private(&cache(root, pre, view), Value::from(v).to_string().as_bytes())
        .map_err(|e| e.to_string())
}

pub(crate) fn mark_refreshed(root: &str, pre: &str) -> Result<(), String> {
    std::fs::write(cache(root, pre, "refreshed"), []).map_err(|e| e.to_string())
}

pub fn last_refresh(root: &str, pre: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(cache(root, pre, "refreshed")).ok()?.modified().ok()
}

/// True when the saved lists are young enough not to ask again.
pub(crate) fn fresh(root: &str, pre: &str) -> bool {
    std::fs::metadata(cache(root, pre, "views"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|d| d.as_secs() < MAX_AGE)
}

/// Libraries and titles that are gone take their lists and pictures with them. `keep` holds the ids of the
/// libraries and titles there are.
pub(crate) fn prune(root: &str, pre: &str, keep: &HashSet<String>) {
    for e in std::fs::read_dir(format!("{root}{DIR}/cache"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(pre).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        let id = rest
            .strip_suffix(".json")
            .or_else(|| rest.get(2..).and_then(|r| r.split('-').next()));
        if id.is_some_and(|id| !matches!(id, "views" | "refreshed") && !keep.contains(id)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub(crate) fn plex_cache_id(server: &str, id: &str) -> String {
    let server: String = server.chars().filter(char::is_ascii_alphanumeric).collect();
    format!("{server}_{id}")
}

/// A film's or series' TMDB id as `tmdbm603` or `tmdbtv1668`, from a Jellyfin item (its `ProviderIds`) or Plex
/// metadata (its `Guid`s, sent with `includeGuids=1`). Both servers name the same title the same way.
pub(crate) fn tmdb_key(o: &Value) -> Option<String> {
    let (series, id) = if o["Type"].is_string() {
        (o["Type"] == "Series", o["ProviderIds"]["Tmdb"].as_str()?)
    } else {
        let guids = o["Guid"].as_array()?;
        let id = guids.iter().find_map(|g| g["id"].as_str()?.strip_prefix("tmdb://"))?;
        (o["type"] == "show", id)
    };
    let kind = if series { "tv" } else { "m" };
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| format!("tmdb{kind}{id}"))
}

/// Where a title's poster (`kind` `p`) or backdrop (`b`) is cached: `cache/t-<kind>-<key>.img` for a title with a
/// TMDB key, shared by both servers; else the server's own, see [`pic_path`].
pub(crate) fn title_pic(pre: &str, kind: char, id: &str, key: Option<&str>, tag: &str) -> Option<String> {
    match key {
        Some(k) => Some(format!("{DIR}/cache/t-{kind}-{k}.img")),
        None => pic_path(pre, kind, id, tag),
    }
}

/// Where a picture is cached: named after its tag (Jellyfin) or the end of its path (Plex, a timestamp), so a new
/// picture gets a new name. `kind` is `p` for a poster, `b` for a backdrop; `id` comes first after it, as [`prune`]
/// reads it.
pub(crate) fn pic_path(pre: &str, kind: char, id: &str, tag: &str) -> Option<String> {
    let end: String = tag
        .rsplit('/')
        .next()?
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    Some(format!(
        "{DIR}/cache/{pre}-{kind}-{id}-{}.img",
        &end[end.len().saturating_sub(8)..]
    ))
}

/// The Jellyfin user's libraries and their titles.
pub fn refresh(root: &str, cfg: &Config, force: bool, send: &dyn Fn(Got) -> bool) -> Result<(), String> {
    if !force && fresh(root, JELLYFIN) {
        return Ok(());
    }
    let parse = |b: Vec<u8>| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string());
    let v = parse(cfg.get(&format!("/Users/{}/Views", cfg.user_id))?)?;
    let views: Vec<(View, &str)> = v["Items"]
        .as_array()
        .ok_or("Jellyfin returned no library list")?
        .iter()
        .filter_map(|o| {
            let view = View {
                id: o["Id"].as_str()?.into(),
                name: o["Name"].as_str()?.into(),
                server: String::new(),
                server_name: String::new(),
                src: Source::None,
            };
            Some((view, types(o["CollectionType"].as_str())?))
        })
        .collect();
    let list: Vec<View> = views.iter().map(|(v, _)| v.clone()).collect();
    save_views(root, JELLYFIN, &list)?;
    if !send(Got::Views(list)) {
        return Ok(());
    }
    let mut keep = HashSet::new();
    for (view, types) in &views {
        let q = format!(
            "/Users/{}/Items?ParentId={}&Recursive=true&IncludeItemTypes={types}&SortBy=SortName\
             &Fields=Overview,Genres,DateCreated,SortName,EndDate,ProviderIds&EnableImageTypes=Primary,Backdrop\
             &ImageTypeLimit=1&EnableTotalRecordCount=false",
            cfg.user_id, view.id
        );
        let list: Vec<Title> = parse(cfg.get_within(&q, 60)?)?["Items"]
            .as_array()
            .ok_or("Jellyfin returned no title list")?
            .iter()
            .filter_map(title)
            .collect();
        save_titles(root, JELLYFIN, &view.id, &list)?;
        keep.extend(list.iter().map(|t| t.id.clone()));
        keep.insert(view.id.clone());
        if !send(Got::Titles(view.id.clone(), list, Source::Jellyfin(cfg.server.clone()))) {
            return Ok(());
        }
    }
    prune(root, JELLYFIN, &keep);
    crate::detail::prune_pictures(root);
    mark_refreshed(root, JELLYFIN)?;
    send(Got::Refreshed);
    Ok(())
}

/// Starts a refresh thread around `refresh(force, send)`: send `true` to refresh now, `false` to refresh only
/// old lists.
pub fn spawn<F>(refresh: F, wake: u32) -> (Sender<bool>, Receiver<Got>)
where
    F: Fn(bool, &dyn Fn(Got) -> bool) -> Result<(), String> + Send + 'static,
{
    let (req_tx, req_rx) = channel::<bool>();
    let (res_tx, res_rx) = channel();
    std::thread::spawn(move || {
        while let Ok(mut force) = req_rx.recv() {
            while let Ok(f) = req_rx.try_recv() {
                force |= f;
            }
            let send = |g: Got| {
                let ok = res_tx.send(g).is_ok();
                unsafe { SDL_PushEvent(&mut Event::new(wake)) };
                ok
            };
            if let Err(e) = refresh(force, &send) {
                crate::sdl::log(&format!("library: {e}"));
            }
        }
    });
    (req_tx, res_rx)
}

const CARD: (f32, f32) = (240.0, 360.0);
/// Columns of the grid of all titles; the rows above it scroll sideways instead.
const COLS: usize = 6;
/// Under a line of posters, before the next line's title.
const BELOW: f32 = 40.0;
/// Libraries this small get only the grid: the rows would repeat it.
const ROWS_FROM: usize = 24;
const ROW_MAX: usize = 30;
const GENRES: usize = 8;

/// A line of posters: a row of its own, or one line of the grid (only the first has a title).
struct Line {
    head: Option<String>,
    head_tex: Option<Tex>,
    idx: Vec<usize>,
    grid: bool,
    col: usize,
    scroll: f32,
    top: f32,
}

impl Line {
    fn new(head: Option<String>, idx: Vec<usize>, grid: bool) -> Line {
        Line {
            head,
            head_tex: None,
            idx,
            grid,
            col: 0,
            scroll: 0.0,
            top: 0.0,
        }
    }

    fn height(&self) -> f32 {
        let head = if self.head.is_some() { LABEL_H } else { 0.0 };
        head + CARD.1 + BELOW
    }
}

/// The rows and grid of a library, as indices into `titles` (the grid sorted by name or by date added).
fn lines(titles: &[Title], by_name: bool) -> Vec<Line> {
    let mut out = vec![];
    let all: Vec<usize> = (0..titles.len()).collect();
    let pick = |mut v: Vec<usize>, key: &dyn Fn(&Title) -> f32| {
        v.sort_by(|a, b| key(&titles[*b]).total_cmp(&key(&titles[*a])));
        v.truncate(ROW_MAX);
        v
    };
    let mut newest = all.clone();
    newest.sort_by(|a, b| titles[*b].added.cmp(&titles[*a].added));
    if titles.len() >= ROWS_FROM {
        let going: Vec<usize> = newest.iter().copied().filter(|i| titles[*i].progress > 0.0).collect();
        let unseen: Vec<usize> = all.iter().copied().filter(|i| !titles[*i].played).collect();
        let mut rows = vec![(
            "Continue watching".to_string(),
            going.into_iter().take(ROW_MAX).collect(),
        )];
        rows.push(("Recently added".into(), newest.iter().copied().take(ROW_MAX).collect()));
        if unseen.len() < titles.len() {
            rows.push(("Unwatched".into(), pick(unseen, &|t| t.rating)));
        }
        let mut count: HashMap<&str, usize> = HashMap::new();
        for g in titles.iter().flat_map(|t| &t.genres) {
            *count.entry(g).or_default() += 1;
        }
        let mut genres: Vec<(&str, usize)> = count.into_iter().filter(|(_, n)| *n >= 6).collect();
        genres.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        for (g, _) in genres.into_iter().take(GENRES) {
            let of: Vec<usize> = all
                .iter()
                .copied()
                .filter(|i| titles[*i].genres.iter().any(|x| x == g))
                .collect();
            rows.push((g.to_string(), pick(of, &|t| t.rating)));
        }
        out.extend(
            rows.into_iter()
                .filter(|(_, v): &(String, Vec<usize>)| !v.is_empty())
                .map(|(h, v)| Line::new(Some(h), v, false)),
        );
    }
    let mut grid = all;
    if by_name {
        grid.sort_by(|a, b| titles[*a].sort.cmp(&titles[*b].sort));
    } else {
        grid = newest;
    }
    let head = if by_name { "All, A to Z" } else { "All, newest first" };
    for (n, c) in grid.chunks(COLS).enumerate() {
        out.push(Line::new((n == 0).then(|| head.to_string()), c.to_vec(), true));
    }
    let mut top = 0.0;
    for l in &mut out {
        l.top = top;
        top += l.height();
    }
    out
}

/// The texts above the rows, for the focused title.
struct Hero {
    id: String,
    title: Option<Tex>,
    meta: Option<Tex>,
    overview: Option<Tex>,
}

pub struct Page {
    pre: &'static str,
    src: Source,
    titles: Vec<Title>,
    lines: Vec<Line>,
    cur: usize,
    scroll: f32,
    cards: HashMap<(usize, usize), Item>,
    hero: Option<Hero>,
    hero_a: f32,
    empty: Option<Tex>,
}

impl Page {
    pub fn new(g: &Gfx, pre: &'static str, src: Source, titles: Vec<Title>, by_name: bool) -> Page {
        Page {
            pre,
            src,
            lines: lines(&titles, by_name),
            titles,
            cur: 0,
            scroll: 0.0,
            cards: HashMap::new(),
            hero: None,
            hero_a: 0.0,
            empty: g.text(
                g.f_menu,
                "Nothing here yet. The library appears once the server has answered.",
                1100,
            ),
        }
    }

    /// Keeps the focus on the same title after the list or the sort changed: in the same row if it is still
    /// there, else in the grid.
    pub fn keep(&mut self, old: &Page) {
        let Some(id) = old.title().map(|t| t.id.clone()) else {
            return;
        };
        let head = old.lines.get(old.cur).and_then(|l| l.head.clone());
        let found = |l: &Line| l.idx.iter().position(|i| self.titles[*i].id == id);
        let at = (!old.lines[old.cur].grid)
            .then(|| self.lines.iter().position(|l| !l.grid && l.head == head))
            .flatten()
            .and_then(|r| Some((r, found(&self.lines[r])?)))
            .or_else(|| {
                (0..self.lines.len())
                    .filter(|r| self.lines[*r].grid)
                    .find_map(|r| Some((r, found(&self.lines[r])?)))
            });
        if let Some((r, c)) = at {
            self.cur = r;
            self.lines[r].col = c;
            self.scroll = self.lines[r].top;
        }
    }

    pub fn source(&self) -> Source {
        self.src.clone()
    }

    pub fn titles(&self) -> &[Title] {
        &self.titles
    }

    /// Puts the focus on the title `id` (in the grid, where every title is). Gives whether the page has it.
    pub fn focus_id(&mut self, id: &str) -> bool {
        let titles = &self.titles;
        let at = self.lines.iter().enumerate().rev().find_map(|(r, l)| {
            let c = l.idx.iter().position(|i| titles[*i].id == id)?;
            l.grid.then_some((r, c))
        });
        let Some((r, c)) = at else { return false };
        self.cur = r;
        self.lines[r].col = c;
        self.scroll = self.lines[r].top;
        true
    }

    pub fn title(&self) -> Option<&Title> {
        let l = self.lines.get(self.cur)?;
        self.titles.get(*l.idx.get(l.col)?)
    }

    fn path(pre: &str, t: &Title, backdrop: bool) -> Option<String> {
        let tag = if backdrop {
            t.backdrop.as_ref()
        } else {
            t.poster.as_ref()
        }?;
        title_pic(pre, if backdrop { 'b' } else { 'p' }, &t.id, t.key.as_deref(), tag)
    }

    /// Whether the page lists the title `id`.
    pub fn has(&self, id: &str) -> bool {
        self.titles.iter().any(|t| t.id == id)
    }

    /// The posters on screen: (line, position in it, title).
    fn shown(&self) -> Vec<(usize, usize, usize)> {
        let mut out = vec![];
        let first = self.cur.saturating_sub(1);
        for (r, l) in self.lines.iter().enumerate().skip(first).take(self.cur + 3 - first) {
            let from = if l.grid { 0 } else { l.col.saturating_sub(1) };
            out.extend(l.idx.iter().enumerate().skip(from).take(9).map(|(c, i)| (r, c, *i)));
        }
        out
    }

    /// The pictures to fetch: the focused backdrop, then the posters on screen and a line around them.
    pub fn wanted(&self) -> Vec<(String, String)> {
        let pic = |t: &Title, backdrop: bool| {
            let tag = if backdrop {
                t.backdrop.as_ref()
            } else {
                t.poster.as_ref()
            }?;
            Some((self.src.url(&t.id, backdrop, tag)?, Self::path(self.pre, t, backdrop)?))
        };
        let shown = self.shown().into_iter().map(|(_, _, i)| (&self.titles[i], false));
        self.title()
            .map(|t| (t, true))
            .into_iter()
            .chain(shown)
            .filter_map(|(t, b)| pic(t, b))
            .collect()
    }

    /// A picture arrived: the posters showing it load it again.
    pub fn saved(&mut self, path: &str) {
        for it in self.cards.values_mut() {
            if it.icons.first().is_some_and(|p| p == path) {
                it.icon_tried = false;
            }
        }
    }

    /// The backdrop to show behind the page: `None` while the focused one is still on its way (the old one
    /// stays), `Some(None)` for a title without one.
    pub fn backdrop(&self, g: &Gfx) -> Option<Option<String>> {
        let Some(t) = self.title() else { return Some(None) };
        let Some(p) = Self::path(self.pre, t, true) else {
            return Some(None);
        };
        match std::fs::metadata(g.rp(&p)) {
            Ok(m) if m.len() > 0 => Some(Some(p)),
            Ok(_) => Some(None),
            Err(_) => None,
        }
    }

    /// Frees the posters' textures while another tab is shown.
    pub fn sleep(&mut self) {
        self.cards.clear();
        self.hero = None;
        for l in &mut self.lines {
            l.head_tex = None;
        }
    }

    /// Moves the focus; true when Up leaves the first line (for the tabs). A row keeps its own position; within
    /// the grid, and from a row into it, the column on screen stays.
    pub fn key(&mut self, dx: i32, dy: i32) -> bool {
        if dy < 0 && self.cur == 0 {
            return true;
        }
        let Some(l) = self.lines.get(self.cur) else {
            return false;
        };
        if dx != 0 {
            let c = l.col as i32 + dx;
            if (0..l.idx.len() as i32).contains(&c) {
                self.lines[self.cur].col = c as usize;
            }
        }
        let to = self.cur as i32 + dy.signum();
        if dy != 0 && (0..self.lines.len() as i32).contains(&to) {
            let (from, to) = (self.cur, to as usize);
            if self.lines[to].grid {
                let on_screen = if self.lines[from].grid { self.lines[from].col } else { 0 };
                self.lines[to].col = on_screen.min(self.lines[to].idx.len() - 1);
            }
            self.cur = to;
        }
        false
    }

    /// Back: to the first title, if the focus is further on. False when it already is there.
    pub fn rewind(&mut self) -> bool {
        let moved = self.cur != 0 || self.lines.first().is_some_and(|l| l.col != 0);
        self.cur = 0;
        if let Some(l) = self.lines.first_mut() {
            l.col = 0;
        }
        moved
    }

    pub fn animate(&mut self, dt: f32, active: bool) -> bool {
        let top = self.lines.get(self.cur).map_or(0.0, |l| l.top);
        let mut anim = ease(&mut self.scroll, top, dt, 12.0);
        anim |= ease(&mut self.hero_a, 1.0, dt, 9.0);
        for l in self.lines.iter_mut().filter(|l| !l.grid) {
            anim |= ease(&mut l.scroll, l.col as f32 * (CARD.0 + GAP), dt, 12.0);
        }
        for ((r, c), it) in &mut self.cards {
            let on = active && *r == self.cur && self.lines[*r].col == *c;
            anim |= ease(&mut it.focus, f32::from(on), dt, 14.0);
        }
        anim
    }

    pub fn draw(&mut self, g: &Gfx) {
        let Some(t) = self.title() else {
            g.put(self.empty.as_ref(), MARGIN, ROW_Y, DIM, 1.0);
            return;
        };
        if self.hero.as_ref().is_none_or(|h| h.id != t.id) {
            let hero = Hero {
                id: t.id.clone(),
                title: g.text(g.f_hero, &clip(&t.name, 36), 0),
                meta: g.text(g.f_sub, &t.meta, 0).filter(|_| !t.meta.is_empty()),
                overview: g
                    .text(g.f_sub, &clip(&t.overview, 260), 1000)
                    .filter(|_| !t.overview.is_empty()),
            };
            self.hero_a = if self.hero.is_some() { 0.0 } else { 1.0 };
            self.hero = Some(hero);
        }
        if let Some(h) = &self.hero {
            let (dx, a) = ((1.0 - self.hero_a) * 32.0, self.hero_a);
            g.put(h.title.as_ref(), MARGIN + dx, 170.0, TEXT, a);
            let y = 170.0 + h.title.as_ref().map_or(0.0, |t| t.h as f32) + 4.0;
            g.put(h.meta.as_ref(), MARGIN + dx, y, TEXT, a);
            g.put(h.overview.as_ref(), MARGIN + dx, y + 52.0, DIM, a);
        }

        let shown = self.shown();
        self.cards
            .retain(|(r, c), _| shown.iter().any(|s| (s.0, s.1) == (*r, *c)));
        let mut focused = vec![];
        for r in self.cur.saturating_sub(1)..(self.cur + 3).min(self.lines.len()) {
            let l = &mut self.lines[r];
            let y = ROW_Y + l.top - self.scroll;
            let a = clamp(1.0 - (self.scroll - l.top) / (l.height() * 0.6), 0.0, 1.0);
            if a <= 0.0 || y > H {
                continue;
            }
            let mut cy = y;
            if let Some(head) = &l.head {
                if l.head_tex.is_none() {
                    l.head_tex = g.text(g.f_row, head, 0);
                }
                g.put(
                    l.head_tex.as_ref(),
                    MARGIN,
                    y,
                    if r == self.cur { TEXT } else { DIM },
                    a,
                );
                cy += LABEL_H;
            }
            for &(_, c, i) in shown.iter().filter(|s| s.0 == r) {
                let x = MARGIN + c as f32 * (CARD.0 + GAP) - if l.grid { 0.0 } else { l.scroll };
                if x > W || x + CARD.0 < 0.0 {
                    continue;
                }
                let t = &self.titles[i];
                let it = self.cards.entry((r, c)).or_insert_with(|| {
                    let mut it = Item::new(format!("jf:{}", t.id), String::new(), Kind::Watch, Some(t.name.clone()));
                    it.icons.extend(Self::path(self.pre, t, false));
                    it.cover = true;
                    it.progress = t.progress;
                    it.color = rgb(40, 46, 58);
                    it.label = g.text(g.f_label, &clip(&t.name, 40), CARD.0 as u32 - 24);
                    it
                });
                if it.focus > 0.01 {
                    focused.push(((r, c), x, cy, a));
                } else {
                    g.card(it, CARD, x, cy, a, 0.0);
                }
            }
        }
        // Drawn last, so the grown poster overlaps its neighbours.
        for (k, x, y, a) in focused {
            if let Some(it) = self.cards.get_mut(&k) {
                g.card(it, CARD, x, y, a, 0.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_from_jellyfin() {
        let movie = json!({ "Id": "m1", "Name": "A Film", "SortName": "Film, A", "Type": "Movie",
            "ProductionYear": 2019, "RunTimeTicks": 79_200_000_000i64, "CommunityRating": 7.84,
            "OfficialRating": "PG-13", "Genres": ["Drama", "Thriller", "Crime", "War"],
            "UserData": { "PlayedPercentage": 25.0, "Played": false },
            "ImageTags": { "Primary": "abcdef0123" }, "BackdropImageTags": ["b1"] });
        let t = title(&movie).unwrap();
        assert_eq!(
            t.meta,
            "2019   ·   2 h 12 min   ·   7.8/10   ·   PG-13   ·   Drama, Thriller, Crime"
        );
        assert_eq!((t.sort.as_str(), t.progress), ("film, a", 0.25));
        assert_eq!(
            Page::path(JELLYFIN, &t, false).unwrap(),
            format!("{DIR}/cache/lib-p-m1-cdef0123.img")
        );
        let show = json!({ "Id": "s1", "Name": "A Show", "Type": "Series", "ProductionYear": 2008,
            "EndDate": "2013-09-29T00:00:00Z", "Status": "Ended", "RunTimeTicks": 27_000_000_000i64,
            "UserData": { "Played": true, "PlayedPercentage": 100.0 } });
        let t = title(&show).unwrap();
        assert_eq!((t.meta.as_str(), t.progress, t.poster), ("2008–2013", 0.0, None));
        assert_eq!(t.key, None);
        let film = json!({ "Id": "1", "Name": "F", "Type": "Movie", "ProviderIds": { "Tmdb": "603" } });
        assert_eq!(title(&film).unwrap().key.as_deref(), Some("tmdbm603"));
        let show = json!({ "ratingKey": "9", "title": "S", "type": "show", "Guid": [{ "id": "tmdb://1668" }] });
        assert_eq!(plex_title(&show).unwrap().key.as_deref(), Some("tmdbtv1668"));
        let p = |key: Option<&str>| title_pic("plib", 'b', "9", key, "/art/123456789");
        assert_eq!(
            p(Some("tmdbtv1668")).unwrap(),
            format!("{DIR}/cache/t-b-tmdbtv1668.img")
        );
        assert_eq!(p(None).unwrap(), format!("{DIR}/cache/plib-b-9-23456789.img"));

        // A refresh that brings a new poster deletes the shared one, so it is fetched again; the backdrop stays.
        let root = std::env::temp_dir().join(format!("tvl-lib-{}", std::process::id()));
        let root = root.to_str().unwrap();
        std::fs::create_dir_all(format!("{root}{DIR}/cache")).unwrap();
        let mut f = title(
            &json!({ "Id": "1", "Name": "F", "Type": "Movie", "ProviderIds": { "Tmdb": "603" },
            "ImageTags": { "Primary": "old" }, "BackdropImageTags": ["bd"] }),
        )
        .unwrap();
        save_titles(root, JELLYFIN, "v", std::slice::from_ref(&f)).unwrap();
        for k in ['p', 'b'] {
            std::fs::write(format!("{root}{DIR}/cache/t-{k}-tmdbm603.img"), "x").unwrap();
        }
        f.poster = Some("new".into());
        save_titles(root, JELLYFIN, "v", &[f]).unwrap();
        let there = |k| std::path::Path::new(&format!("{root}{DIR}/cache/t-{k}-tmdbm603.img")).exists();
        assert_eq!((there('p'), there('b')), (false, true));
        let _ = std::fs::remove_dir_all(root);
        assert_eq!(types(Some("music")), None);
        assert_eq!(types(None), Some("Movie,Series"));
    }

    #[test]
    fn completed_refresh_is_separate_from_the_cached_library_lists() {
        let dir = std::env::temp_dir().join(format!("tvl-refresh-{}", std::process::id()));
        let root = dir.to_str().unwrap();
        save_views(root, JELLYFIN, &[]).unwrap();
        assert!(last_refresh(root, JELLYFIN).is_none());
        mark_refreshed(root, JELLYFIN).unwrap();
        prune(root, JELLYFIN, &HashSet::new());
        assert!(last_refresh(root, JELLYFIN).is_some());
        assert!(last_refresh(root, PLEX).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plex_detail_keys_are_scoped_to_the_server() {
        assert_ne!(plex_cache_id("first", "7"), plex_cache_id("second", "7"));
        assert_eq!(plex_cache_id("../server", "7"), "server_7");
    }

    fn t(i: usize, genres: &[&str], played: bool) -> Title {
        Title {
            id: i.to_string(),
            name: String::new(),
            sort: format!("{i:03}"),
            added: format!("2025-{:03}", 100 - i),
            meta: String::new(),
            overview: String::new(),
            progress: if i == 5 { 0.5 } else { 0.0 },
            played,
            rating: i as f32,
            genres: genres.iter().map(|g| g.to_string()).collect(),
            poster: None,
            backdrop: None,
            key: None,
        }
    }

    fn page(titles: Vec<Title>) -> Page {
        Page {
            pre: JELLYFIN,
            src: Source::None,
            lines: lines(&titles, true),
            titles,
            cur: 0,
            scroll: 0.0,
            cards: HashMap::new(),
            hero: None,
            hero_a: 0.0,
            empty: None,
        }
    }

    #[test]
    fn titles_from_plex() {
        let movie = json!({ "ratingKey": "42", "type": "movie", "title": "A Film", "titleSort": "Film, A",
            "year": 2019, "duration": 7_920_000, "viewOffset": 1_980_000, "audienceRating": 7.84,
            "contentRating": "PG-13", "addedAt": 1_700_000_000u64, "summary": "About it.",
            "Genre": [{ "tag": "Drama" }, { "tag": "War" }],
            "thumb": "/library/metadata/42/thumb/1712345678", "art": "/library/metadata/42/art/1712345678" });
        let t = plex_title(&movie).unwrap();
        assert_eq!(
            t.meta,
            "2019   ·   2 h 12 min   ·   7.8/10   ·   PG-13   ·   Drama, War"
        );
        assert_eq!(
            (t.id.as_str(), t.sort.as_str(), t.progress, t.played),
            ("42", "film, a", 0.25, false)
        );
        assert_eq!(t.added, "001700000000");
        assert_eq!(
            Page::path(PLEX, &t, true).unwrap(),
            format!("{DIR}/cache/plib-b-42-12345678.img")
        );
        let src = Source::Plex {
            base: "http://10.0.0.2:32400".into(),
            token: "tok".into(),
        };
        assert_eq!(
            src.url(&t.id, false, t.poster.as_deref().unwrap()).unwrap(),
            "http://10.0.0.2:32400/photo/:/transcode?width=300&height=450&minSize=1&upscale=1\
             &url=%2Flibrary%2Fmetadata%2F42%2Fthumb%2F1712345678&X-Plex-Token=tok"
        );
        assert_eq!(Source::None.url("42", false, "x"), None);
        let show = json!({ "ratingKey": 7, "type": "show", "title": "A Show", "year": 2008,
            "leafCount": 10, "viewedLeafCount": 10, "duration": 1_800_000 });
        let t = plex_title(&show).unwrap();
        assert_eq!(
            (t.id.as_str(), t.meta.as_str(), t.played, t.poster),
            ("7", "2008", true, None)
        );
    }

    #[test]
    fn rows_and_grid() {
        let titles = (0..30)
            .map(|i| t(i, if i < 10 { &["Drama"] } else { &["Comedy"] }, i % 3 == 0))
            .collect();
        let mut p = page(titles);
        let heads: Vec<_> = p.lines.iter().filter_map(|l| l.head.as_deref()).collect();
        assert_eq!(
            heads,
            [
                "Continue watching",
                "Recently added",
                "Unwatched",
                "Comedy",
                "Drama",
                "All, A to Z"
            ]
        );
        assert_eq!(p.lines[0].idx, [5]);
        assert_eq!(p.lines[1].idx[..3], [0, 1, 2], "newest first");
        assert_eq!(p.lines[2].idx[..2], [29, 28], "best rated first, watched ones left out");
        assert_eq!(p.lines.len(), 5 + 5, "30 titles make 5 grid lines");

        assert!(p.key(0, -1), "up from the first row goes to the tabs");
        p.key(0, 1);
        p.key(1, 0);
        p.key(1, 0);
        assert_eq!((p.cur, p.lines[1].col), (1, 2));
        p.key(0, 1);
        p.key(0, -1);
        assert_eq!(p.lines[1].col, 2, "a row keeps its place");
        p.cur = 4;
        p.lines[4].col = 7;
        p.key(0, 1);
        assert_eq!((p.cur, p.lines[5].col), (5, 0), "into the grid at its first column");
        p.key(1, 0);
        p.key(0, 1);
        assert_eq!(p.lines[6].col, 1, "the grid keeps the column");
        p.cur = 9;
        p.key(0, 1);
        assert_eq!(p.cur, 9, "no line below");
        assert_eq!(p.title().unwrap().id, "24");
        assert!(p.rewind() && p.cur == 0 && !p.rewind());
    }

    #[test]
    fn small_libraries_are_only_a_grid() {
        let p = page((0..8).map(|i| t(i, &["Drama"], false)).collect());
        assert_eq!(p.lines.len(), 2);
        assert!(p.lines.iter().all(|l| l.grid));
        assert_eq!(p.lines[1].idx, [6, 7]);
    }
}
